//! Play while downloading. A task on the async thread writes the episode into a cache file;
//! the decoder reads that file through [`NetReader`]. Seeking past what has arrived restarts the
//! download there with an HTTP range request; what's already on disk stays usable.

use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use symphonia::core::io::MediaSource;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Decode only with this much ready ahead of the reader (~4 s at 128 kbps), so a slow network
/// shows as "buffering" instead of blocking the audio thread.
pub const READY: u64 = 64 * 1024;
/// A seek this close ahead of the running download waits for it instead of starting another.
const NEAR: u64 = 512 * 1024;
/// How long a read may wait for data before giving up.
const STALL: Duration = Duration::from_secs(20);
const CLOSE: u64 = u64::MAX;

#[derive(Default)]
struct State {
    /// Byte ranges on disk, sorted and merged.
    spans: Vec<(u64, u64)>,
    /// Where the running download writes next (None when no download runs).
    cursor: Option<u64>,
    total: Option<u64>,
    failed: Option<String>,
    /// A restart already asked for, so a waiting reader doesn't ask again.
    asked: Option<u64>,
}

impl State {
    fn add(&mut self, a: u64, b: u64) {
        self.spans.push((a, b));
        self.spans.sort_unstable();
        let mut out: Vec<(u64, u64)> = Vec::with_capacity(self.spans.len());
        for &(s, e) in &self.spans {
            match out.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => out.push((s, e)),
            }
        }
        self.spans = out;
    }

    /// Bytes on disk from `pos` onwards without a gap.
    fn ahead(&self, pos: u64) -> u64 {
        self.spans.iter().find(|(s, e)| *s <= pos && pos < *e).map_or(0, |(_, e)| e - pos)
    }

    fn at_end(&self, pos: u64) -> bool {
        self.total.is_some_and(|t| pos >= t)
    }
}

/// Shared between the download task, the reader and the engine.
pub struct Src {
    st: Mutex<State>,
    cv: Condvar,
    /// Where the decoder is reading (kept by [`NetReader`]).
    read_pos: AtomicU64,
    restart: UnboundedSender<u64>,
    /// Wakes the audio thread when data arrives.
    wake: Box<dyn Fn() + Send + Sync>,
}

impl Src {
    /// Enough is on disk to decode the next packet without waiting (or the file is complete there).
    pub fn ready(&self) -> bool {
        let st = self.st.lock().unwrap();
        let pos = self.read_pos.load(Ordering::Relaxed);
        st.ahead(pos) >= READY || st.at_end(pos + st.ahead(pos)) || st.failed.is_some()
    }

    pub fn failed(&self) -> Option<String> {
        self.st.lock().unwrap().failed.clone()
    }

    fn notify(&self) {
        self.cv.notify_all();
        (self.wake)();
    }

    /// Ask for a download from `pos` unless one is already on its way there.
    fn want(&self, st: &mut State, pos: u64) {
        let covered = st.ahead(pos) > 0 || st.at_end(pos);
        let coming = st.cursor.is_some_and(|c| c <= pos && pos - c < NEAR) || st.asked == Some(pos);
        if !covered && !coming {
            st.asked = Some(pos);
            let _ = self.restart.send(pos);
        }
    }
}

/// Starts downloading `url` into `path` and returns the reader for the decoder. The download
/// stops and the file is deleted when the reader is dropped.
pub fn open(client: reqwest::Client, url: String, path: PathBuf, rt: &tokio::runtime::Handle, wake: Box<dyn Fn() + Send + Sync>) -> io::Result<(NetReader, Arc<Src>)> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let writer = OpenOptions::new().create(true).truncate(true).write(true).read(true).open(&path)?;
    let reader = File::open(&path)?;
    let (tx, rx) = unbounded_channel();
    let src = Arc::new(Src { st: Mutex::new(State::default()), cv: Condvar::new(), read_pos: AtomicU64::new(0), restart: tx, wake });
    rt.spawn(download(client, url, writer, path, src.clone(), rx));
    Ok((NetReader { src: src.clone(), file: reader, pos: 0 }, src))
}

enum Next {
    Restart(u64),
    Done,
    Failed(String, u64),
    Closed,
}

async fn download(client: reqwest::Client, url: String, mut file: File, path: PathBuf, src: Arc<Src>, mut rx: UnboundedReceiver<u64>) {
    let mut start = 0;
    let mut tries = 0;
    loop {
        let next = fetch(&client, &url, start, &mut file, &src, &mut rx).await;
        src.st.lock().unwrap().cursor = None;
        let wait_for_restart = match next {
            Next::Restart(p) => {
                (start, tries) = (p, 0);
                false
            }
            Next::Failed(e, at) if tries < 3 => {
                tries += 1;
                log(&format!("download retry {tries} at byte {at}: {e}"));
                tokio::time::sleep(Duration::from_millis(500 << tries)).await;
                start = at;
                false
            }
            Next::Failed(e, _) => {
                src.st.lock().unwrap().failed = Some(e);
                src.notify();
                true
            }
            Next::Done => true,
            Next::Closed => break,
        };
        if wait_for_restart {
            // nothing to do until a seek lands in a gap (or the reader goes away)
            match rx.recv().await {
                Some(p) if p != CLOSE => {
                    (start, tries) = (p, 0);
                    src.st.lock().unwrap().failed = None;
                }
                _ => break,
            }
        }
    }
    drop(file);
    let _ = std::fs::remove_file(&path);
}

async fn fetch(client: &reqwest::Client, url: &str, start: u64, file: &mut File, src: &Src, rx: &mut UnboundedReceiver<u64>) -> Next {
    let req = client.get(url).header(reqwest::header::RANGE, format!("bytes={start}-")).send();
    let mut resp = tokio::select! {
        r = req => match r {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => return Next::Failed(format!("HTTP {}", r.status()), start),
            Err(e) => return Next::Failed(e.to_string(), start),
        },
        p = rx.recv() => return match p { Some(p) if p != CLOSE => Next::Restart(p), _ => Next::Closed },
    };
    // 206: the range we asked for. 200: the server ignores ranges and sends it all from 0.
    let partial = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let mut at = if partial { start } else { 0 };
    let total = if partial {
        resp.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok()).and_then(|v| v.rsplit('/').next()?.parse().ok())
    } else {
        resp.content_length()
    };
    {
        let mut st = src.st.lock().unwrap();
        if st.total.is_none() {
            st.total = total;
        }
        st.cursor = Some(at);
        st.asked = None;
    }
    src.notify();
    loop {
        let chunk = tokio::select! {
            c = resp.chunk() => c,
            p = rx.recv() => return match p { Some(p) if p != CLOSE => Next::Restart(p), _ => Next::Closed },
        };
        match chunk {
            Ok(Some(b)) => {
                if let Err(e) = file.seek(SeekFrom::Start(at)).and_then(|_| file.write_all(&b)) {
                    return Next::Failed(format!("cache file: {e}"), at);
                }
                let mut st = src.st.lock().unwrap();
                st.add(at, at + b.len() as u64);
                at += b.len() as u64;
                st.cursor = Some(at);
                drop(st);
                src.notify();
            }
            Ok(None) => {
                let mut st = src.st.lock().unwrap();
                if st.total.is_none() {
                    st.total = Some(at); // no length header: the end of the body is the end
                }
                drop(st);
                src.notify();
                // shortcut: after a seek forward, the gap behind it is only fetched when playback
                // seeks back into it. Fill gaps in the background if seeking back proves common.
                return Next::Done;
            }
            Err(e) => return Next::Failed(e.to_string(), at),
        }
    }
}

/// The decoder's view: a seekable file that waits for bytes still on their way.
pub struct NetReader {
    src: Arc<Src>,
    file: File,
    pos: u64,
}

impl Read for NetReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let began = Instant::now();
        let mut st = self.src.st.lock().unwrap();
        loop {
            if let Some(e) = &st.failed {
                return Err(io::Error::other(e.clone()));
            }
            let n = st.ahead(self.pos).min(buf.len() as u64) as usize;
            if n > 0 {
                drop(st);
                self.file.seek(SeekFrom::Start(self.pos))?;
                let got = self.file.read(&mut buf[..n])?;
                self.pos += got as u64;
                self.src.read_pos.store(self.pos, Ordering::Relaxed);
                return Ok(got);
            }
            if st.at_end(self.pos) || buf.is_empty() {
                return Ok(0);
            }
            self.src.want(&mut st, self.pos);
            let left = STALL.checked_sub(began.elapsed()).ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "the download stalled"))?;
            st = self.src.cv.wait_timeout(st, left).unwrap().0;
        }
    }
}

impl Seek for NetReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let mut st = self.src.st.lock().unwrap();
        let pos = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => st.total.and_then(|t| t.checked_add_signed(d)),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek outside the file"))?;
        self.pos = pos;
        self.src.read_pos.store(pos, Ordering::Relaxed);
        self.src.want(&mut st, pos);
        Ok(pos)
    }
}

impl MediaSource for NetReader {
    fn is_seekable(&self) -> bool {
        self.src.st.lock().unwrap().total.is_some()
    }

    fn byte_len(&self) -> Option<u64> {
        self.src.st.lock().unwrap().total
    }
}

impl Drop for NetReader {
    fn drop(&mut self) {
        let _ = self.src.restart.send(CLOSE);
    }
}

/// Where in-progress streams are cached: `$XDG_CACHE_HOME/hark/tmp` (or the platform's cache dir).
pub fn tmp_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| {
        let home = std::env::var_os(if cfg!(windows) { "LOCALAPPDATA" } else { "HOME" }).map(PathBuf::from)?;
        Some(if cfg!(windows) {
            home
        } else if cfg!(target_os = "macos") {
            home.join("Library/Caches")
        } else {
            home.join(".cache")
        })
    });
    base.unwrap_or_else(std::env::temp_dir).join("hark").join("tmp")
}

// shortcut: dev logging to stderr until hark has a TUI and a log file.
fn log(msg: &str) {
    eprintln!("hark: {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_merge_and_measure() {
        let mut st = State::default();
        st.add(100, 200);
        st.add(0, 50);
        st.add(50, 100); // fills the gap: one span
        assert_eq!(st.spans, [(0, 200)]);
        st.add(500, 600);
        st.add(150, 250); // overlaps the end
        assert_eq!(st.spans, [(0, 250), (500, 600)]);
        assert_eq!(st.ahead(0), 250);
        assert_eq!(st.ahead(240), 10);
        assert_eq!(st.ahead(250), 0, "end is exclusive");
        assert_eq!(st.ahead(300), 0, "in a gap");
        assert_eq!(st.ahead(550), 50);
        st.total = Some(600);
        assert!(st.at_end(600) && !st.at_end(599));
    }
}
