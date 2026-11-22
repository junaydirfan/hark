//! MPRIS (D-Bus) on Linux: media keys, the KDE/GNOME media widget, the lock screen, `playerctl`
//! and KDE Connect. We publish a snapshot after each change and only touch D-Bus for fields that
//! changed, so this costs nothing while idle. Podcast speed is MPRIS's playback rate.

use tokio::sync::mpsc::UnboundedSender;

use crate::app::App;

/// Commands from the desktop (widget buttons, media keys, playerctl…).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // only MPRIS sends them
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

// shortcut: macOS Now Playing needs AppKit's event loop on the main thread (trill has it);
// Windows has no integration yet. Both run without it.
#[cfg(not(target_os = "linux"))]
pub struct Mpris;

#[cfg(not(target_os = "linux"))]
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
