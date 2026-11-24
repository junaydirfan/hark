//! What's on screen, what's playing, and what each key does.

use std::{
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Position, Rect, Size},
    widgets::{ListState, TableState},
};
use ratatui_image::{FilterType, Resize, picker::Picker, protocol::Protocol};
use tokio::sync::{Semaphore, mpsc::UnboundedSender};

use crate::{
    discover::{self, Card},
    engine::{Cmd, Engine, Event},
    feed::{self, Episode, Show},
    mpris::Remote,
    store::{self, Cached, Listen, Prefs, Sub},
};

/// A feed url, shared by every row that points into that feed.
pub type Url = Rc<str>;

/// Sidebar entries above the shows.
pub const FIXED: [&str; 4] = ["Inbox", "In progress", "Queue", "Discover"];
/// Shows looked at but not followed are kept this many at a time.
const PREVIEWS: usize = 8;
/// Feeds are refreshed at launch when older than this, and on `R`.
const STALE_S: i64 = 3600;
/// While playing, the position is saved this often (also on pause, seek, switch, quit, and when
/// the terminal closes), so a crash or a pulled plug loses at most this much.
const SAVE_EVERY_S: f64 = 10.0;
/// Stopping this close to the end (the outro, usually) counts as finished.
const NEAR_END_S: f64 = 30.0;

/// Whether stopping at `pos` of `dur` means the episode is done.
fn finished(pos: f64, dur: f64) -> bool {
    dur > 0.0 && pos >= dur - NEAR_END_S
}

/// Smart resume: how far to go back when you come back to an episode, so you hear the end of
/// the sentence again. The longer you were away, the more context you need.
pub fn rewind_for(away_s: i64) -> f64 {
    match away_s {
        ..60 => 0.0,
        60..3600 => 3.0,
        3600..86400 => 10.0,
        _ => 20.0,
    }
}
/// The Inbox: unplayed episodes from the last 30 days, newest first.
const INBOX_DAYS: i64 = 30;
const INBOX_MAX: usize = 300;

pub enum Msg {
    /// A feed fetch finished: Some = new content, None = not modified.
    Feed { url: String, res: Result<Option<Cached>, String>, adding: bool },
    /// A show opened from Discover or Search, before following it.
    Preview { url: String, res: Result<Option<Cached>, String> },
    /// Directory results for a cache key ("top:0", "foryou", "search:<q>").
    Cards { key: String, res: Result<Vec<Card>, String> },
    /// A newer hark release is out (its tag).
    Update(String),
    /// A cover, encoded for the terminal at `size` cells.
    Art { url: String, size: (u16, u16), res: Result<Protocol, String> },
}

/// The playing episode's cover in the sidebar.
pub struct Art {
    pub url: String,
    pub size: (u16, u16),
    /// None while it's being fetched / encoded.
    pub proto: Option<Protocol>,
    busy: bool,
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
    Discover,
    Search(String),
    Show(Url),
}

/// A row in the main table.
#[derive(Clone)]
pub enum Item {
    Ep { show: Url, i: usize },
    Show(Card),
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
    /// The episode's own artwork, else the show's.
    pub art: Option<String>,
    saved_at: f64,
    /// When it was paused, for smart resume on un-pause.
    paused_at: Option<Instant>,
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
    /// A newer release's tag, for the bottom line.
    pub update: Option<String>,
    /// Per-show settings (episode order), by feed url.
    show_prefs: HashMap<String, store::ShowPrefs>,
    /// Shows opened from Discover or Search, not followed.
    previews: Vec<ShowView>,
    /// Directory results this session, by key; and the keys being fetched.
    cards: HashMap<String, Vec<Card>>,
    cards_loading: Vec<String>,
    /// Discover's genre: 0 = all podcasts, else 1 + an index into GENRES.
    pub genre: usize,
    country: String,
    /// How the terminal draws images (kitty / sixel / iTerm2 / half-blocks); None = no images.
    pub img_picker: Option<Picker>,
    pub art: Option<Art>,
    /// Cells the sidebar has room for (set while drawing).
    pub art_want: Option<(u16, u16)>,
    /// Where the last frame drew the sidebar list, the table and the progress bar (for clicks).
    pub side_area: Rect,
    pub main_area: Rect,
    pub bar_area: Rect,
    last_click: Option<(Instant, usize)>,
    engine: Engine,
    tx: UnboundedSender<Msg>,
    pub client: reqwest::Client,
    fetches: Arc<Semaphore>,
    confirm_remove: Option<(String, Instant)>,
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Newest episode first (feeds usually are; not all of them).
fn sort_episodes(show: &mut Show) {
    feed::sort(&mut show.episodes, feed::Sort::Newest);
}

impl App {
    pub fn new(engine: Engine, tx: UnboundedSender<Msg>, img_picker: Option<Picker>) -> App {
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
            update: None,
            show_prefs: store::load_show_prefs(),
            previews: Vec::new(),
            cards: HashMap::new(),
            cards_loading: Vec::new(),
            genre: 0,
            country: discover::country(),
            img_picker,
            art: None,
            art_want: None,
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

    /// A followed show, or one being previewed.
    pub fn show(&self, url: &str) -> Option<&ShowView> {
        self.shows.iter().chain(&self.previews).find(|s| *s.url == *url)
    }

    pub fn is_followed(&self, url: &str) -> bool {
        self.shows.iter().any(|s| *s.url == *url)
    }

    /// Apple's feed address isn't always the one you subscribed with (The Daily has two), so
    /// a directory card also counts as followed when the title matches.
    pub fn follows(&self, c: &Card) -> Option<Url> {
        self.shows.iter().find(|s| s.sub.url == c.feed || s.show.title.eq_ignore_ascii_case(&c.title)).map(|s| s.url.clone())
    }

    pub fn episode(&self, item: &Item) -> Option<(&ShowView, &Episode)> {
        let Item::Ep { show, i } = item else { return None };
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

    /// Whether directory results are still on their way.
    pub fn cards_pending(&self) -> bool {
        !self.cards_loading.is_empty()
    }

    /// The genre Discover shows: None = all podcasts.
    pub fn genre_name(&self) -> Option<&'static str> {
        self.genre.checked_sub(1).map(|i| discover::GENRES[i].0)
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
            Kind::Discover => {
                let top = format!("top:{}", self.genre);
                let keys = if self.genre == 0 { vec!["foryou".to_string(), top] } else { vec![top] };
                keys.iter().flat_map(|k| self.cards.get(k).into_iter().flatten()).cloned().map(Item::Show).collect()
            }
            Kind::Search(q) => self.cards.get(&format!("search:{q}")).into_iter().flatten().cloned().map(Item::Show).collect(),
            Kind::Show(u) => self.show(u).map_or(Vec::new(), |s| {
                let mut idx: Vec<usize> = (0..s.show.episodes.len()).collect();
                let sort = self.sort_of(u);
                if sort != feed::Sort::Newest {
                    idx.sort_by_key(|&i| sort.key(&s.show.episodes[i]));
                }
                idx.into_iter().map(|i| Item::Ep { show: u.clone(), i }).collect()
            }),
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
        self.want_cards();
        let rows = self.rows_for(&self.view.kind);
        let n = rows.len();
        self.view.title = match &self.view.kind {
            Kind::Inbox => format!("Inbox · {n} new in the last {INBOX_DAYS} days"),
            Kind::InProgress => format!("In progress · {n}"),
            Kind::Queue => format!("Queue · {n}"),
            Kind::Discover => {
                let what = match self.genre_name() {
                    Some(g) => format!("top {g} podcasts"),
                    None if self.cards.get("foryou").is_some_and(|c| !c.is_empty()) => "for you, then the top podcasts".into(),
                    None => "the top podcasts".into(),
                };
                let busy = if self.cards_loading.is_empty() { "" } else { " (loading…)" };
                format!("Discover · {what} ({}){busy} · o/O genre · f follow", self.country.to_uppercase())
            }
            Kind::Search(q) => {
                let busy = if self.cards_loading.iter().any(|k| *k == format!("search:{q}")) { " (searching…)" } else { "" };
                format!("Search · “{q}” · {n} shows{busy} · Enter open · f follow")
            }
            Kind::Show(u) => match self.show(u) {
                Some(s) => {
                    let mut t = s.show.title.clone();
                    if !s.show.categories.is_empty() {
                        t.push_str(&format!(" · {}", s.show.categories.join(", ")));
                    }
                    t.push_str(&format!(" · {n} episodes, {} (s)", self.sort_of(u).name()));
                    if !self.is_followed(u) {
                        t.push_str(" · not followed: f to follow");
                    }
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
        self.back.clear();
        self.push(kind);
    }

    /// Open a view on top of the current one; Esc comes back.
    fn push(&mut self, kind: Kind) {
        self.notes = None;
        let v = View { kind, title: String::new(), rows: Vec::new(), state: TableState::default().with_selected(Some(0)) };
        let old = std::mem::replace(&mut self.view, v);
        if !old.rows.is_empty() || old.kind != self.view.kind {
            self.back.push(old);
        }
        self.back.truncate(20);
        self.rebuild();
        self.focus = Focus::Main;
    }

    // ---------------------------------------------------------------- discover

    /// Fetch the directory results the open view needs, once per session.
    fn want_cards(&mut self) {
        let keys: Vec<String> = match &self.view.kind {
            Kind::Discover if self.genre == 0 => vec!["foryou".into(), "top:0".into()],
            Kind::Discover => vec![format!("top:{}", self.genre)],
            _ => return,
        };
        for k in keys {
            self.fetch_cards(k);
        }
    }

    fn fetch_cards(&mut self, key: String) {
        if self.cards.contains_key(&key) || self.cards_loading.contains(&key) {
            return;
        }
        let (client, tx, cc) = (self.client.clone(), self.tx.clone(), self.country.clone());
        if key == "foryou" {
            // your genres, most common first, from the categories your feeds declare
            let mut count: HashMap<u32, usize> = HashMap::new();
            for s in &self.shows {
                for c in &s.show.categories {
                    if let Some(g) = discover::genre_id(c) {
                        *count.entry(g).or_default() += 1;
                    }
                }
            }
            let mut genres: Vec<(u32, usize)> = count.into_iter().collect();
            genres.sort_by_key(|(g, n)| (std::cmp::Reverse(*n), *g));
            if genres.is_empty() {
                self.cards.insert(key, Vec::new());
                return;
            }
            let genres: Vec<u32> = genres.into_iter().map(|(g, _)| g).collect();
            let follows: Vec<(String, String)> = self.shows.iter().map(|s| (s.sub.url.clone(), s.show.title.to_lowercase())).collect();
            self.cards_loading.push(key.clone());
            tokio::spawn(async move {
                let followed = |c: &Card| follows.iter().any(|(u, t)| *u == c.feed || *t == c.title.to_lowercase());
                let res = discover::for_you(&client, &cc, &genres, &followed).await;
                let _ = tx.send(Msg::Cards { key, res });
            });
            return;
        }
        self.cards_loading.push(key.clone());
        tokio::spawn(async move {
            let res = match key.split_once(':') {
                Some(("top", g)) => discover::top(&client, &cc, g.parse::<usize>().ok().and_then(|g| g.checked_sub(1)).map(|i| discover::GENRES[i].1)).await,
                Some(("search", q)) => discover::search(&client, q, &cc).await,
                _ => Ok(Vec::new()),
            };
            let _ = tx.send(Msg::Cards { key, res });
        });
    }

    fn search(&mut self, q: String) {
        self.cards.remove(&format!("search:{q}")); // a new search asks again
        self.fetch_cards(format!("search:{q}"));
        self.push(Kind::Search(q));
    }

    fn sort_of(&self, url: &str) -> feed::Sort {
        self.show_prefs.get(url).map_or_default(|p| p.sort)
    }

    /// `s` in a show: the next episode order, remembered for that show.
    fn cycle_sort(&mut self) {
        let Kind::Show(u) = &self.view.kind else { return };
        let p = self.show_prefs.entry(u.to_string()).or_default();
        p.sort = p.sort.next();
        let _ = store::save_show_prefs(self.show_prefs.iter().map(|(u, p)| (u.as_str(), p)));
        self.view.state.select(Some(0));
        self.rebuild();
    }

    /// `o` / `O` in Discover: the next / previous genre.
    fn cycle_genre(&mut self, delta: isize) {
        if self.view.kind != Kind::Discover {
            return;
        }
        let n = discover::GENRES.len() as isize + 1;
        self.genre = (self.genre as isize + delta).rem_euclid(n) as usize;
        self.view.state.select(Some(0));
        self.rebuild();
    }

    /// Enter on a show card: its episodes, followed or not.
    fn open_card(&mut self, c: &Card) {
        if let Some(url) = self.follows(c) {
            return self.push(Kind::Show(url));
        }
        if !self.previews.iter().any(|p| p.sub.url == c.feed) {
            let placeholder = Show { title: c.title.clone(), ..Default::default() };
            self.previews.push(ShowView { sub: Sub { url: c.feed.clone(), title: c.title.clone() }, url: c.feed.as_str().into(), show: placeholder, etag: String::new(), last_modified: String::new(), fetched: 0, loading: true, error: None });
            let (client, tx, url) = (self.client.clone(), self.tx.clone(), c.feed.clone());
            tokio::spawn(async move {
                let res = fetch_feed(&client, &url, "", "").await;
                let _ = tx.send(Msg::Preview { url, res });
            });
        }
        let url = self.previews.iter().find(|p| p.sub.url == c.feed).map(|p| p.url.clone());
        if let Some(url) = url {
            self.push(Kind::Show(url));
        }
    }

    /// `f`: follow the highlighted show (or the previewed one that's open).
    fn follow(&mut self) {
        let target: Option<String> = match (self.selected_item(), &self.view.kind) {
            (Some(Item::Show(c)), _) if self.focus == Focus::Main => {
                if self.follows(c).is_some() {
                    self.status = format!("You already follow {}. (D D unfollows it from its page.)", c.title);
                    return;
                }
                Some(c.feed.clone())
            }
            (_, Kind::Show(u)) if !self.is_followed(u) => Some(u.to_string()),
            (_, Kind::Show(_)) => {
                self.status = "You already follow this show. D D unfollows it.".into();
                return;
            }
            _ => None,
        };
        let Some(url) = target else {
            self.status = "Highlight a show in Discover or Search to follow it.".into();
            return;
        };
        match self.previews.iter().position(|p| p.sub.url == url && !p.loading && p.error.is_none()) {
            Some(i) => {
                let p = self.previews.remove(i);
                let title = p.show.title.clone();
                let c = Cached { show: p.show, etag: p.etag, last_modified: p.last_modified, fetched: p.fetched };
                let _ = store::save_feed(&url, &c);
                self.shows.push(ShowView { sub: Sub { url: url.clone(), title: title.clone() }, url: p.url, show: c.show, etag: c.etag, last_modified: c.last_modified, fetched: c.fetched, loading: false, error: None });
                self.save_subs();
                self.sort_shows();
                self.cards.remove("foryou"); // your genres changed
                self.status = format!("Following {title}.");
            }
            None => {
                self.status = "Following…".into();
                self.fetch(url, String::new(), String::new(), true);
            }
        }
        self.rebuild();
    }

    fn open_side(&mut self) {
        let Some(i) = self.side_state.selected() else { return };
        let kind = match i {
            0 => Kind::Inbox,
            1 => Kind::InProgress,
            2 => Kind::Queue,
            3 => Kind::Discover,
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

    /// Look for a newer hark release (GitHub, at most once a day) for the bottom line.
    pub fn check_update(&self) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Some(tag) = crate::update::check().await {
                let _ = tx.send(Msg::Update(tag));
            }
        });
    }

    pub fn on_msg(&mut self, msg: Msg) {
        let (url, res, adding) = match msg {
            Msg::Feed { url, res, adding } => (url, res, adding),
            Msg::Preview { url, res } => return self.on_preview(url, res),
            Msg::Update(tag) => {
                self.update = Some(tag);
                return;
            }
            Msg::Art { url, size, res } => {
                if let Some(a) = self.art.as_mut().filter(|a| a.url == url) {
                    a.busy = false;
                    match res {
                        Ok(p) if a.size == size => a.proto = Some(p),
                        Ok(_) => {} // the room changed meanwhile: the next draw asks again
                        Err(_) => a.proto = None,
                    }
                }
                // podcast covers are often 3000 px: decoding one allocates tens of MB, freed now
                crate::trim_heap();
                return;
            }
            Msg::Cards { key, res } => {
                self.cards_loading.retain(|k| *k != key);
                match res {
                    Ok(c) => {
                        self.cards.insert(key, c);
                    }
                    Err(e) => self.status = format!("Apple Podcasts didn't answer: {e}"),
                }
                return self.rebuild();
            }
        };
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

    fn on_preview(&mut self, url: String, res: Result<Option<Cached>, String>) {
        let Some(p) = self.previews.iter_mut().find(|p| p.sub.url == url) else { return };
        p.loading = false;
        match res {
            Ok(Some(mut c)) => {
                c.show.episodes = c.show.episodes.to_vec();
                sort_episodes(&mut c.show);
                (p.show, p.etag, p.last_modified, p.fetched) = (c.show, c.etag, c.last_modified, c.fetched);
            }
            Ok(None) => {}
            Err(e) => {
                let e = if e.starts_with("XML error") { "the show's feed is broken".to_string() } else { e };
                p.error = Some(e);
            }
        }
        // keep the newest few, but never the one on screen
        while self.previews.len() > PREVIEWS {
            let on_screen = |p: &ShowView| self.view.kind == Kind::Show(p.url.clone()) || self.back.iter().any(|v| v.kind == Kind::Show(p.url.clone()));
            match self.previews.iter().position(|p| !on_screen(p)) {
                Some(i) => drop(self.previews.remove(i)),
                None => break,
            }
        }
        crate::trim_heap();
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

    /// `/`: a feed url subscribes, an OPML file imports, anything else searches.
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
        let path: std::path::PathBuf = match t.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME").map_or(t.into(), |h| std::path::Path::new(&h).join(rest)),
            None => t.into(),
        };
        // not an address and not a file: it's a search
        if !path.is_file() {
            return self.search(t.to_string());
        }
        let subs = match std::fs::read(&path) {
            Ok(b) => store::parse_opml(&b),
            Err(e) => {
                self.status = format!("{}: {e}", path.display());
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
        let art = e.image.clone().or_else(|| s.show.image.clone());
        self.remember();
        let l = self.state.get(&guid).copied().unwrap_or_default();
        let dur = if l.dur > 0.0 { l.dur } else { dur };
        let back = rewind_for(unix_now() - l.updated);
        let at = if l.played || l.pos <= 0.0 || finished(l.pos, dur) { 0.0 } else { (l.pos - back).max(0.0) };
        self.engine.send(Cmd::Load { src: url, at_s: at });
        self.status = match (at > 0.0, back > 0.0) {
            (false, _) => String::new(),
            (true, false) => format!("Resuming at {}.", fmt_s(at)),
            (true, true) => format!("Resuming at {} ({back:.0} s back, for context).", fmt_s(at)),
        };
        if let Some(q) = self.queue.iter().position(|(u, g)| *u == show_url && *g == guid) {
            self.queue.remove(q);
            self.save_queue();
        }
        self.now = Some(Now { show_url, guid: guid.clone(), title, show, duration: dur, pos: at, playing: true, buffering: false, art, saved_at: at, paused_at: None });
        // a replayed episode starts unplayed again
        let l = self.state.entry(guid).or_default();
        (l.played, l.pos, l.updated) = (false, at, unix_now());
        self.save_state();
        self.rebuild();
    }

    /// Enter on a row: play an episode, open a show.
    fn play_selected(&mut self) {
        match self.selected_item().cloned() {
            Some(Item::Show(c)) => self.open_card(&c),
            Some(it) => self.play(&it),
            None => {}
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
        if n.playing {
            // back from a long pause: a few seconds of context, as when reopening an episode
            let back = n.paused_at.take().map_or(0.0, |t| rewind_for(t.elapsed().as_secs() as i64));
            if back > 0.0 {
                n.pos = (n.pos - back).max(0.0);
                self.engine.send(Cmd::Seek(n.pos));
            }
        } else {
            n.paused_at = Some(Instant::now());
        }
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
        self.set_speed_to(self.prefs.speed + delta);
    }

    fn set_speed_to(&mut self, x: f32) {
        self.prefs.speed = (x * 10.0).round().clamp(5.0, 30.0) / 10.0;
        self.engine.send(Cmd::Speed(self.prefs.speed));
        let _ = store::save_prefs(&self.prefs);
    }

    fn set_volume(&mut self, delta: i16) {
        self.set_volume_to((self.prefs.volume as i16 + delta).clamp(0, 100) as u8);
    }

    fn set_volume_to(&mut self, v: u8) {
        self.prefs.volume = v.min(100);
        self.engine.send(Cmd::Volume(self.prefs.volume as f32 / 100.0));
        let _ = store::save_prefs(&self.prefs);
    }

    /// Save where the playing episode is. Within the last 30 s it counts as played; seeking back
    /// out of that makes it unplayed again.
    fn remember(&mut self) {
        let Some(n) = &mut self.now else { return };
        n.saved_at = n.pos;
        let done = finished(n.pos, n.duration);
        let l = self.state.entry(n.guid.clone()).or_default();
        (l.played, l.pos, l.updated) = (done, if done { 0.0 } else { n.pos }, unix_now());
        if n.duration > 0.0 {
            l.dur = n.duration;
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
        let guid = e.guid.clone();
        let l = self.state.entry(guid.clone()).or_default();
        (l.pos, l.played, l.updated) = (0.0, !l.played, unix_now());
        let played = l.played;
        self.status = if played { "Marked played.".into() } else { "Marked unplayed.".into() };
        // marking the playing episode played means you're done with it: stop it here, or the
        // next position save would make it unplayed again
        if let Some(n) = self.now.as_mut().filter(|n| played && n.guid == guid && n.playing) {
            n.playing = false;
            self.engine.send(Cmd::Pause(true));
        }
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
                    self.state.entry(n.guid.clone()).or_default().dur = duration_s;
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
                self.state.insert(n.guid.clone(), Listen { pos: 0.0, played: true, updated: unix_now(), dur: n.duration });
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

    /// Media keys, the desktop's media widget, playerctl.
    pub fn on_remote(&mut self, r: Remote) {
        let playing = self.now.as_ref().is_some_and(|n| n.playing);
        match r {
            Remote::PlayPause => self.toggle_pause(),
            Remote::Play if !playing => self.toggle_pause(),
            Remote::Pause if playing => self.toggle_pause(),
            Remote::Play | Remote::Pause => {}
            Remote::Next => {
                if !self.play_next() {
                    self.status = "The queue is empty.".into();
                }
            }
            Remote::Prev => self.seek_to(0.0),
            Remote::Seek(d) => self.seek_by(d),
            Remote::SetPosition(s) => self.seek_to(s),
            Remote::Volume(v) => self.set_volume_to(v),
            Remote::Rate(r) => self.set_speed_to(r),
        }
    }

    // ---------------------------------------------------------------- art

    /// `i`: the cover in the sidebar on / off.
    fn toggle_art(&mut self) {
        if self.img_picker.is_none() {
            self.status = "This terminal can't show images here.".into();
            return;
        }
        self.prefs.art = !self.prefs.art;
        if !self.prefs.art {
            self.art = None; // free the encoded image
        }
        let _ = store::save_prefs(&self.prefs);
    }

    /// Zooming changes the font's pixel size, which iTerm2 and sixel images are drawn in: pick
    /// up the new size and re-encode the cover. One ioctl per resize.
    pub fn on_resize(&mut self) {
        let (Some(p), Ok(ws)) = (self.img_picker.as_ref(), crossterm::terminal::window_size()) else { return };
        if ws.width == 0 || ws.height == 0 || ws.columns == 0 || ws.rows == 0 {
            return;
        }
        let (w, h) = (ws.width / ws.columns, ws.height / ws.rows);
        let old = p.font_size();
        if (w, h) == (old.width, old.height) {
            return;
        }
        // The query-built picker can't change its font size; from_fontsize is the only way.
        #[allow(deprecated)]
        let mut np = Picker::from_fontsize(ratatui_image::FontSize::new(w, h));
        np.set_protocol_type(p.protocol_type());
        self.img_picker = Some(np);
        if let Some(a) = self.art.as_mut() {
            a.size = (0, 0); // no longer matches art_want, so refresh_media re-encodes
        }
    }

    /// Fetch and encode the playing episode's cover if the sidebar wants one it doesn't have.
    /// Called after each draw (the draw says how much room there is); cheap when nothing changed.
    pub fn refresh_media(&mut self) {
        let Some(url) = self.now.as_ref().and_then(|n| n.art.clone()) else {
            self.art = None;
            return;
        };
        let (Some(picker), Some(size), true) = (self.img_picker.as_ref(), self.art_want, self.prefs.art) else { return };
        if self.art.as_ref().is_some_and(|a| a.url == url && (a.size == size || a.busy)) {
            return;
        }
        // keep showing the old cover until the new one is ready (no flicker)
        let keep = self.art.take().and_then(|a| a.proto);
        self.art = Some(Art { url: url.clone(), size, proto: keep, busy: true });
        let (client, tx, picker) = (self.client.clone(), self.tx.clone(), picker.clone());
        tokio::spawn(async move {
            let bytes = async { client.get(&url).send().await?.error_for_status()?.bytes().await }.await.map_err(cause);
            let res = match bytes {
                Ok(b) => tokio::task::spawn_blocking(move || encode_art(&picker, &b, size)).await.unwrap_or_else(|e| Err(e.to_string())),
                Err(e) => Err(e),
            };
            let _ = tx.send(Msg::Art { url, size, res });
        });
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
                Some(v) => {
                    self.view = v;
                    self.rebuild();
                }
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
            KeyCode::Char('i') => self.toggle_art(),
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
            KeyCode::Char('f') => self.follow(),
            KeyCode::Char('s') => self.cycle_sort(),
            KeyCode::Char('o') => self.cycle_genre(1),
            KeyCode::Char('O') => self.cycle_genre(-1),
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

/// Decode a cover and encode it for the terminal at `size` cells (on a blocking thread).
fn encode_art(picker: &Picker, bytes: &[u8], size: (u16, u16)) -> Result<Protocol, String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    // Shrink with a cheap filter first: covers are up to 3000 px, the target a few hundred.
    let fs = picker.font_size();
    let (w, h) = (size.0 as u32 * fs.width as u32, size.1 as u32 * fs.height as u32);
    let img = if img.width() > w * 2 || img.height() > h * 2 { img.thumbnail(w * 2, h * 2) } else { img };
    picker.new_protocol(img, Size::new(size.0, size.1), Resize::Fit(Some(FilterType::Triangle))).map_err(|e| e.to_string())
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
    fn progress_rules() {
        assert_eq!(rewind_for(30), 0.0, "a quick pause: no rewind");
        assert_eq!(rewind_for(10 * 60), 3.0);
        assert_eq!(rewind_for(5 * 3600), 10.0);
        assert_eq!(rewind_for(3 * 86400), 20.0);
        assert!(finished(1790.0, 1800.0) && finished(1770.0, 1800.0), "the last 30 s count as finished");
        assert!(!finished(1700.0, 1800.0));
        assert!(!finished(5000.0, 0.0), "unknown length: never auto-finished");
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
