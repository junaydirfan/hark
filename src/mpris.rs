//! MPRIS (D-Bus) on Linux: media keys, the KDE/GNOME media widget, the lock screen, `playerctl`
//! and KDE Connect. On macOS the same through Now Playing: media keys, Control Center, the lock
//! screen. We publish a snapshot after each change and only touch D-Bus for fields that
//! changed, so this costs nothing while idle. Podcast speed is MPRIS's playback rate.

use tokio::sync::mpsc::UnboundedSender;

use crate::app::App;

/// Commands from the desktop (widget buttons, media keys, playerctl…).
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))] // only the desktop sends them
pub enum Remote {
    PlayPause,
    Play,
    Pause,
    /// "Next": the next episode in the queue.
    Next,
    /// "Previous": back to the start of the episode.
    Prev,
    /// Relative seek in seconds.
    Seek(f64),
    /// Absolute position in seconds.
    SetPosition(f64),
    /// 0..=100
    Volume(u8),
    /// Playback speed.
    Rate(f32),
}

#[cfg(target_os = "linux")]
pub use dbus::Mpris;
#[cfg(target_os = "macos")]
pub use mac::{Mpris, prepare, run_main};

// shortcut: Windows has no media-key integration yet; hark runs without it.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub struct Mpris;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Mpris {
    pub async fn new(_tx: UnboundedSender<Remote>) -> Option<Self> {
        None
    }

    pub async fn sync(&mut self, _app: &App) {}
}

#[cfg(target_os = "linux")]
mod dbus {
    use std::{rc::Rc, time::Instant};

    use mpris_server::{Metadata, PlaybackStatus, Player, Time, TrackId};

    use super::{App, Remote, UnboundedSender};

    #[derive(Clone, Default, PartialEq)]
    struct Track {
        guid: String,
        title: String,
        show: String,
        len_s: f64,
        art: Option<String>,
    }

    pub struct Mpris {
        player: Rc<Player>,
        track: Track,
        status: Option<PlaybackStatus>,
        vol: Option<u8>,
        rate: Option<f32>,
        pos: f64,
        pos_at: Instant,
    }

    impl Mpris {
        /// Registers `org.mpris.MediaPlayer2.hark`. None (and hark carries on without desktop
        /// integration) when there's no session bus.
        pub async fn new(tx: UnboundedSender<Remote>) -> Option<Self> {
            let player = Player::builder("hark")
                .identity("hark")
                .desktop_entry("hark")
                .can_play(true)
                .can_pause(true)
                .can_go_next(true)
                .can_go_previous(true)
                .can_seek(true)
                .can_control(true)
                .minimum_rate(0.5)
                .maximum_rate(3.0)
                .build()
                .await
                .ok()?;
            macro_rules! on {
                ($connect:ident, $f:expr) => {{
                    let tx = tx.clone();
                    let f = $f;
                    player.$connect(move |_| {
                        let _ = tx.send(f());
                    });
                }};
            }
            on!(connect_play_pause, || Remote::PlayPause);
            on!(connect_play, || Remote::Play);
            on!(connect_pause, || Remote::Pause);
            on!(connect_stop, || Remote::Pause);
            on!(connect_next, || Remote::Next);
            on!(connect_previous, || Remote::Prev);
            {
                let tx = tx.clone();
                player.connect_seek(move |_, off| {
                    let _ = tx.send(Remote::Seek(off.as_millis() as f64 / 1000.0));
                });
            }
            {
                let tx = tx.clone();
                player.connect_set_position(move |_, _id, pos| {
                    let _ = tx.send(Remote::SetPosition(pos.as_millis().max(0) as f64 / 1000.0));
                });
            }
            {
                let tx = tx.clone();
                player.connect_set_volume(move |_, v| {
                    let _ = tx.send(Remote::Volume((v.clamp(0.0, 1.0) * 100.0).round() as u8));
                });
            }
            player.connect_set_rate(move |_, r| {
                let _ = tx.send(Remote::Rate(r as f32));
            });
            let player = Rc::new(player);
            tokio::task::spawn_local(player.run());
            Some(Mpris { player, track: Track::default(), status: None, vol: None, rate: None, pos: 0.0, pos_at: Instant::now() })
        }

        /// Push whatever changed since the last call.
        pub async fn sync(&mut self, app: &App) {
            let p = &self.player;
            let track = app
                .now
                .as_ref()
                .map(|n| Track { guid: n.guid.clone(), title: n.title.clone(), show: n.show.clone(), len_s: n.duration, art: n.art.clone() })
                .unwrap_or_default();
            let new_track = track.guid != self.track.guid;
            if track != self.track {
                let mut m = Metadata::new();
                let id = format!("/org/hark/episode/{:016x}", crate::paths::fnv(&track.guid));
                m.set_trackid(Some(if track.guid.is_empty() { TrackId::NO_TRACK } else { TrackId::try_from(id).unwrap_or(TrackId::NO_TRACK) }));
                if !track.guid.is_empty() {
                    m.set_title(Some(track.title.clone()));
                    m.set_artist(Some(vec![track.show.clone()]));
                    m.set_album(Some(track.show.clone()));
                    m.set_length(Some(Time::from_millis((track.len_s * 1000.0) as i64)));
                    m.set_art_url(track.art.clone());
                }
                let _ = p.set_metadata(m).await;
                self.track = track;
            }
            let status = match &app.now {
                None => PlaybackStatus::Stopped,
                Some(n) if n.playing => PlaybackStatus::Playing,
                Some(_) => PlaybackStatus::Paused,
            };
            if self.status != Some(status) {
                let _ = p.set_playback_status(status).await;
                self.status = Some(status);
            }
            if self.vol != Some(app.prefs.volume) {
                let _ = p.set_volume(app.prefs.volume as f64 / 100.0).await;
                self.vol = Some(app.prefs.volume);
            }
            if self.rate != Some(app.prefs.speed) {
                let _ = p.set_rate(app.prefs.speed as f64).await;
                self.rate = Some(app.prefs.speed);
            }
            // Clients read Position on demand and extrapolate while Playing (at Rate); tell them
            // about jumps with the Seeked signal.
            let pos = app.now.as_ref().map_or(0.0, |n| n.pos);
            p.set_position(Time::from_millis((pos * 1000.0) as i64));
            let expected = if status == PlaybackStatus::Playing { self.pos + self.pos_at.elapsed().as_secs_f64() * app.prefs.speed as f64 } else { self.pos };
            if !new_track && (pos - expected).abs() > 1.5 {
                let _ = p.seeked(Time::from_millis((pos * 1000.0) as i64)).await;
            }
            self.pos = pos;
            self.pos_at = Instant::now();
        }
    }
}

/// macOS Now Playing: media keys, Control Center and the lock screen. macOS only hands
/// media keys to an NSApplication whose main thread runs AppKit's event loop, which is
/// where it delivers them; so on macOS the player itself runs on a second thread ([`prepare`]).
/// Ported from trill.
#[cfg(target_os = "macos")]
mod mac {
    use std::{
        cell::{Cell, RefCell},
        ffi::c_void,
        ptr::NonNull,
        sync::{
            OnceLock,
            atomic::{AtomicBool, Ordering},
        },
        time::Instant,
    };

    use block2::{DynBlock, RcBlock};
    use objc2::{
        msg_send,
        rc::{Allocated, Retained, autoreleasepool},
        runtime::{AnyClass, AnyObject},
    };
    use objc2_foundation::{NSData, NSMutableDictionary, NSNumber, NSSize, NSString};
    use objc2_media_player::{
        MPChangePlaybackPositionCommandEvent, MPMediaItemPropertyAlbumTitle, MPMediaItemPropertyArtist, MPMediaItemPropertyArtwork,
        MPMediaItemPropertyPlaybackDuration, MPMediaItemPropertyTitle, MPNowPlayingInfoCenter,
        MPNowPlayingInfoPropertyElapsedPlaybackTime, MPNowPlayingInfoPropertyPlaybackRate, MPNowPlayingPlaybackState,
        MPRemoteCommand, MPRemoteCommandCenter, MPRemoteCommandEvent, MPRemoteCommandHandlerStatus,
    };

    use super::{App, Remote, UnboundedSender};

    // NSApplication is in AppKit, which nothing else links.
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopGetMain() -> *mut c_void;
        fn CFRunLoopPerformBlock(rl: *mut c_void, mode: *const c_void, block: &DynBlock<dyn Fn()>);
        fn CFRunLoopWakeUp(rl: *mut c_void);
        static kCFRunLoopCommonModes: *const c_void;
    }

    static ON: AtomicBool = AtomicBool::new(false);
    static TX: OnceLock<UnboundedSender<Remote>> = OnceLock::new();

    fn ns_app() -> Option<*mut AnyObject> {
        let cls = AnyClass::get(c"NSApplication")?;
        // SAFETY: plain AppKit call.
        Some(unsafe { msg_send![cls, sharedApplication] })
    }

    /// On the main thread, before other threads start: make hark a background NSApplication
    /// and register for the media keys. True when the main thread must then go to
    /// [`run_main`] and the player run elsewhere. Skipped over SSH, where there's no desktop.
    pub fn prepare() -> bool {
        if std::env::var_os("SSH_CONNECTION").is_some() {
            return false;
        }
        let Some(app) = ns_app() else { return false };
        // SAFETY: main thread; plain AppKit / MediaPlayer calls.
        unsafe {
            // background-only: no Dock icon, no menu bar (NSApplicationActivationPolicyProhibited)
            let _: bool = msg_send![app, setActivationPolicy: 2isize];
            autoreleasepool(|_| register());
        }
        ON.store(true, Ordering::Relaxed);
        true
    }

    /// The main thread's job from here on: AppKit's event loop. Never returns.
    pub fn run_main() -> ! {
        if let Some(app) = ns_app() {
            // SAFETY: main thread.
            let _: () = unsafe { msg_send![app, run] };
        }
        loop {
            std::thread::park();
        }
    }

    unsafe fn register() {
        let cc = unsafe { MPRemoteCommandCenter::sharedCommandCenter() };
        let on = |cmd: &MPRemoteCommand, f: fn(&MPRemoteCommandEvent) -> Remote| {
            let block = RcBlock::new(move |e: NonNull<MPRemoteCommandEvent>| {
                let Some(tx) = TX.get() else { return MPRemoteCommandHandlerStatus::CommandFailed };
                // SAFETY: MediaPlayer hands us a live event.
                let _ = tx.send(f(unsafe { e.as_ref() }));
                MPRemoteCommandHandlerStatus::Success
            });
            unsafe {
                cmd.setEnabled(true);
                cmd.addTargetWithHandler(&block);
            }
        };
        unsafe {
            on(&cc.togglePlayPauseCommand(), |_| Remote::PlayPause);
            on(&cc.playCommand(), |_| Remote::Play);
            on(&cc.pauseCommand(), |_| Remote::Pause);
            on(&cc.stopCommand(), |_| Remote::Pause);
            on(&cc.nextTrackCommand(), |_| Remote::Next);
            on(&cc.previousTrackCommand(), |_| Remote::Prev);
            on(&cc.changePlaybackPositionCommand(), |e| {
                let e = &*(e as *const MPRemoteCommandEvent as *const MPChangePlaybackPositionCommandEvent);
                Remote::SetPosition(e.positionTime().max(0.0))
            });
        }
    }

    /// What Now Playing shows, copied out of the app so the main thread can publish it.
    struct Info {
        title: String,
        show: String,
        art: Option<String>,
        len_s: f64,
        pos_s: f64,
        /// when `pos_s` was read
        at: Instant,
        playing: bool,
        speed: f64,
    }

    thread_local! {
        /// Main thread: the cover (with its url) and the info last published, so a cover
        /// that arrives later can be added to it.
        static SHOWN: RefCell<(Option<(String, Retained<AnyObject>)>, Option<Info>)> = const { RefCell::new((None, None)) };
    }

    /// Run `f` once on the main thread, which MediaPlayer belongs to.
    fn on_main(f: impl FnOnce() + 'static) {
        let f = Cell::new(Some(f));
        let block = RcBlock::new(move || {
            if let Some(f) = f.take() {
                autoreleasepool(|_| f());
            }
        });
        // SAFETY: CoreFoundation copies the block and runs it on the main run loop.
        unsafe {
            let main = CFRunLoopGetMain();
            CFRunLoopPerformBlock(main, kCFRunLoopCommonModes, &block);
            CFRunLoopWakeUp(main);
        }
    }

    pub struct Mpris {
        guid: String,
        art: String,
        playing: bool,
        speed: f32,
        pos: f64,
        pos_at: Instant,
    }

    impl Mpris {
        pub async fn new(tx: UnboundedSender<Remote>) -> Option<Self> {
            if !ON.load(Ordering::Relaxed) {
                return None;
            }
            let _ = TX.set(tx);
            Some(Mpris { guid: String::new(), art: String::new(), playing: false, speed: 1.0, pos: 0.0, pos_at: Instant::now() })
        }

        /// Publish when the episode, play/pause, speed or position (a seek) changed; macOS
        /// moves the position along by itself in between.
        pub async fn sync(&mut self, app: &App) {
            if let Some(url) = app.now.as_ref().and_then(|n| n.art.clone())
                && url != self.art
            {
                self.art = url.clone();
                let client = app.client.clone();
                tokio::spawn(async move {
                    if let Ok(bytes) = async { client.get(&url).send().await?.error_for_status()?.bytes().await }.await {
                        on_main(move || set_art(url, &bytes));
                    }
                });
            }
            let guid = app.now.as_ref().map_or("", |n| n.guid.as_str());
            let playing = app.now.as_ref().is_some_and(|n| n.playing);
            let pos = app.now.as_ref().map_or(0.0, |n| n.pos);
            let speed = app.prefs.speed;
            let expected = self.pos + if self.playing { self.pos_at.elapsed().as_secs_f64() * self.speed as f64 } else { 0.0 };
            if guid == self.guid && playing == self.playing && speed == self.speed && (pos - expected).abs() < 1.5 {
                return;
            }
            (self.playing, self.speed, self.pos, self.pos_at) = (playing, speed, pos, Instant::now());
            self.guid = guid.to_string();
            let info = app.now.as_ref().map(|n| Info {
                title: n.title.clone(),
                show: n.show.clone(),
                art: n.art.clone(),
                len_s: n.duration,
                pos_s: pos,
                at: Instant::now(),
                playing,
                speed: speed as f64,
            });
            on_main(move || {
                SHOWN.with_borrow_mut(|(art, last)| {
                    *last = info;
                    // SAFETY: main thread.
                    unsafe { publish(art.as_ref(), last.as_ref()) }
                })
            });
        }
    }

    /// Main thread: a cover arrived; add it if its episode is still the one shown.
    fn set_art(url: String, bytes: &[u8]) {
        // SAFETY: main thread; plain AppKit / MediaPlayer calls.
        let Some(a) = (unsafe { artwork(bytes) }) else { return };
        SHOWN.with_borrow_mut(|(art, last)| {
            *art = Some((url, a));
            if last.as_ref().and_then(|i| i.art.as_ref()) == art.as_ref().map(|a| &a.0) {
                unsafe { publish(art.as_ref(), last.as_ref()) }
            }
        });
    }

    /// An `MPMediaItemArtwork` for the image bytes (NSImage reads jpeg/png itself).
    unsafe fn artwork(bytes: &[u8]) -> Option<Retained<AnyObject>> {
        let data = NSData::with_bytes(bytes);
        unsafe {
            let a: Allocated<AnyObject> = msg_send![AnyClass::get(c"NSImage")?, alloc];
            let img: Option<Retained<AnyObject>> = msg_send![a, initWithData: &*data];
            let img = img?;
            let size: NSSize = msg_send![&*img, size];
            let handler = RcBlock::new(move |_: NSSize| Retained::as_ptr(&img) as *mut AnyObject);
            let a: Allocated<AnyObject> = msg_send![AnyClass::get(c"MPMediaItemArtwork")?, alloc];
            msg_send![a, initWithBoundsSize: size, requestHandler: &*handler]
        }
    }

    unsafe fn publish(art: Option<&(String, Retained<AnyObject>)>, info: Option<&Info>) {
        let center = unsafe { MPNowPlayingInfoCenter::defaultCenter() };
        let Some(i) = info else {
            unsafe {
                center.setNowPlayingInfo(None);
                center.setPlaybackState(MPNowPlayingPlaybackState::Stopped);
            }
            return;
        };
        let dict = NSMutableDictionary::<NSString, AnyObject>::new();
        let text = |k: &NSString, v: &str| dict.insert(k, NSString::from_str(v).as_ref());
        let num = |k: &NSString, v: f64| dict.insert(k, NSNumber::new_f64(v).as_ref());
        // a re-publish (cover arrived) mustn't move the position back
        let pos = i.pos_s + if i.playing { i.at.elapsed().as_secs_f64() * i.speed } else { 0.0 };
        unsafe {
            text(MPMediaItemPropertyTitle, &i.title);
            text(MPMediaItemPropertyArtist, &i.show);
            text(MPMediaItemPropertyAlbumTitle, &i.show);
            num(MPMediaItemPropertyPlaybackDuration, i.len_s);
            num(MPNowPlayingInfoPropertyElapsedPlaybackTime, pos);
            // macOS moves the position along at this rate: the podcast speed while playing
            num(MPNowPlayingInfoPropertyPlaybackRate, if i.playing { i.speed } else { 0.0 });
            if let Some((url, a)) = art
                && i.art.as_ref() == Some(url)
            {
                dict.insert(MPMediaItemPropertyArtwork, a);
            }
            center.setNowPlayingInfo(Some(&dict));
            center.setPlaybackState(if i.playing { MPNowPlayingPlaybackState::Playing } else { MPNowPlayingPlaybackState::Paused });
        }
    }
}
