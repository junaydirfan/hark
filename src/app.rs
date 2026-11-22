//! What's on screen, what's playing, and what each key does.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::{ListState, TableState};
use tokio::sync::{Semaphore, mpsc::UnboundedSender};

use crate::{
    engine::{Cmd, Engine, Event},
    feed::{self, Show},
    store::{self, Cached, Listen, Prefs, Sub},
};

/// Feeds are refreshed at launch when older than this, and on `R`.
const STALE_S: i64 = 3600;
/// While playing, the position is saved this often (also on pause, seek, switch and quit).
const SAVE_EVERY_S: f64 = 30.0;
/// Seconds before the end that count as finished when picking where to resume.
const NEAR_END_S: f64 = 30.0;

pub enum Msg {
    /// A feed fetch finished: Some = new content, None = not modified.
    Feed { url: String, res: Result<Option<Cached>, String>, adding: bool },
}

pub struct ShowView {
    pub sub: Sub,
    pub show: Show,
    etag: String,
    last_modified: String,
    fetched: i64,
    pub loading: bool,
    pub error: Option<String>,
}

pub struct Now {
    pub guid: String,
    pub title: String,
    pub show: String,
    pub duration: f64,
    pub pos: f64,
    pub playing: bool,
    pub buffering: bool,
    saved_at: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Shows,
    Episodes,
    Input,
}

pub struct App {
    pub shows: Vec<ShowView>,
    pub show_state: ListState,
    pub ep_state: TableState,
    pub focus: Focus,
    pub input: String,
    pub status: String,
    pub state: HashMap<String, Listen>,
    pub now: Option<Now>,
    pub prefs: Prefs,
    pub quit: bool,
    engine: Engine,
    tx: UnboundedSender<Msg>,
    client: reqwest::Client,
    fetches: Arc<Semaphore>,
    confirm_remove: Option<(String, Instant)>,
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Newest episode first (feeds usually are; not all of them).
fn sort_episodes(show: &mut Show) {
    show.episodes.sort_by_key(|e| std::cmp::Reverse(e.published));
}

impl App {
    pub fn new(engine: Engine, tx: UnboundedSender<Msg>) -> App {
        let prefs = store::load_prefs();
        engine.send(Cmd::Speed(prefs.speed));
        engine.send(Cmd::Volume(prefs.volume as f32 / 100.0));
        // cache first: everything below is on screen in the first frame, before any network
        let shows = store::load_subs()
            .into_iter()
            .map(|sub| {
                let c = store::load_feed(&sub.url).unwrap_or_default();
                let mut show = c.show;
                if show.title.is_empty() {
                    show.title = sub.title.clone();
                }
                sort_episodes(&mut show);
                ShowView { sub, show, etag: c.etag, last_modified: c.last_modified, fetched: c.fetched, loading: false, error: None }
            })
            .collect();
        let mut app = App {
            shows,
            show_state: ListState::default().with_selected(Some(0)),
            ep_state: TableState::default().with_selected(Some(0)),
            focus: Focus::Shows,
            input: String::new(),
            status: String::new(),
            state: store::load_state(),
            now: None,
            prefs,
            quit: false,
            engine,
            tx,
            client: reqwest::Client::builder().user_agent(concat!("hark/", env!("CARGO_PKG_VERSION"), " (podcast player)")).build().expect("http client"),
            fetches: Arc::new(Semaphore::new(8)),
            confirm_remove: None,
        };
        app.sort_shows();
        if app.shows.is_empty() {
            app.focus = Focus::Input;
            app.status = "Welcome! Paste a podcast's feed URL, or the path to an OPML file exported from another app.".into();
        }
        app
    }

    pub fn selected_show(&self) -> Option<&ShowView> {
        self.shows.get(self.show_state.selected()?)
    }

    // ---------------------------------------------------------------- feeds

    /// Refresh every feed (or only stale ones) a few at a time.
    pub fn refresh(&mut self, force: bool) {
        let now = unix_now();
        let mut n = 0;
        for i in 0..self.shows.len() {
            let s = &self.shows[i];
            if !s.loading && (force || now - s.fetched > STALE_S) {
                let (url, etag, lm) = (s.sub.url.clone(), s.etag.clone(), s.last_modified.clone());
                self.shows[i].loading = true;
                self.fetch(url, etag, lm, false);
                n += 1;
            }
        }
        if force {
            self.status = if n == 0 { "Everything is being refreshed already.".into() } else { format!("Refreshing {n} show{}…", if n == 1 { "" } else { "s" }) };
        }
    }

    fn fetch(&self, url: String, etag: String, last_modified: String, adding: bool) {
        let (client, tx, gate) = (self.client.clone(), self.tx.clone(), self.fetches.clone());
        tokio::spawn(async move {
            let _permit = gate.acquire().await;
            let res = fetch_feed(&client, &url, &etag, &last_modified).await;
            let _ = tx.send(Msg::Feed { url, res, adding });
        });
    }

    pub fn on_msg(&mut self, msg: Msg) {
        let Msg::Feed { url, res, adding } = msg;
        // a page that isn't RSS fails as XML; say what that means
        let res = res.map_err(|e| if e.starts_with("XML error") || e == "not a podcast feed" { "that address isn't a podcast feed".to_string() } else { e });
        let i = self.shows.iter().position(|s| s.sub.url == url);
        match (res, i) {
            (Err(e), None) => self.status = format!("Couldn't add {url}: {e}"),
            (Err(e), Some(i)) => {
                let s = &mut self.shows[i];
                (s.loading, s.error) = (false, Some(e.clone()));
                if adding {
                    self.status = format!("Couldn't load {}: {e}", s.show.title);
                }
            }
            (Ok(None), Some(i)) => {
                let s = &mut self.shows[i];
                (s.loading, s.error, s.fetched) = (false, None, unix_now());
                let c = Cached { show: std::mem::take(&mut s.show), etag: s.etag.clone(), last_modified: s.last_modified.clone(), fetched: s.fetched };
                let _ = store::save_feed(&url, &c);
                s.show = c.show;
            }
            (Ok(None), None) => {}
            (Ok(Some(mut c)), i) => {
                // The parse thread allocated these among its own freed scraps; copies made here
                // leave its arena empty, so trim_heap can hand all of it back (8 MB for The Daily).
                c.show.episodes = c.show.episodes.to_vec();
                sort_episodes(&mut c.show);
                let renamed = i.is_some_and(|i| self.shows[i].sub.title != c.show.title);
                let moved = c.show.new_url.clone().filter(|n| *n != url && !self.shows.iter().any(|s| s.sub.url == *n));
                let key = moved.clone().unwrap_or_else(|| url.clone());
                let _ = store::save_feed(&key, &c);
                let sub = Sub { url: key.clone(), title: c.show.title.clone() };
                let view = ShowView { sub, show: c.show, etag: c.etag, last_modified: c.last_modified, fetched: c.fetched, loading: false, error: None };
                let title = view.show.title.clone();
                match i {
                    Some(i) => self.shows[i] = view,
                    None => self.shows.push(view),
                }
                if let Some(m) = &moved {
                    store::forget_feed(&url);
                    self.status = format!("{title} moved to {m}; following it.");
                }
                if adding {
                    self.status = format!("Added {title}.");
                    if self.focus == Focus::Input {
                        self.focus = Focus::Shows;
                    }
                }
                if adding || moved.is_some() || renamed {
                    self.save_subs();
                }
                self.sort_shows_keeping(adding.then_some(key.as_str()));
                crate::trim_heap();
            }
        }
        if !self.shows.iter().any(|s| s.loading) && (self.status.starts_with("Refreshing") || self.status.starts_with("Importing")) {
            let failed = self.shows.iter().filter(|s| s.error.is_some()).count();
            self.status = match failed {
                0 => "Everything is up to date.".into(),
                n => format!("Up to date, except {n} show{} that couldn't be reached (marked !).", if n == 1 { "" } else { "s" }),
            };
        }
    }

    fn save_subs(&mut self) {
        for s in &mut self.shows {
            if !s.show.title.is_empty() {
                s.sub.title = s.show.title.clone();
            }
        }
        let subs: Vec<Sub> = self.shows.iter().map(|s| s.sub.clone()).collect();
        if let Err(e) = store::save_subs(&subs) {
            self.status = format!("Couldn't save subscriptions: {e}");
        }
    }

    fn sort_shows(&mut self) {
        self.sort_shows_keeping(None);
    }

    /// Newest episode first; keeps the selected show selected (or selects `select`).
    fn sort_shows_keeping(&mut self, select: Option<&str>) {
        let keep = select.map(str::to_string).or_else(|| self.selected_show().map(|s| s.sub.url.clone()));
        self.shows.sort_by_key(|s| std::cmp::Reverse(s.show.episodes.first().map_or(0, |e| e.published)));
        let i = keep.and_then(|u| self.shows.iter().position(|s| s.sub.url == u)).unwrap_or(0);
        self.show_state.select(Some(i.min(self.shows.len().saturating_sub(1))));
    }

    /// `a`: a feed url subscribes; anything else is read as an OPML file to import.
    fn add(&mut self, text: String) {
        let t = text.trim();
        if t.is_empty() {
            return;
        }
        if t.starts_with("http://") || t.starts_with("https://") {
            if self.shows.iter().any(|s| s.sub.url == t) {
                self.status = "You're already subscribed to that one.".into();
            } else {
                self.status = format!("Adding {t}…");
                self.fetch(t.to_string(), String::new(), String::new(), true);
            }
            return;
        }
        let path = match t.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME").map_or(t.into(), |h| std::path::Path::new(&h).join(rest)),
            None => t.into(),
        };
        let subs = match std::fs::read(&path) {
            Ok(b) => store::parse_opml(&b),
            Err(e) => {
                self.status = format!("{}: {e} (paste a feed URL starting with https://, or an OPML file's path)", path.display());
                return;
            }
        };
        let new: Vec<Sub> = subs.into_iter().filter(|s| !self.shows.iter().any(|v| v.sub.url == s.url)).collect();
        if new.is_empty() {
            self.status = "No new shows in that file.".into();
            return;
        }
        self.status = format!("Importing {} shows…", new.len());
        for sub in new {
            let show = Show { title: sub.title.clone(), ..Default::default() };
            self.fetch(sub.url.clone(), String::new(), String::new(), false);
            self.shows.push(ShowView { sub, show, etag: String::new(), last_modified: String::new(), fetched: 0, loading: true, error: None });
        }
        self.save_subs();
        self.focus = Focus::Shows;
    }

    fn unsubscribe(&mut self) {
        let Some(i) = self.show_state.selected().filter(|&i| i < self.shows.len()) else { return };
        let url = self.shows[i].sub.url.clone();
        let title = self.shows[i].show.title.clone();
        if self.confirm_remove.as_ref().is_some_and(|(u, t)| *u == url && t.elapsed().as_secs() < 3) {
            self.confirm_remove = None;
            self.shows.remove(i);
            store::forget_feed(&url);
            self.save_subs();
            self.show_state.select(Some(i.min(self.shows.len().saturating_sub(1))));
            self.status = format!("Unsubscribed from {title}.");
        } else {
            self.confirm_remove = Some((url, Instant::now()));
            self.status = format!("Press D again to unsubscribe from {title}.");
        }
    }

    // ---------------------------------------------------------------- playback

    fn play_selected(&mut self) {
        let (Some(si), Some(ei)) = (self.show_state.selected(), self.ep_state.selected()) else { return };
        let Some(sv) = self.shows.get(si) else { return };
        let Some(e) = sv.show.episodes.get(ei) else { return };
        let (guid, title, url, dur, show) = (e.guid.clone(), e.title.clone(), e.url.clone(), e.duration_s as f64, sv.show.title.clone());
        self.remember();
        let l = self.state.get(&guid).copied().unwrap_or_default();
        let at = if l.played || (dur > 0.0 && l.pos > dur - NEAR_END_S) { 0.0 } else { l.pos };
        self.engine.send(Cmd::Load { src: url, at_s: at });
        self.status = if at > 0.0 { format!("Resuming at {}.", fmt_s(at)) } else { String::new() };
        self.now = Some(Now { guid, title, show, duration: dur, pos: at, playing: true, buffering: false, saved_at: at });
    }

    fn toggle_pause(&mut self) {
        let Some(n) = &mut self.now else { return };
        n.playing = !n.playing;
        self.engine.send(Cmd::Pause(!n.playing));
        self.remember();
    }

    fn seek_by(&mut self, d: f64) {
        let Some(n) = &mut self.now else { return };
        let max = if n.duration > 0.0 { n.duration - 1.0 } else { f64::MAX };
        n.pos = (n.pos + d).clamp(0.0, max.max(0.0));
        self.engine.send(Cmd::Seek(n.pos));
        self.remember();
    }

    fn set_speed(&mut self, delta: f32) {
        self.prefs.speed = ((self.prefs.speed + delta) * 10.0).round().clamp(5.0, 30.0) / 10.0;
        self.engine.send(Cmd::Speed(self.prefs.speed));
        let _ = store::save_prefs(&self.prefs);
    }

    fn set_volume(&mut self, delta: i16) {
        self.prefs.volume = (self.prefs.volume as i16 + delta).clamp(0, 100) as u8;
        self.engine.send(Cmd::Volume(self.prefs.volume as f32 / 100.0));
        let _ = store::save_prefs(&self.prefs);
    }

    /// Save where the playing episode is.
    fn remember(&mut self) {
        let Some(n) = &mut self.now else { return };
        n.saved_at = n.pos;
        let l = self.state.entry(n.guid.clone()).or_default();
        if !l.played {
            l.pos = n.pos;
        }
        self.save_state();
    }

    fn save_state(&mut self) {
        if let Err(e) = store::save_state(&self.state) {
            self.status = format!("Couldn't save your place: {e}");
        }
    }

    fn toggle_played(&mut self) {
        let (Some(si), Some(ei)) = (self.show_state.selected(), self.ep_state.selected()) else { return };
        let Some(e) = self.shows.get(si).and_then(|s| s.show.episodes.get(ei)) else { return };
        let l = self.state.entry(e.guid.clone()).or_default();
        *l = Listen { pos: 0.0, played: !l.played };
        self.save_state();
        self.move_sel(1);
    }

    /// Returns whether the screen changed.
    pub fn on_engine(&mut self, ev: Event) -> bool {
        let Some(n) = &mut self.now else { return false };
        match ev {
            Event::Loaded { duration_s } => {
                if duration_s > 0.0 {
                    n.duration = duration_s;
                }
            }
            Event::Position(p) => {
                n.pos = p;
                if (p - n.saved_at).abs() >= SAVE_EVERY_S {
                    self.remember();
                }
            }
            Event::Buffering(b) => n.buffering = b,
            Event::Ended => {
                n.playing = false;
                n.pos = n.duration;
                self.state.insert(n.guid.clone(), Listen { pos: 0.0, played: true });
                self.status = format!("Finished {}.", n.title);
                self.save_state();
            }
            Event::Error(e) => {
                n.playing = false;
                self.status = e;
            }
        }
        true
    }

    pub fn shutdown(&mut self) {
        self.remember();
        let _ = store::save_prefs(&self.prefs);
    }

    // ---------------------------------------------------------------- keys

    fn move_sel(&mut self, delta: isize) {
        let (len, state_sel) = match self.focus {
            Focus::Shows => (self.shows.len(), self.show_state.selected()),
            _ => (self.selected_show().map_or(0, |s| s.show.episodes.len()), self.ep_state.selected()),
        };
        if len == 0 {
            return;
        }
        let next = (state_sel.unwrap_or(0) as isize + delta).clamp(0, len as isize - 1) as usize;
        match self.focus {
            Focus::Shows => {
                if Some(next) != self.show_state.selected() {
                    self.show_state.select(Some(next));
                    self.ep_state = TableState::default().with_selected(Some(0));
                }
            }
            _ => self.ep_state.select(Some(next)),
        }
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.focus == Focus::Input {
            match k.code {
                KeyCode::Esc => {
                    self.input.clear();
                    self.focus = Focus::Shows;
                    self.status.clear();
                }
                KeyCode::Enter => {
                    let t = std::mem::take(&mut self.input);
                    self.focus = Focus::Shows;
                    self.add(t);
                }
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Char(c) => self.input.push(c),
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Tab | KeyCode::BackTab => self.focus = if self.focus == Focus::Shows { Focus::Episodes } else { Focus::Shows },
            KeyCode::Char('h') => self.focus = Focus::Shows,
            KeyCode::Char('l') => self.focus = Focus::Episodes,
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::PageDown => self.move_sel(20),
            KeyCode::PageUp => self.move_sel(-20),
            KeyCode::Home | KeyCode::Char('g') => self.move_sel(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_sel(isize::MAX / 2),
            KeyCode::Enter => {
                if self.focus == Focus::Shows {
                    self.focus = Focus::Episodes;
                } else {
                    self.play_selected();
                }
            }
            KeyCode::Esc | KeyCode::Backspace => self.focus = Focus::Shows,
            KeyCode::Char(' ') => self.toggle_pause(),
            KeyCode::Left | KeyCode::Char(',') => self.seek_by(-15.0),
            KeyCode::Right | KeyCode::Char('.') => self.seek_by(30.0),
            KeyCode::Char('[') => self.set_speed(-0.1),
            KeyCode::Char(']') => self.set_speed(0.1),
            KeyCode::Char('+') | KeyCode::Char('=') => self.set_volume(5),
            KeyCode::Char('-') => self.set_volume(-5),
            KeyCode::Char('m') => self.toggle_played(),
            KeyCode::Char('a') => {
                self.focus = Focus::Input;
                self.status.clear();
            }
            KeyCode::Char('R') => self.refresh(true),
            KeyCode::Char('D') => self.unsubscribe(),
            _ => {}
        }
    }
}

/// GET with "only if changed" headers; parses off the async thread (a big feed takes ~20 ms).
async fn fetch_feed(client: &reqwest::Client, url: &str, etag: &str, last_modified: &str) -> Result<Option<Cached>, String> {
    let mut req = client.get(url);
    if !etag.is_empty() {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    if !last_modified.is_empty() {
        req = req.header(reqwest::header::IF_MODIFIED_SINCE, last_modified);
    }
    let resp = req.send().await.map_err(cause)?;
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let header = |h| resp.headers().get(h).and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok()).unwrap_or("").to_string();
    let (new_etag, last_modified) = (header(reqwest::header::ETAG), header(reqwest::header::LAST_MODIFIED));
    // some servers (NPR) ignore If-None-Match but still send the same ETag: unchanged, skip the parse
    if !etag.is_empty() && new_etag == etag {
        return Ok(None);
    }
    let etag = new_etag;
    let body = resp.bytes().await.map_err(cause)?;
    let show = tokio::task::spawn_blocking(move || {
        let mut show = feed::parse(&body)?;
        // shortcut: notes aren't shown yet (Phase 2 loads them on demand); keeping them costs MBs
        for e in &mut show.episodes {
            e.notes = String::new();
        }
        Ok::<_, String>(show)
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(Some(Cached { show, etag, last_modified, fetched: unix_now() }))
}

/// reqwest's own message ("error sending request") hides the reason; the innermost one says it.
fn cause(e: reqwest::Error) -> String {
    let mut src: &dyn std::error::Error = &e;
    while let Some(next) = src.source() {
        src = next;
    }
    if e.is_timeout() { "timed out".into() } else { src.to_string() }
}

/// 75 → "1:15", 3725 → "1:02:05".
pub fn fmt_s(s: f64) -> String {
    let s = s.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// Unix seconds → "2025-11-19" (UTC; a day off near midnight is fine for a list of episodes).
pub fn fmt_date(t: i64) -> String {
    if t <= 0 {
        return String::new();
    }
    let z = t.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(fmt_s(75.9), "1:15");
        assert_eq!(fmt_s(3725.0), "1:02:05");
        assert_eq!(fmt_s(-3.0), "0:00");
        assert_eq!(fmt_date(0), "");
        assert_eq!(fmt_date(86400), "1970-01-02");
        assert_eq!(fmt_date(1582957800), "2020-02-29");
        assert_eq!(fmt_date(feed::rfc2822("Wed, 31 Dec 2025 23:00:00 GMT").unwrap()), "2025-12-31");
    }
}
