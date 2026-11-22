//! In-process audio: symphonia decodes, Sonic changes the speed without changing the pitch, cpal
//! plays. One thread owns all of it and sleeps until the output runs low or a command arrives;
//! paused or idle, it waits for a command with no timer at all, so it costs nothing.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use libsonic::SonicStream;
use symphonia::core::{
    codecs::audio::{AudioDecoder, AudioDecoderOptions},
    errors::Error as SymErr,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
    units::{Time, TimeBase},
};
use tokio::sync::mpsc::UnboundedSender;

pub enum Cmd {
    /// Play a file from `at_s` seconds in.
    Load { path: PathBuf, at_s: f64 },
    Pause(bool),
    Seek(f64),
    Speed(f32),
    /// 0.0 ..= 1.0
    Volume(f32),
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Loaded { duration_s: f64 },
    /// Where playback is, in the episode's own seconds; sent when the whole second changes.
    Position(f64),
    Ended,
    Error(String),
}

/// Half a second of audio is decoded ahead; the output asks for more when it drops below a quarter.
const AHEAD_S: f64 = 0.5;

struct Shared {
    ring: Mutex<VecDeque<f32>>,
    inbox: Mutex<VecDeque<Cmd>>,
    wake: Condvar,
}

pub struct Engine {
    shared: Arc<Shared>,
}

impl Engine {
    pub fn start(events: UnboundedSender<Event>) -> Engine {
        let shared = Arc::new(Shared { ring: Mutex::new(VecDeque::new()), inbox: Mutex::new(VecDeque::new()), wake: Condvar::new() });
        let s = shared.clone();
        std::thread::Builder::new().name("audio".into()).spawn(move || Player::new(s, events).run()).expect("audio thread");
        Engine { shared }
    }

    pub fn send(&self, c: Cmd) {
        self.shared.inbox.lock().unwrap().push_back(c);
        self.shared.wake.notify_one();
    }
}

struct Track {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    id: u32,
    tb: Option<TimeBase>,
    /// Source seconds at the end of the last decoded packet.
    decoded_s: f64,
    eof: bool,
}

struct Player {
    shared: Arc<Shared>,
    events: UnboundedSender<Event>,
    /// The output stream and the (rate, channels) it was opened for.
    out: Option<(cpal::Stream, u32, u16)>,
    track: Option<Track>,
    sonic: Option<SonicStream>,
    paused: bool,
    speed: f32,
    volume: f32,
    last_sec: i64,
    buf: Vec<f32>,
}

impl Player {
    fn new(shared: Arc<Shared>, events: UnboundedSender<Event>) -> Self {
        Player { shared, events, out: None, track: None, sonic: None, paused: false, speed: 1.0, volume: 1.0, last_sec: -1, buf: Vec::new() }
    }

    fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    fn run(mut self) {
        loop {
            let busy = self.track.is_some() && !self.paused;
            let cmds: Vec<Cmd> = {
                let mut q = self.shared.inbox.lock().unwrap();
                if q.is_empty() {
                    q = if busy {
                        // the output callback wakes us when it runs low; the timeout only covers a missed wake-up
                        self.shared.wake.wait_timeout(q, Duration::from_millis(250)).unwrap().0
                    } else {
                        self.shared.wake.wait(q).unwrap()
                    };
                }
                q.drain(..).collect()
            };
            for c in cmds {
                self.handle(c);
            }
            if self.track.is_some() && !self.paused {
                self.fill();
            }
        }
    }

    fn handle(&mut self, c: Cmd) {
        match c {
            Cmd::Load { path, at_s } => {
                if let Err(e) = self.load(path, at_s) {
                    self.track = None;
                    self.emit(Event::Error(e));
                }
            }
            Cmd::Pause(p) => {
                self.paused = p;
                if let Some((s, ..)) = &self.out {
                    let _ = if p { s.pause() } else { s.play() };
                }
            }
            Cmd::Seek(s) => self.seek(s),
            Cmd::Speed(x) => {
                self.speed = x.clamp(0.5, 3.0);
                if let Some(s) = &mut self.sonic {
                    s.set_speed(self.speed);
                }
            }
            Cmd::Volume(v) => {
                self.volume = v.clamp(0.0, 1.0);
                if let Some(s) = &mut self.sonic {
                    s.set_volume(self.volume);
                }
            }
        }
    }

    fn load(&mut self, path: PathBuf, at_s: f64) -> Result<(), String> {
        let file = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format = symphonia::default::get_probe().probe(&hint, mss, FormatOptions::default(), MetadataOptions::default()).map_err(|e| format!("can't read this audio: {e}"))?;
        let track = format.default_track(TrackType::Audio).ok_or("no audio in this file")?;
        let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or("no audio in this file")?.clone();
        let (id, tb, frames) = (track.id, track.time_base, track.num_frames);
        let rate = params.sample_rate.ok_or("unknown sample rate")?;
        let channels = params.channels.as_ref().map_or(2, |c| c.count() as u16);
        let decoder = symphonia::default::get_codecs().make_audio_decoder(&params, &AudioDecoderOptions::default()).map_err(|e| format!("unsupported audio: {e}"))?;
        let duration_s = match (tb, frames) {
            (Some(tb), Some(n)) => tb.calc_time_saturating(symphonia::core::units::Timestamp::new(n as i64)).as_secs_f64(),
            _ => 0.0,
        };
        if self.out.as_ref().is_none_or(|(_, r, c)| (*r, *c) != (rate, channels)) {
            self.out = None; // close the old stream before opening one for the new format
            self.out = Some((self.open_output(rate, channels)?, rate, channels));
        }
        self.track = Some(Track { format, decoder, id, tb, decoded_s: 0.0, eof: false });
        self.emit(Event::Loaded { duration_s });
        self.paused = false;
        self.seek(at_s);
        if let Some((s, ..)) = &self.out {
            s.play().map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    // shortcut: plays at the file's own rate and f32 samples. PulseAudio (Linux, also on PipeWire)
    // converts as needed; CoreAudio/WASAPI may refuse other rates. Add a resampler for macOS/Windows.
    fn open_output(&self, rate: u32, channels: u16) -> Result<cpal::Stream, String> {
        #[cfg(target_os = "linux")]
        let host = cpal::host_from_id(cpal::HostId::PulseAudio).unwrap_or_else(|_| cpal::default_host());
        #[cfg(not(target_os = "linux"))]
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no audio output device")?;
        let config = cpal::StreamConfig { channels, sample_rate: rate, buffer_size: cpal::BufferSize::Default };
        let shared = self.shared.clone();
        let low = (rate as f64 * channels as f64 * AHEAD_S / 2.0) as usize;
        let events = self.events.clone();
        device
            .build_output_stream(
                config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let mut r = shared.ring.lock().unwrap();
                    let n = data.len().min(r.len());
                    for (d, s) in data.iter_mut().zip(r.drain(..n)) {
                        *d = s;
                    }
                    data[n..].fill(0.0);
                    if r.len() < low {
                        shared.wake.notify_one();
                    }
                },
                move |e| {
                    let _ = events.send(Event::Error(format!("audio output: {e}")));
                },
                None,
            )
            .map_err(|e| format!("can't open audio output: {e}"))
    }

    fn new_sonic(&mut self) {
        let Some((_, rate, ch)) = &self.out else { return };
        self.sonic = SonicStream::new(*rate, *ch as u32).ok().map(|mut s| {
            s.set_speed(self.speed);
            s.set_volume(self.volume);
            s
        });
    }

    fn seek(&mut self, to_s: f64) {
        let Some(t) = &mut self.track else { return };
        let to_s = to_s.max(0.0);
        if to_s > 0.0 {
            let time = Time::try_from_secs_f64(to_s).unwrap_or_default();
            match t.format.seek(SeekMode::Coarse, SeekTo::Time { time, track_id: Some(t.id) }) {
                Ok(done) => t.decoded_s = t.tb.map_or(to_s, |tb| tb.calc_time_saturating(done.actual_ts).as_secs_f64()),
                Err(e) => return self.emit(Event::Error(format!("seek failed: {e}"))),
            }
        } else if t.decoded_s > 0.0 {
            // back to the very start: rewind the reader the same way
            let _ = t.format.seek(SeekMode::Accurate, SeekTo::Time { time: Time::default(), track_id: Some(t.id) });
            t.decoded_s = 0.0;
        }
        t.decoder.reset();
        t.eof = false;
        self.shared.ring.lock().unwrap().clear();
        self.new_sonic();
        self.last_sec = -1;
        self.report();
    }

    /// Where the listener is: what has been decoded, minus what still waits in the output buffer.
    fn position(&self) -> f64 {
        let (Some(t), Some((_, rate, ch))) = (&self.track, &self.out) else { return 0.0 };
        let queued_frames = self.shared.ring.lock().unwrap().len() as f64 / *ch as f64;
        (t.decoded_s - queued_frames / *rate as f64 * self.speed as f64).max(0.0)
    }

    fn report(&mut self) {
        let p = self.position();
        if p as i64 != self.last_sec {
            self.last_sec = p as i64;
            self.emit(Event::Position(p));
        }
    }

    /// Decode until half a second is queued (or the file ends).
    fn fill(&mut self) {
        let Some((_, rate, ch)) = &self.out else { return };
        let cap = (*rate as f64 * *ch as f64 * AHEAD_S) as usize;
        loop {
            if self.shared.ring.lock().unwrap().len() >= cap {
                break;
            }
            let Some(t) = &mut self.track else { return };
            if t.eof {
                if self.shared.ring.lock().unwrap().is_empty() {
                    self.track = None;
                    self.emit(Event::Ended);
                    return;
                }
                break;
            }
            let packet = match t.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    t.eof = true;
                    if let Some(s) = &mut self.sonic {
                        s.flush();
                        let n = s.available_frames();
                        self.shared.ring.lock().unwrap().extend(s.read_f32(n));
                    }
                    continue;
                }
                Err(SymErr::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    t.eof = true;
                    continue;
                }
                Err(e) => {
                    self.track = None;
                    return self.emit(Event::Error(format!("reading audio: {e}")));
                }
            };
            if packet.track_id != t.id {
                continue;
            }
            if let Some(tb) = t.tb {
                t.decoded_s = tb.calc_time_saturating(packet.pts).as_secs_f64() + tb.calc_time_saturating(symphonia::core::units::Timestamp::new(packet.dur.get() as i64)).as_secs_f64();
            }
            match t.decoder.decode(&packet) {
                Ok(audio) => {
                    self.buf.resize(audio.samples_interleaved(), 0.0);
                    audio.copy_to_slice_interleaved(&mut self.buf);
                    if let Some(s) = &mut self.sonic {
                        s.write_f32(&self.buf);
                        let n = s.available_frames();
                        self.shared.ring.lock().unwrap().extend(s.read_f32(n));
                    }
                }
                Err(SymErr::DecodeError(_)) => {} // a damaged frame: skip it, like every player does
                Err(e) => {
                    self.track = None;
                    return self.emit(Event::Error(format!("decoding audio: {e}")));
                }
            }
        }
        self.report();
    }
}
