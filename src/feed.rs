//! Podcast RSS: one streaming pass over the XML (no DOM), keeping only what hark shows or plays.

use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};

#[derive(Debug, Default)]
pub struct Show {
    pub title: String,
    pub author: String,
    pub image: Option<String>,
    /// `itunes:new-feed-url`: the show moved; subscribe there from now on.
    pub new_url: Option<String>,
    pub episodes: Vec<Episode>,
}

#[derive(Debug, Default, Clone)]
pub struct Episode {
    /// The feed's guid, or the audio url when there is none.
    pub guid: String,
    pub title: String,
    /// Seconds since the Unix epoch; 0 = unknown.
    pub published: i64,
    /// 0 = unknown (filled in once played).
    pub duration_s: u32,
    pub url: String,
    pub mime: String,
    pub bytes: u64,
    pub image: Option<String>,
    /// Show notes (HTML as the feed gives it).
    pub notes: String,
    /// Podcasting 2.0 `podcast:chapters` (JSON).
    pub chapters: Option<String>,
    /// Podcasting 2.0 `podcast:transcript`: (url, mime), the most useful kind on offer.
    pub transcript: Option<(String, String)>,
}

// shortcut: tags are matched by their usual prefixes (itunes:, podcast:, content:), not by
// resolved namespace; a feed that binds those namespaces to other prefixes loses these fields.
pub fn parse(xml: &[u8]) -> Result<Show, String> {
    let mut r = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut path: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut show = Show::default();
    let mut ep: Option<Episode> = None;
    loop {
        let ev = r.read_event_into(&mut buf).map_err(|e| format!("XML error at byte {}: {e}", r.buffer_position()))?;
        match ev {
            Event::Start(e) => {
                attrs(&e, &path, &mut show, &mut ep);
                if e.name().as_ref() == "item" {
                    ep = Some(Episode::default());
                }
                path.push(e.name().as_ref().to_string());
                text.clear();
            }
            Event::Empty(e) => attrs(&e, &path, &mut show, &mut ep),
            Event::Text(t) => text.push_str(&t.xml10_content()),
            Event::CData(t) => text.push_str(&t.xml10_content()),
            Event::GeneralRef(g) => match g.resolve_char_ref() {
                Ok(Some(c)) => text.push(c),
                _ => text.push_str(match g.as_ref() {
                    "amp" => "&",
                    "lt" => "<",
                    "gt" => ">",
                    "quot" => "\"",
                    "apos" => "'",
                    _ => "",
                }),
            },
            Event::End(_) => {
                let name = path.pop().unwrap_or_default();
                let parent = path.last().map(String::as_str);
                let t = text.trim();
                match (parent, name.as_str(), ep.as_mut()) {
                    (Some("item"), "title", Some(e)) => e.title = t.to_string(),
                    (Some("item"), "guid", Some(e)) => e.guid = t.to_string(),
                    (Some("item"), "pubDate", Some(e)) => e.published = rfc2822(t).unwrap_or(0),
                    (Some("item"), "itunes:duration", Some(e)) => e.duration_s = duration(t),
                    // description is the usual home of the notes; the others only fill a gap
                    (Some("item"), "description", Some(e)) => e.notes = t.to_string(),
                    (Some("item"), "content:encoded" | "itunes:summary", Some(e)) if e.notes.is_empty() => e.notes = t.to_string(),
                    (Some("channel"), "title", None) => show.title = t.to_string(),
                    (Some("channel"), "itunes:author", None) => show.author = t.to_string(),
                    (Some("channel"), "itunes:new-feed-url", None) if !t.is_empty() => show.new_url = Some(t.to_string()),
                    (Some("image"), "url", None) if show.image.is_none() && !t.is_empty() => show.image = Some(t.to_string()),
                    (_, "item", Some(_)) => {
                        let mut e = ep.take().unwrap_or_default();
                        if !e.url.is_empty() {
                            if e.guid.is_empty() {
                                e.guid = e.url.clone();
                            }
                            show.episodes.push(e);
                        }
                    }
                    _ => {}
                }
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if show.title.is_empty() && show.episodes.is_empty() {
        return Err("not a podcast feed".into());
    }
    Ok(show)
}

/// Attribute-carrying tags: enclosure, images, chapters, transcripts.
fn attrs(e: &BytesStart, path: &[String], show: &mut Show, ep: &mut Option<Episode>) {
    let get = |k: &str| e.try_get_attribute(k).ok().flatten().and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok()).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let in_channel = path.last().is_some_and(|p| p == "channel");
    match (e.name().as_ref(), ep.as_mut()) {
        ("enclosure", Some(ep)) => {
            ep.url = get("url").unwrap_or_default();
            ep.mime = get("type").unwrap_or_default();
            ep.bytes = get("length").and_then(|l| l.parse().ok()).unwrap_or(0);
        }
        ("itunes:image", Some(ep)) => ep.image = get("href"),
        ("itunes:image", None) if in_channel => show.image = get("href").or(show.image.take()),
        ("podcast:chapters", Some(ep)) => ep.chapters = get("url"),
        ("podcast:transcript", Some(ep)) => {
            if let (Some(url), Some(mime)) = (get("url"), get("type")) {
                // timed formats first: they can follow along like lyrics
                let rank = |m: &str| ["text/vtt", "application/x-subrip", "application/srt", "application/json", "text/html", "text/plain"].iter().position(|x| *x == m).unwrap_or(9);
                if ep.transcript.as_ref().is_none_or(|(_, cur)| rank(&mime) < rank(cur)) {
                    ep.transcript = Some((url, mime));
                }
            }
        }
        _ => {}
    }
}

/// `itunes:duration`: "3725", "62:05", "1:02:05" (and the odd "3725.4") → seconds.
pub fn duration(s: &str) -> u32 {
    s.split(':').try_fold(0u32, |acc, p| p.trim().split('.').next()?.parse::<u32>().ok().map(|n| acc * 60 + n)).unwrap_or(0)
}

/// RFC 2822 dates as feeds write them ("Wed, 19 Nov 2025 21:00:00 -0500", "GMT", "PST", no
/// seconds, no weekday, 2-digit years) → Unix seconds.
pub fn rfc2822(s: &str) -> Option<i64> {
    let s = s.split_once(',').map_or(s, |(_, rest)| rest);
    let mut it = s.split_whitespace();
    let day: i64 = it.next()?.parse().ok()?;
    let mon = it.next()?.get(..3)?.to_ascii_lowercase();
    let month = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"].iter().position(|m| *m == mon)? as i64 + 1;
    let mut year: i64 = it.next()?.parse().ok()?;
    if year < 100 {
        year += if year < 50 { 2000 } else { 1900 };
    }
    let mut hms = it.next().unwrap_or("0:0").split(':').map(|p| p.parse::<i64>().unwrap_or(0));
    let (h, m, sec) = (hms.next()?, hms.next().unwrap_or(0), hms.next().unwrap_or(0));
    let zone = it.next().unwrap_or("Z");
    let off_min = match zone.as_bytes().first() {
        Some(b'+' | b'-') if zone.len() >= 5 => {
            let v: i64 = zone[1..5].parse().ok()?;
            let m = v / 100 * 60 + v % 100;
            if zone.starts_with('-') { -m } else { m }
        }
        _ => 60 * match zone.to_ascii_uppercase().as_str() {
            "EDT" => -4,
            "EST" | "CDT" => -5,
            "CST" | "MDT" => -6,
            "MST" | "PDT" => -7,
            "PST" => -8,
            _ => 0, // GMT, UT, UTC, Z and anything unknown
        },
    };
    Some(days_from_civil(year, month, day) * 86400 + h * 3600 + m * 60 + sec - off_min * 60)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(rfc2822("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(rfc2822("Wed, 19 Nov 2025 21:00:00 -0500"), Some(1763604000));
        assert_eq!(rfc2822("Wed, 19 Nov 2025 21:00:00 EST"), rfc2822("Wed, 19 Nov 2025 21:00:00 -0500"));
        assert_eq!(rfc2822("Wed, 19 Nov 2025 18:00:00 PST"), rfc2822("Thu, 20 Nov 2025 02:00:00 +0000"));
        assert_eq!(rfc2822("Wed, 19 Nov 2025 19:00:00 CDT"), rfc2822("Thu, 20 Nov 2025 00:00:00 GMT"));
        assert_eq!(rfc2822("19 Nov 2025 21:00 +0000"), Some(1763586000), "no weekday, no seconds");
        assert_eq!(rfc2822("Sat, 29 Feb 2020 12:00:00 +0530"), Some(1582957800));
        assert_eq!(rfc2822("29 February 20 12:00:00 Z"), rfc2822("Sat, 29 Feb 2020 12:00:00 +0000"), "2-digit year, long month");
        assert_eq!(rfc2822("garbage"), None);
    }

    #[test]
    fn durations() {
        assert_eq!(duration("3725"), 3725);
        assert_eq!(duration("62:05"), 3725);
        assert_eq!(duration("1:02:05"), 3725);
        assert_eq!(duration("3725.4"), 3725);
        assert_eq!(duration(""), 0);
        assert_eq!(duration("about an hour"), 0);
    }

    #[test]
    fn parses_a_feed() {
        let xml = br#"<?xml version="1.0"?>
<rss xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd" xmlns:podcast="https://podcastindex.org/namespace/1.0" xmlns:content="http://purl.org/rss/1.0/modules/content/">
<channel>
  <title>Tom &amp; Jerry&#39;s Show</title>
  <itunes:author>T&amp;J</itunes:author>
  <itunes:new-feed-url>https://new.example/feed</itunes:new-feed-url>
  <image><url>https://ex/cover-small.jpg</url><title>ignored</title></image>
  <itunes:image href="https://ex/cover.jpg"/>
  <item>
    <title><![CDATA[Ep 2: <Fun> & games]]></title>
    <guid isPermaLink="false">ep-2</guid>
    <pubDate>Thu, 01 Jan 1970 01:00:00 GMT</pubDate>
    <itunes:duration>1:00:01</itunes:duration>
    <enclosure url="https://ex/2.mp3?a=1&amp;b=2" type="audio/mpeg" length="123"/>
    <description>short</description>
    <content:encoded><![CDATA[<p>long</p>]]></content:encoded>
    <podcast:chapters url="https://ex/2.json" type="application/json+chapters"/>
    <podcast:transcript url="https://ex/2.html" type="text/html"/>
    <podcast:transcript url="https://ex/2.vtt" type="text/vtt"/>
    <podcast:transcript url="https://ex/2.json" type="application/json"/>
  </item>
  <item><title>no audio, skipped</title><guid>x</guid></item>
  <item>
    <title>Ep 1</title>
    <enclosure url="https://ex/1.mp3" type="audio/mpeg"/>
    <content:encoded>only content</content:encoded>
  </item>
</channel></rss>"#;
        let s = parse(xml).unwrap();
        assert_eq!(s.title, "Tom & Jerry's Show");
        assert_eq!(s.author, "T&J");
        assert_eq!(s.image.as_deref(), Some("https://ex/cover.jpg"));
        assert_eq!(s.new_url.as_deref(), Some("https://new.example/feed"));
        assert_eq!(s.episodes.len(), 2);
        let e = &s.episodes[0];
        assert_eq!((e.title.as_str(), e.guid.as_str(), e.published, e.duration_s), ("Ep 2: <Fun> & games", "ep-2", 3600, 3601));
        assert_eq!((e.url.as_str(), e.mime.as_str(), e.bytes), ("https://ex/2.mp3?a=1&b=2", "audio/mpeg", 123));
        assert_eq!(e.notes, "short");
        assert_eq!(e.chapters.as_deref(), Some("https://ex/2.json"));
        assert_eq!(e.transcript, Some(("https://ex/2.vtt".into(), "text/vtt".into())));
        let e = &s.episodes[1];
        assert_eq!((e.guid.as_str(), e.notes.as_str(), e.duration_s), ("https://ex/1.mp3", "only content", 0));
    }

    #[test]
    fn rejects_non_feeds() {
        assert!(parse(b"<html><body>hi</body></html>").is_err());
        assert!(parse(b"not xml at all").is_err());
    }
}
