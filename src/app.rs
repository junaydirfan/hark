//! What's on screen, what's playing, and what each key does.

use std::{
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Position, Rect},
    widgets::{ListState, TableState},
};
use tokio::sync::{Semaphore, mpsc::UnboundedSender};

use crate::{
    engine::{Cmd, Engine, Event},
    feed::{self, Episode, Show},
    store::{self, Cached, Listen, Prefs, Sub},
};

/// A feed url, shared by every row that points into that feed.
pub type Url = Rc<str>;

/// Sidebar entries above the shows.
pub const FIXED: [&str; 3] = ["Inbox", "In progress", "Queue"];
/// Feeds are refreshed at launch when older than this, and on `R`.
const STALE_S: i64 = 3600;
/// While playing, the position is saved this often (also on pause, seek, switch and quit).
const SAVE_EVERY_S: f64 = 30.0;
/// Seconds before the end that count as finished when picking where to resume.
const NEAR_END_S: f64 = 30.0;
/// The Inbox: unplayed episodes from the last 30 days, newest first.
const INBOX_DAYS: i64 = 30;
const INBOX_MAX: usize = 300;

pub enum Msg {
    /// A feed fetch finished: Some = new content, None = not modified.
    Feed { url: String, res: Result<Option<Cached>, String>, adding: bool },
}

pub struct ShowView {
    pub sub: Sub,
    pub url: Url,
    pub show: Show,
    etag: String,
    last_modified: String,
    fetched: i64,
    pub loading: bool,
    pub error: Option<String>,
}

#[derive(Clone, PartialEq)]
pub enum Kind {
    Inbox,
    InProgress,
    Queue,
    Show(Url),
}

/// A row in the main table.
#[derive(Clone)]
pub enum Item {
    Ep { show: Url, i: usize },
}

pub struct View {
    pub kind: Kind,
    pub title: String,
    pub rows: Vec<Item>,
    pub state: TableState,
}

pub struct Now {
    pub show_url: Url,
    pub guid: String,
    pub title: String,
    pub show: String,
    pub duration: f64,
    pub pos: f64,
    pub playing: bool,
    pub buffering: bool,
    saved_at: f64,
}

/// `L`: an episode's show notes in place of the table.
pub struct Notes {
    pub title: String,
    pub lines: Vec<String>,
    pub links: Vec<String>,
    pub scroll: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Side,
    Main,
    Input,
}

pub struct App {
    pub shows: Vec<ShowView>,
    pub side_state: ListState,
    pub view: View,
    back: Vec<View>,
    pub focus: Focus,
    pub input: String,
    pub status: String,
    pub state: HashMap<String, Listen>,
    pub now: Option<Now>,
    pub prefs: Prefs,
    pub queue: VecDeque<(Url, String)>,
    /// Inbox, In progress, Queue sizes for the sidebar.
    pub counts: [usize; 3],
    pub notes: Option<Notes>,
    pub help_open: bool,
    pub quit: bool,
    /// Where the last frame drew the sidebar list, the table and the progress bar (for clicks).
    pub side_area: Rect,
    pub main_area: Rect,
    pub bar_area: Rect,
    last_click: Option<(Instant, usize)>,
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
        crate::theme::set(prefs.theme);
        // cache first: everything below is on screen in the first frame, before any network
        let shows: Vec<ShowView> = store::load_subs()
            .into_iter()
            .map(|sub| {
                let c = store::load_feed(&sub.url).unwrap_or_default();
                let mut show = c.show;
                if show.title.is_empty() {
                    show.title = sub.title.clone();
                }
                sort_episodes(&mut show);
                ShowView { url: sub.url.as_str().into(), sub, show, etag: c.etag, last_modified: c.last_modified, fetched: c.fetched, loading: false, error: None }
            })
            .collect();
        let queue = store::load_queue()
            .into_iter()
            .filter_map(|(u, g)| Some((shows.iter().find(|s| *s.url == *u)?.url.clone(), g)))
            .collect();
        let mut app = App {
            shows,
            side_state: ListState::default().with_selected(Some(0)),
            view: View { kind: Kind::Inbox, title: String::new(), rows: Vec::new(), state: TableState::default() },
            back: Vec::new(),
            focus: Focus::Side,
            input: String::new(),
            status: String::new(),
            state: store::load_state(),
            now: None,
            prefs,
            queue,
            counts: [0; 3],
            notes: None,
            help_open: false,
            quit: false,
            side_area: Rect::default(),
            main_area: Rect::default(),
            bar_area: Rect::default(),
            last_click: None,
            engine,
            tx,
            client: reqwest::Client::builder().user_agent(concat!("hark/", env!("CARGO_PKG_VERSION"), " (podcast player)")).build().expect("http client"),
            fetches: Arc::new(Semaphore::new(8)),
            confirm_remove: None,
        };
        app.sort_shows();
        app.rebuild();
        if app.shows.is_empty() {
            app.focus = Focus::Input;
            app.status = "Welcome! Paste a podcast's feed URL, or the path to an OPML file exported from another app.".into();
        }
        app
    }

    // ---------------------------------------------------------------- lookups

    pub fn show(&self, url: &str) -> Option<&ShowView> {
        self.shows.iter().find(|s| *s.url == *url)
    }

    pub fn episode(&self, item: &Item) -> Option<(&ShowView, &Episode)> {
        let Item::Ep { show, i } = item;
        let s = self.show(show)?;
        Some((s, s.show.episodes.get(*i)?))
    }

    fn selected_item(&self) -> Option<&Item> {
        self.view.rows.get(self.view.state.selected()?)
    }

    /// The show the sidebar or the open view points at (for `D` and the like).
    fn current_show(&self) -> Option<Url> {
        match (&self.focus, &self.view.kind) {
            (Focus::Main, Kind::Show(u)) => Some(u.clone()),
            _ => self.side_state.selected().and_then(|i| i.checked_sub(FIXED.len())).and_then(|i| self.shows.get(i)).map(|s| s.url.clone()),
        }
    }

    // ---------------------------------------------------------------- views

    fn rows_for(&self, kind: &Kind) -> Vec<Item> {
        let all = || self.shows.iter().flat_map(|s| s.show.episodes.iter().enumerate().map(move |(i, e)| (s, i, e)));
        let listen = |g: &str| self.state.get(g).copied().unwrap_or_default();
        match kind {
            Kind::Inbox => {
                let since = unix_now() - INBOX_DAYS * 86400;
                let mut v: Vec<(i64, Item)> = all()
                    .filter(|(_, _, e)| e.published >= since && !listen(&e.guid).played)
                    .map(|(s, i, e)| (e.published, Item::Ep { show: s.url.clone(), i }))
                    .collect();
                v.sort_by_key(|(p, _)| std::cmp::Reverse(*p));
                v.into_iter().take(INBOX_MAX).map(|(_, it)| it).collect()
            }
            Kind::InProgress => {
                let mut v: Vec<(i64, Item)> = all()
                    .filter_map(|(s, i, e)| {
                        let l = listen(&e.guid);
                        (l.pos > 0.0 && !l.played).then(|| (l.updated, Item::Ep { show: s.url.clone(), i }))
                    })
                    .collect();
                v.sort_by_key(|(u, _)| std::cmp::Reverse(*u));
                v.into_iter().map(|(_, it)| it).collect()
            }
            Kind::Queue => self.queue.iter().filter_map(|(u, g)| self.locate(u, g)).collect(),
            Kind::Show(u) => self.show(u).map_or(Vec::new(), |s| (0..s.show.episodes.len()).map(|i| Item::Ep { show: u.clone(), i }).collect()),
        }
    }

    /// The row for an episode by its guid (indexes shift when a feed refreshes).
    fn locate(&self, url: &str, guid: &str) -> Option<Item> {
        let s = self.show(url)?;
        let i = s.show.episodes.iter().position(|e| e.guid == guid)?;
        Some(Item::Ep { show: s.url.clone(), i })
    }

    fn key_of(&self, item: &Item) -> Option<(Url, String)> {
        self.episode(item).map(|(s, e)| (s.url.clone(), e.guid.clone()))
    }

    /// Recompute the open view and the sidebar counts, keeping the selected row selected.
    fn rebuild(&mut self) {
        let keep = self.selected_item().and_then(|it| self.key_of(it));
        let rows = self.rows_for(&self.view.kind);
        let n = rows.len();
        self.view.title = match &self.view.kind {
            Kind::Inbox => format!("Inbox · {n} new in the last {INBOX_DAYS} days"),
            Kind::InProgress => format!("In progress · {n}"),
            Kind::Queue => format!("Queue · {n}"),
            Kind::Show(u) => match self.show(u) {
                Some(s) => {
                    let mut t = format!("{} · {n} episodes", s.show.title);
                    if s.loading {
                        t.push_str(" (refreshing…)");
                    }
                    if let Some(e) = &s.error {
                        t.push_str(&format!(" (couldn't refresh: {e})"));
                    }
                    t
                }
                None => "Show".into(),
            },
        };
        self.view.rows = rows;
        let sel = keep.and_then(|(u, g)| self.view.rows.iter().position(|it| self.key_of(it).is_some_and(|(a, b)| a == u && b == g)));
        let cur = self.view.state.selected().unwrap_or(0);
        self.view.state.select(Some(sel.unwrap_or(cur).min(n.saturating_sub(1))));
        self.counts = [self.rows_for(&Kind::Inbox).len(), self.rows_for(&Kind::InProgress).len(), self.queue.len()];
    }

    fn open(&mut self, kind: Kind) {
        self.notes = None;
        self.back.clear();
        self.view = View { kind, title: String::new(), rows: Vec::new(), state: TableState::default().with_selected(Some(0)) };
        self.rebuild();
        self.focus = Focus::Main;
    }

    fn open_side(&mut self) {
        let Some(i) = self.side_state.selected() else { return };
        let kind = match i {
            0 => Kind::Inbox,
            1 => Kind::InProgress,
            2 => Kind::Queue,
            _ => match self.shows.get(i - FIXED.len()) {
                Some(s) => Kind::Show(s.url.clone()),
                None => return,
            },
        };
        self.open(kind);
    }

    /// `w`: the playing episode, in its show.
    fn jump_to_playing(&mut self) {
        let Some((url, guid)) = self.now.as_ref().map(|n| (n.show_url.clone(), n.guid.clone())) else { return };
        if let Some(i) = self.shows.iter().position(|s| s.url == url) {
            self.side_state.select(Some(i + FIXED.len()));
        }
        self.open(Kind::Show(url.clone()));
        if let Some(r) = self.view.rows.iter().position(|it| self.key_of(it).is_some_and(|(_, g)| g == guid)) {
            self.view.state.select(Some(r));
        }
    }

    // ---------------------------------------------------------------- feeds

    /// Refresh every feed (or only stale ones) a few at a time.
    pub fn refresh(&mut self, force: bool) {
        let now = unix_now();
        let mut n = 0;
        for i in 0..self.shows.len() {
            let s = &self.shows[i];
            if !s.loading && (force || now - s.fetched > STALE_S) {
                let (url, mut etag, mut lm) = (s.sub.url.clone(), s.etag.clone(), s.last_modified.clone());
                // no notes on disk yet (a cache from before they were kept): fetch it whole once
                if !store::has_notes(&url) {
                    (etag, lm) = (String::new(), String::new());
                }
                self.shows[i].loading = true;
                self.fetch(url, etag, lm, false);
                n += 1;
            }
        }
        if force {
            self.status = if n == 0 { "Everything is being refreshed already.".into() } else { format!("Refreshing {n} show{}…", if n == 1 { "" } else { "s" }) };
        }
        self.rebuild();
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
                // keep the same Rc when the url didn't change, so rows pointing here stay valid
                let rc: Url = match (i, &moved) {
                    (Some(i), None) => self.shows[i].url.clone(),
                    _ => key.as_str().into(),
                };
                let view = ShowView { sub, url: rc, show: c.show, etag: c.etag, last_modified: c.last_modified, fetched: c.fetched, loading: false, error: None };
                let title = view.show.title.clone();
                match i {
                    Some(i) => self.shows[i] = view,
                    None => self.shows.push(view),
                }
                if let Some(m) = &moved {
                    // shortcut: its show notes were saved under the old address; they return with the next refresh
                    store::forget_feed(&url);
                    self.status = format!("{title} moved to {m}; following it.");
                }
                if adding {
                    self.status = format!("Added {title}.");
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
        self.rebuild();
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
        let side = self.side_state.selected().unwrap_or(0);
        let keep = select.map(str::to_string).or_else(|| side.checked_sub(FIXED.len()).and_then(|i| self.shows.get(i)).map(|s| s.sub.url.clone()));
        self.shows.sort_by_key(|s| std::cmp::Reverse(s.show.episodes.first().map_or(0, |e| e.published)));
        match keep.and_then(|u| self.shows.iter().position(|s| s.sub.url == u)) {
            Some(i) => self.side_state.select(Some(i + FIXED.len())),
            None => self.side_state.select(Some(side.min(FIXED.len() + self.shows.len().saturating_sub(1)))),
        }
    }

    /// `/`: a feed url subscribes; anything else is read as an OPML file to import.
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
            self.shows.push(ShowView { url: sub.url.as_str().into(), sub, show, etag: String::new(), last_modified: String::new(), fetched: 0, loading: true, error: None });
        }
        self.save_subs();
        self.rebuild();
    }

    fn unsubscribe(&mut self) {
        let Some(url) = self.current_show() else { return };
        let Some(i) = self.shows.iter().position(|s| s.url == url) else { return };
        let title = self.shows[i].show.title.clone();
        if self.confirm_remove.as_ref().is_some_and(|(u, t)| **u == *url && t.elapsed().as_secs() < 3) {
            self.confirm_remove = None;
            self.shows.remove(i);
            store::forget_feed(&url);
            self.save_subs();
            let before = self.queue.len();
            self.queue.retain(|(u, _)| *u != url);
            if self.queue.len() != before {
                self.save_queue();
            }
            self.side_state.select(Some((i + FIXED.len()).min(FIXED.len() + self.shows.len()).saturating_sub(1)));
            if self.view.kind == Kind::Show(url) {
                self.open(Kind::Inbox);
                self.side_state.select(Some(0));
                self.focus = Focus::Side;
            }
            self.rebuild();
            self.status = format!("Unsubscribed from {title}.");
        } else {
            self.confirm_remove = Some((url.to_string(), Instant::now()));
            self.status = format!("Press D again to unsubscribe from {title}.");
        }
    }

    // ---------------------------------------------------------------- playback

    fn play(&mut self, item: &Item) {
        let Some((s, e)) = self.episode(item) else { return };
        let (show_url, guid, title, url, dur, show) = (s.url.clone(), e.guid.clone(), e.title.clone(), e.url.clone(), e.duration_s as f64, s.show.title.clone());
        self.remember();
        let l = self.state.get(&guid).copied().unwrap_or_default();
        let at = if l.played || (dur > 0.0 && l.pos > dur - NEAR_END_S) { 0.0 } else { l.pos };
        self.engine.send(Cmd::Load { src: url, at_s: at });
        self.status = if at > 0.0 { format!("Resuming at {}.", fmt_s(at)) } else { String::new() };
        if let Some(q) = self.queue.iter().position(|(u, g)| *u == show_url && *g == guid) {
            self.queue.remove(q);
            self.save_queue();
        }
        self.now = Some(Now { show_url, guid: guid.clone(), title, show, duration: dur, pos: at, playing: true, buffering: false, saved_at: at });
        // a replayed episode starts unplayed again
        let l = self.state.entry(guid).or_default();
        (l.played, l.pos, l.updated) = (false, at, unix_now());
        self.save_state();
        self.rebuild();
    }

    fn play_selected(&mut self) {
        if let Some(it) = self.selected_item().cloned() {
            self.play(&it);
        }
    }

    /// `n`: the next episode in the queue.
    fn play_next(&mut self) -> bool {
        while let Some((u, g)) = self.queue.front().cloned() {
            if let Some(it) = self.locate(&u, &g) {
                self.play(&it);
                return true;
            }
            self.queue.pop_front(); // no longer in its feed
        }
        false
    }

    fn toggle_pause(&mut self) {
        let Some(n) = &mut self.now else { return };
        n.playing = !n.playing;
        self.engine.send(Cmd::Pause(!n.playing));
        self.remember();
    }

    fn seek_to(&mut self, to: f64) {
        let Some(n) = &mut self.now else { return };
        let max = if n.duration > 0.0 { n.duration - 1.0 } else { f64::MAX };
        n.pos = to.clamp(0.0, max.max(0.0));
        self.engine.send(Cmd::Seek(n.pos));
        self.remember();
    }

    fn seek_by(&mut self, d: f64) {
        if let Some(p) = self.now.as_ref().map(|n| n.pos) {
            self.seek_to(p + d);
        }
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
            (l.pos, l.updated) = (n.pos, unix_now());
        }
        self.save_state();
    }

    fn save_state(&mut self) {
        if let Err(e) = store::save_state(&self.state) {
            self.status = format!("Couldn't save your place: {e}");
        }
    }

    fn save_queue(&mut self) {
        if let Err(e) = store::save_queue(self.queue.iter().map(|(u, g)| (&**u, g.as_str()))) {
            self.status = format!("Couldn't save the queue: {e}");
        }
    }

    fn toggle_played(&mut self) {
        let Some(it) = self.selected_item().cloned() else { return };
        let Some((_, e)) = self.episode(&it) else { return };
        let l = self.state.entry(e.guid.clone()).or_default();
        *l = Listen { pos: 0.0, played: !l.played, updated: unix_now() };
        self.status = if l.played { "Marked played.".into() } else { "Marked unplayed.".into() };
        self.save_state();
        if matches!(self.view.kind, Kind::Show(_)) {
            self.move_sel(1);
        }
        self.rebuild();
    }

    /// `a`: add to the end of the queue (or take it out again); `A`: play it next.
    fn queue_selected(&mut self, next: bool) {
        let Some(it) = self.selected_item().cloned() else { return };
        let Some((url, guid)) = self.key_of(&it) else { return };
        let title = self.episode(&it).map(|(_, e)| e.title.clone()).unwrap_or_default();
        let at = self.queue.iter().position(|(u, g)| *u == url && *g == guid);
        match (at, next) {
            (Some(i), false) => {
                self.queue.remove(i);
                self.status = format!("Removed from the queue: {title}");
            }
            (at, true) => {
                if let Some(i) = at {
                    self.queue.remove(i);
                }
                self.queue.push_front((url, guid));
                self.status = format!("Playing next: {title}");
            }
            (None, false) => {
                self.queue.push_back((url, guid));
                self.status = format!("Queued ({}): {title}", self.queue.len());
            }
        }
        self.save_queue();
        if self.view.kind != Kind::Queue {
            self.move_sel(1);
        }
        self.rebuild();
    }

    /// `L`: the show notes of the highlighted episode (or the playing one).
    fn toggle_notes(&mut self) {
        if self.notes.take().is_some() {
            return;
        }
        let target = self
            .selected_item()
            .filter(|_| self.focus == Focus::Main)
            .and_then(|it| self.key_of(it))
            .or_else(|| self.now.as_ref().map(|n| (n.show_url.clone(), n.guid.clone())));
        let Some((url, guid)) = target else {
            self.status = "Highlight an episode (or play one) to read its notes.".into();
            return;
        };
        let title = self.locate(&url, &guid).and_then(|it| self.episode(&it).map(|(_, e)| e.title.clone())).unwrap_or_default();
        let (lines, links) = match store::load_notes(&url, &guid) {
            Some(html) => feed::notes_text(&html),
            None => (vec!["No show notes saved for this episode yet. They arrive with the next refresh (R).".into()], Vec::new()),
        };
        self.notes = Some(Notes { title, lines, links, scroll: 0 });
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
                let title = n.title.clone();
                self.state.insert(n.guid.clone(), Listen { pos: 0.0, played: true, updated: unix_now() });
                self.save_state();
                if !self.play_next() {
                    self.status = format!("Finished {title}. The queue is empty.");
                }
                self.rebuild();
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
        if let Some(n) = &mut self.notes {
            n.scroll = (n.scroll as isize + delta).clamp(0, n.lines.len().saturating_sub(1) as isize) as usize;
            return;
        }
        let (len, sel) = match self.focus {
            Focus::Side => (FIXED.len() + self.shows.len(), self.side_state.selected()),
            _ => (self.view.rows.len(), self.view.state.selected()),
        };
        if len == 0 {
            return;
        }
        let next = (sel.unwrap_or(0) as isize + delta).clamp(0, len as isize - 1) as usize;
        match self.focus {
            Focus::Side => self.side_state.select(Some(next)),
            _ => self.view.state.select(Some(next)),
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
        if self.help_open {
            self.help_open = false;
            return;
        }
        if self.focus == Focus::Input {
            match k.code {
                KeyCode::Esc => {
                    self.input.clear();
                    self.focus = Focus::Side;
                    self.status.clear();
                }
                KeyCode::Enter => {
                    let t = std::mem::take(&mut self.input);
                    self.focus = Focus::Side;
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
        if self.notes.is_some() && matches!(k.code, KeyCode::Esc | KeyCode::Backspace) {
            self.notes = None;
            return;
        }
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.help_open = true,
            KeyCode::Tab | KeyCode::BackTab => self.focus = if self.focus == Focus::Side { Focus::Main } else { Focus::Side },
            KeyCode::Char('h') => self.focus = Focus::Side,
            KeyCode::Char('l') => self.focus = Focus::Main,
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::PageDown => self.move_sel(20),
            KeyCode::PageUp => self.move_sel(-20),
            KeyCode::Home | KeyCode::Char('g') => self.move_sel(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_sel(isize::MAX / 2),
            KeyCode::Enter => {
                if self.focus == Focus::Side {
                    self.open_side();
                } else {
                    self.play_selected();
                }
            }
            KeyCode::Esc | KeyCode::Backspace => match self.back.pop() {
                Some(v) => self.view = v,
                None => self.focus = Focus::Side,
            },
            KeyCode::Char(' ') => self.toggle_pause(),
            KeyCode::Left | KeyCode::Char(',') => self.seek_by(-15.0),
            KeyCode::Right | KeyCode::Char('.') => self.seek_by(30.0),
            KeyCode::Char('n') => {
                if !self.play_next() {
                    self.status = "The queue is empty: a adds an episode, A plays one next.".into();
                }
            }
            KeyCode::Char('p') => self.seek_to(0.0),
            KeyCode::Char('[') => self.set_speed(-0.1),
            KeyCode::Char(']') => self.set_speed(0.1),
            KeyCode::Char('+') | KeyCode::Char('=') => self.set_volume(5),
            KeyCode::Char('-') => self.set_volume(-5),
            KeyCode::Char('a') => self.queue_selected(false),
            KeyCode::Char('A') => self.queue_selected(true),
            KeyCode::Char('m') => self.toggle_played(),
            KeyCode::Char('L') => self.toggle_notes(),
            KeyCode::Char('w') => self.jump_to_playing(),
            KeyCode::Char('t') => {
                self.prefs.theme = (self.prefs.theme + 1) % crate::theme::THEMES.len();
                crate::theme::set(self.prefs.theme);
                self.status = format!("Theme: {}", crate::theme::name(self.prefs.theme));
                let _ = store::save_prefs(&self.prefs);
            }
            KeyCode::Char('/') => {
                self.focus = Focus::Input;
                self.status.clear();
            }
            KeyCode::Char('R') => self.refresh(true),
            KeyCode::Char('D') => self.unsubscribe(),
            _ => {}
        }
    }

    /// Click a sidebar entry to open it, a row to select it (double-click plays), the progress
    /// bar to seek; the wheel scrolls the pane under the pointer. Returns whether to redraw,
    /// so pointer moves (sent on every motion while mouse capture is on) cost no frame.
    pub fn on_mouse(&mut self, m: MouseEvent) -> bool {
        let pos = Position::new(m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                if self.help_open || self.focus == Focus::Input {
                    return false;
                }
                if self.side_area.contains(pos) {
                    self.focus = Focus::Side;
                } else if self.main_area.contains(pos) {
                    self.focus = Focus::Main;
                }
                self.move_sel(if m.kind == MouseEventKind::ScrollDown { 1 } else { -1 });
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.help_open {
                    self.help_open = false;
                } else if self.focus == Focus::Input {
                    return false;
                } else if let Some(r) = row_at(self.side_area, 1, self.side_state.offset(), FIXED.len() + 1 + self.shows.len(), pos) {
                    // display rows: the fixed entries, the "Shows" divider, then the shows
                    let Some(i) = (match r {
                        r if r < FIXED.len() => Some(r),
                        r if r == FIXED.len() => None,
                        r => Some(r - 1),
                    }) else {
                        return false;
                    };
                    self.side_state.select(Some(i));
                    self.focus = Focus::Side;
                    self.open_side();
                } else if let Some(i) = row_at(self.main_area, 2, self.view.state.offset(), self.view.rows.len(), pos)
                    && self.notes.is_none()
                {
                    self.focus = Focus::Main;
                    self.view.state.select(Some(i));
                    let double = self.last_click.is_some_and(|(t, j)| j == i && t.elapsed() < Duration::from_millis(400));
                    self.last_click = (!double).then(|| (Instant::now(), i));
                    if double {
                        self.play_selected();
                    }
                } else if let Some(len) = self.now.as_ref().map(|n| n.duration).filter(|d| *d > 0.0)
                    && self.bar_area.contains(pos)
                {
                    self.seek_to((pos.x - self.bar_area.x) as f64 / self.bar_area.width.max(1) as f64 * len);
                } else {
                    return false;
                }
            }
            _ => return false,
        }
        true
    }
}

/// The list row under `pos` in a bordered pane whose rows start `top` lines down, scrolled by `offset`.
fn row_at(area: Rect, top: u16, offset: usize, len: usize, pos: Position) -> Option<usize> {
    if pos.x <= area.x || pos.x + 1 >= area.right() || pos.y < area.y + top || pos.y + 1 >= area.bottom() {
        return None;
    }
    let i = offset + (pos.y - area.y - top) as usize;
    (i < len).then_some(i)
}

/// GET with "only if changed" headers. Parsing (~20 ms for a big feed) and writing the show
/// notes happen off the async thread; the notes then leave memory until they're shown.
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
    let body = resp.bytes().await.map_err(cause)?;
    let url = url.to_string();
    let show = tokio::task::spawn_blocking(move || {
        let mut show = feed::parse(&body)?;
        let _ = store::save_notes(&url, &show.episodes);
        for e in &mut show.episodes {
            e.notes = String::new();
        }
        Ok::<_, String>(show)
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(Some(Cached { show, etag: new_etag, last_modified, fetched: unix_now() }))
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

    #[test]
    fn clicks_map_to_rows() {
        let area = Rect::new(10, 5, 20, 10); // rows at y 7..14, x 11..29
        let at = |x, y| Position::new(x, y);
        assert_eq!(row_at(area, 2, 0, 50, at(15, 7)), Some(0));
        assert_eq!(row_at(area, 2, 30, 50, at(15, 13)), Some(36));
        assert_eq!(row_at(area, 2, 0, 50, at(15, 6)), None, "header");
        assert_eq!(row_at(area, 2, 0, 50, at(15, 14)), None, "bottom border");
        assert_eq!(row_at(area, 2, 0, 3, at(15, 10)), None, "past the last row");
    }
}
