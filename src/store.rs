//! What hark saves: subscriptions (standard OPML), one cache file per feed, listening state for
//! the episodes you've touched, and preferences. Plain text, written atomically.

use std::{collections::HashMap, path::PathBuf};

use quick_xml::{Reader, events::Event};

use crate::{
    feed::{Episode, Extra, Show},
    paths::{cache_dir, data_dir, fnv, write_atomic},
};

/// A subscription: the feed url and the name to show until the feed itself has loaded.
#[derive(Clone, Debug, PartialEq)]
pub struct Sub {
    pub url: String,
    pub title: String,
}

fn subs_path() -> PathBuf {
    data_dir().join("subscriptions.opml")
}

pub fn load_subs() -> Vec<Sub> {
    std::fs::read(subs_path()).map(|b| parse_opml(&b)).unwrap_or_default()
}

pub fn save_subs(subs: &[Sub]) -> std::io::Result<()> {
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<opml version=\"2.0\">\n  <head><title>hark subscriptions</title></head>\n  <body>\n");
    for s in subs {
        x.push_str(&format!("    <outline type=\"rss\" text=\"{0}\" title=\"{0}\" xmlUrl=\"{1}\"/>\n", esc(&s.title), esc(&s.url)));
    }
    x.push_str("  </body>\n</opml>\n");
    write_atomic(&subs_path(), x.as_bytes())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Every `<outline xmlUrl=…>` at any depth (other apps nest them in folders).
pub fn parse_opml(xml: &[u8]) -> Vec<Sub> {
    let mut r = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out: Vec<Sub> = Vec::new();
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e) | Event::Empty(e)) if e.name().as_ref().eq_ignore_ascii_case("outline") => {
                let get = |k: &str| e.try_get_attribute(k).ok().flatten().and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok()).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
                if let Some(url) = get("xmlUrl").or_else(|| get("xmlurl"))
                    && !out.iter().any(|s| s.url == url)
                {
                    let title = get("title").or_else(|| get("text")).unwrap_or_else(|| url.clone());
                    out.push(Sub { url, title });
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// A feed as last fetched, plus what the server needs to answer "not modified".
#[derive(Default)]
pub struct Cached {
    pub show: Show,
    pub etag: String,
    pub last_modified: String,
    /// When it was fetched (Unix seconds).
    pub fetched: i64,
}

fn feed_path(url: &str) -> PathBuf {
    cache_dir().join("feeds").join(format!("{:016x}.tsv", fnv(url)))
}

/// Tabs and newlines would break the line format; titles never need them.
fn clean(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

// shortcut: show notes aren't cached (Phase 1 doesn't show them); they arrive with each refresh.
pub fn save_feed(url: &str, c: &Cached) -> std::io::Result<()> {
    let s = &c.show;
    let mut out = format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        clean(&c.etag),
        clean(&c.last_modified),
        c.fetched,
        clean(&s.title),
        clean(&s.author),
        clean(s.image.as_deref().unwrap_or("")),
        clean(&s.categories.join("|")),
        clean(&s.subcategories.join("|"))
    );
    for e in &s.episodes {
        // the 5th field (the file type) is no longer used; kept empty so the layout stays the same
        out.push_str(&format!("{}\t{}\t{}\t{}\t\t{}\n", clean(&e.guid), e.published, e.duration_s, clean(&e.url), clean(&e.title)));
    }
    write_atomic(&feed_path(url), out.as_bytes())
}

pub fn load_feed(url: &str) -> Option<Cached> {
    let text = std::fs::read_to_string(feed_path(url)).ok()?;
    let mut lines = text.lines();
    let h: Vec<&str> = lines.next()?.split('\t').collect();
    let field = |i: usize| h.get(i).copied().unwrap_or("").to_string();
    let list = |i: usize| field(i).split('|').filter(|c| !c.is_empty()).map(str::to_string).collect();
    let mut show = Show { title: field(3), author: field(4), image: Some(field(5)).filter(|s| !s.is_empty()), categories: list(6), subcategories: list(7), ..Default::default() };
    for l in lines {
        let f: Vec<&str> = l.splitn(6, '\t').collect();
        if let [guid, published, dur, url, _, title] = f[..] {
            show.episodes.push(Episode { guid: guid.into(), published: published.parse().unwrap_or(0), duration_s: dur.parse().unwrap_or(0), url: url.into(), title: title.into() });
        }
    }
    // a cache from before (sub)categories were kept (6 or 7 fields): forget its validators so
    // the next refresh fetches the whole feed once instead of getting "not modified"
    let old = h.len() < 8;
    let (etag, last_modified) = if old { (String::new(), String::new()) } else { (field(0), field(1)) };
    Some(Cached { show, etag, last_modified, fetched: field(2).parse().unwrap_or(0) })
}

pub fn forget_feed(url: &str) {
    let _ = std::fs::remove_file(feed_path(url));
    let _ = std::fs::remove_file(notes_path(url));
}

/// Listening state of one episode.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Listen {
    /// Seconds in; 0 = not started.
    pub pos: f64,
    pub played: bool,
    /// When it last changed (Unix seconds), for "In progress" order and smart resume.
    pub updated: i64,
    /// The real length the player measured (seconds; 0 = not played yet). Feeds' own
    /// durations are often off, more so with ads inserted on the fly.
    pub dur: f64,
}

fn state_path() -> PathBuf {
    data_dir().join("state.tsv")
}

/// guid → state, only for episodes you've touched.
pub fn load_state() -> HashMap<String, Listen> {
    let text = std::fs::read_to_string(state_path()).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let guid = f.next()?.to_string();
            let pos = f.next()?.parse().ok()?;
            let played = f.next()? == "1";
            let updated = f.next().and_then(|u| u.parse().ok()).unwrap_or(0);
            let dur = f.next().and_then(|d| d.parse().ok()).unwrap_or(0.0);
            Some((guid, Listen { pos, played, updated, dur }))
        })
        .collect()
}

pub fn save_state(state: &HashMap<String, Listen>) -> std::io::Result<()> {
    let mut out = String::with_capacity(state.len() * 48);
    for (g, l) in state {
        if l.played || l.pos > 0.0 {
            out.push_str(&format!("{}\t{:.1}\t{}\t{}\t{:.0}\n", clean(g), l.pos, l.played as u8, l.updated, l.dur));
        }
    }
    write_atomic(&state_path(), out.as_bytes())
}

/// Speed, volume and theme, remembered between runs.
pub struct Prefs {
    pub speed: f32,
    /// 0..=100
    pub volume: u8,
    pub theme: usize,
    /// Cover art in the sidebar (`i` toggles).
    pub art: bool,
}

pub fn load_prefs() -> Prefs {
    let text = std::fs::read_to_string(data_dir().join("prefs")).unwrap_or_default();
    let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')?.trim().parse::<f32>().ok());
    let theme = text.lines().find_map(|l| l.strip_prefix("theme=")).map_or(0, |t| crate::theme::by_name(t.trim()));
    let art = !text.lines().any(|l| l.trim() == "art=off");
    Prefs { speed: get("speed").unwrap_or(1.0).clamp(0.5, 3.0), volume: get("volume").unwrap_or(100.0).clamp(0.0, 100.0) as u8, theme, art }
}

pub fn save_prefs(p: &Prefs) -> std::io::Result<()> {
    let text = format!("speed={}\nvolume={}\ntheme={}\nart={}\n", p.speed, p.volume, crate::theme::name(p.theme), if p.art { "on" } else { "off" });
    write_atomic(&data_dir().join("prefs"), text.as_bytes())
}

/// The queue: (feed url, episode guid), in play order.
pub fn load_queue() -> Vec<(String, String)> {
    let text = std::fs::read_to_string(data_dir().join("queue.tsv")).unwrap_or_default();
    text.lines().filter_map(|l| l.split_once('\t')).map(|(u, g)| (u.to_string(), g.to_string())).collect()
}

pub fn save_queue<'a>(q: impl Iterator<Item = (&'a str, &'a str)>) -> std::io::Result<()> {
    let text: String = q.map(|(u, g)| format!("{}\t{}\n", clean(u), clean(g))).collect();
    write_atomic(&data_dir().join("queue.tsv"), text.as_bytes())
}

fn notes_path(url: &str) -> PathBuf {
    cache_dir().join("feeds").join(format!("{:016x}.notes", fnv(url)))
}

/// Show notes (with each episode's chapters and transcript links) live in their own file per
/// feed and are read one episode at a time, when needed: together they're most of a feed's
/// size (6 MB for The Daily). A line: guid, notes, chapters url, transcript url, its type,
/// the episode's own cover.
pub fn save_notes(url: &str, eps: &[Episode], extras: &[Extra]) -> std::io::Result<()> {
    let mut out = String::new();
    for (e, x) in eps.iter().zip(extras).filter(|(_, x)| **x != Extra::default()) {
        let (tr, tr_type) = x.transcript.as_ref().map_or(("", ""), |(u, m)| (u.as_str(), m.as_str()));
        let f = |o: &Option<String>| clean(o.as_deref().unwrap_or(""));
        out.push_str(&format!("{}\t{}\t{}\t{}\t{}\t{}\n", clean(&e.guid), escape_line(&x.notes), f(&x.chapters), clean(tr), clean(tr_type), f(&x.image)));
    }
    write_atomic(&notes_path(url), out.as_bytes())
}

pub fn has_notes(url: &str) -> bool {
    notes_path(url).exists()
}

pub fn load_notes(url: &str, guid: &str) -> Option<String> {
    Some(load_extra(url, guid)?.notes).filter(|n| !n.is_empty())
}

/// One episode's notes, chapters, transcript and cover, from the notes file.
pub fn load_extra(url: &str, guid: &str) -> Option<Extra> {
    let text = std::fs::read_to_string(notes_path(url)).ok()?;
    Some(extra_of(text.lines().find_map(|l| l.strip_prefix(guid)?.strip_prefix('\t'))?))
}

/// A notes line after its guid → its `Extra`. Notes are escaped, so tabs only separate fields
/// (older files have just the notes).
fn extra_of(line: &str) -> Extra {
    let f: Vec<&str> = line.split('\t').collect();
    let at = |i: usize| f.get(i).filter(|v| !v.is_empty()).map(|v| v.to_string());
    Extra { notes: f.first().map(|n| unescape_line(n)).unwrap_or_default(), chapters: at(1), transcript: at(2).map(|u| (u, at(3).unwrap_or_default())), image: at(4) }
}

/// Multi-line text on one line: `\` → `\\`, newline → `\n`, tab → `\t`.
fn escape_line(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\t', "\\t").replace('\r', "")
}

fn unescape_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match (c, c == '\\').1.then(|| it.next()).flatten() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(o) => out.push(o),
            None if c != '\\' => out.push(c),
            None => {}
        }
    }
    out
}

/// Settings for one show, by feed url (`show-prefs.tsv`: url, then `key=value` fields).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ShowPrefs {
    pub sort: crate::feed::Sort,
    /// Its own speed (`e`); None = your usual one.
    pub speed: Option<f32>,
    /// Seconds of intro to skip at the start of every episode (`I`); 0 = none.
    pub intro: f32,
    /// Seconds of outro to skip at the end (`E`); 0 = none.
    pub outro: f32,
    /// Keep this many of the newest episodes downloaded (`K`); 0 = none.
    pub keep: u8,
}

fn parse_show_prefs(text: &str) -> HashMap<String, ShowPrefs> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let url = f.next().filter(|u| !u.is_empty())?;
            let mut p = ShowPrefs::default();
            for (k, v) in f.filter_map(|kv| kv.split_once('=')) {
                match k {
                    "sort" => p.sort = crate::feed::Sort::parse(v).unwrap_or_default(),
                    "speed" => p.speed = v.parse::<f32>().ok().map(|s| s.clamp(0.5, 3.0)),
                    "intro" => p.intro = v.parse::<f32>().unwrap_or(0.0).max(0.0),
                    "outro" => p.outro = v.parse::<f32>().unwrap_or(0.0).max(0.0),
                    "keep" => p.keep = v.parse().unwrap_or(0),
                    _ => {}
                }
            }
            Some((url.to_string(), p))
        })
        .collect()
}

pub fn load_show_prefs() -> HashMap<String, ShowPrefs> {
    parse_show_prefs(&std::fs::read_to_string(data_dir().join("show-prefs.tsv")).unwrap_or_default())
}

/// Only shows with something other than the defaults get a line.
pub fn save_show_prefs<'a>(prefs: impl Iterator<Item = (&'a str, &'a ShowPrefs)>) -> std::io::Result<()> {
    let line = |u: &str, p: &ShowPrefs| {
        let mut l = format!("{}\tsort={}", clean(u), p.sort.id());
        if let Some(s) = p.speed {
            l.push_str(&format!("\tspeed={s}"));
        }
        if p.intro > 0.0 {
            l.push_str(&format!("\tintro={:.0}", p.intro));
        }
        if p.outro > 0.0 {
            l.push_str(&format!("\toutro={:.0}", p.outro));
        }
        if p.keep > 0 {
            l.push_str(&format!("\tkeep={}", p.keep));
        }
        l + "\n"
    };
    let text: String = prefs.filter(|(_, p)| **p != ShowPrefs::default()).map(|(u, p)| line(u, p)).collect();
    write_atomic(&data_dir().join("show-prefs.tsv"), text.as_bytes())
}

/// Feeds you said no to in For you (`x`), one url per line.
pub fn load_hidden() -> std::collections::HashSet<String> {
    std::fs::read_to_string(data_dir().join("not-interested.txt")).unwrap_or_default().lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

pub fn save_hidden(h: &std::collections::HashSet<String>) -> std::io::Result<()> {
    let mut v: Vec<&String> = h.iter().collect();
    v.sort();
    let text: String = v.iter().map(|u| format!("{}\n", clean(u))).collect();
    write_atomic(&data_dir().join("not-interested.txt"), text.as_bytes())
}

/// Downloaded episodes: guid → (feed url, file name in the downloads folder).
pub fn load_downloads() -> HashMap<String, (String, String)> {
    let text = std::fs::read_to_string(data_dir().join("downloads.tsv")).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (g, u, file) = (f.next()?, f.next()?, f.next()?);
            // a file deleted by hand is just gone
            crate::download::dir().join(file).exists().then(|| (g.to_string(), (u.to_string(), file.to_string())))
        })
        .collect()
}

pub fn save_downloads(d: &HashMap<String, (String, String)>) -> std::io::Result<()> {
    let text: String = d.iter().map(|(g, (u, f))| format!("{}\t{}\t{}\n", clean(g), clean(u), clean(f))).collect();
    write_atomic(&data_dir().join("downloads.tsv"), text.as_bytes())
}

/// `hark --export`: your shows as OPML on stdout, for any other podcast app.
pub fn export() -> Result<(), String> {
    use std::io::Write;
    let path = subs_path();
    let opml = std::fs::read(&path).map_err(|e| format!("{}: {e} (no shows followed yet?)", path.display()))?;
    std::io::stdout().write_all(&opml).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_lines_old_and_new() {
        assert_eq!(extra_of("line one\\nline\\ttwo"), Extra { notes: "line one\nline\ttwo".into(), ..Default::default() }, "older files: just the notes");
        let x = extra_of("hi\thttps://c.json\thttps://t.vtt\ttext/vtt\thttps://cover.jpg");
        assert_eq!((x.notes.as_str(), x.chapters.as_deref(), x.image.as_deref()), ("hi", Some("https://c.json"), Some("https://cover.jpg")));
        assert_eq!(x.transcript, Some(("https://t.vtt".into(), "text/vtt".into())));
        let x = extra_of("\thttps://c.json\t\t\thttps://cover.jpg");
        assert_eq!((x.notes.as_str(), x.chapters.as_deref(), x.transcript, x.image.as_deref()), ("", Some("https://c.json"), None, Some("https://cover.jpg")), "empty fields keep their place");
    }

    #[test]
    fn show_prefs_round_trip() {
        let p = parse_show_prefs("https://a/feed\tsort=oldest\tspeed=1.3\tintro=65\toutro=40\nhttps://b/feed\tsort=bogus\tfuture=1\n\n");
        assert_eq!(p["https://a/feed"], ShowPrefs { sort: crate::feed::Sort::Oldest, speed: Some(1.3), intro: 65.0, outro: 40.0, keep: 0 });
        assert_eq!(p["https://b/feed"].sort, crate::feed::Sort::Newest, "unknown values fall back");
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn opml_from_other_apps() {
        let x = br#"<?xml version="1.0"?><opml version="1.0"><body>
          <outline text="feeds">
            <outline type="rss" text="A &amp; B" xmlUrl="https://a.example/feed?x=1&amp;y=2"/>
            <outline text="no title attr" xmlUrl="https://b.example/rss"></outline>
            <outline text="dupe" xmlUrl="https://b.example/rss"/>
            <outline text="a folder without a feed"/>
          </outline></body></opml>"#;
        let subs = parse_opml(x);
        assert_eq!(subs, [Sub { url: "https://a.example/feed?x=1&y=2".into(), title: "A & B".into() }, Sub { url: "https://b.example/rss".into(), title: "no title attr".into() }]);
    }

    #[test]
    fn notes_escape_round_trip() {
        for s in ["plain", "two\nlines", "tab\there", "back\\slash\\n not a newline", "", "end\\"] {
            let e = escape_line(s);
            assert!(!e.contains('\n') && !e.contains('\t'), "{e:?}");
            assert_eq!(unescape_line(&e), s);
        }
    }

    #[test]
    fn opml_round_trip() {
        let subs = vec![Sub { url: "https://x.example/f?a=1&b=\"2\"".into(), title: "Tom & <Jerry>".into() }];
        let mut x = String::new();
        for s in &subs {
            x.push_str(&format!("<outline text=\"{}\" xmlUrl=\"{}\"/>", esc(&s.title), esc(&s.url)));
        }
        assert_eq!(parse_opml(format!("<opml><body>{x}</body></opml>").as_bytes()), subs);
    }
}
