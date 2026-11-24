//! Finding shows: Apple Podcasts' public directory (no account, no key). Search sends what you
//! type; charts send a genre and a country. Recommendations are built from the categories your
//! own feeds declare, weighted by what you listen to, so your list of shows never leaves the
//! machine: Apple only ever sees which genre charts are asked for.

use serde_json::Value;

/// A show you might follow.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub title: String,
    pub author: String,
    pub feed: String,
    pub genre: String,
    pub episodes: u32,
    /// When the latest episode came out (Unix seconds; 0 = unknown): a dead show looks dead.
    pub updated: i64,
    pub explicit: bool,
    /// "for you", "#3" (chart position) or "" (search).
    pub tag: String,
}

/// "2026-11-05T09:00:00Z" → Unix seconds.
fn iso_date(s: &str) -> Option<i64> {
    let (date, time) = s.split_once('T').unwrap_or((s, "0:0:0"));
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.trim_end_matches('Z').split(':').map(|p| p.parse::<i64>().unwrap_or(0));
    let secs = t.next().unwrap_or(0) * 3600 + t.next().unwrap_or(0) * 60 + t.next().unwrap_or(0);
    Some(crate::feed::days_from_civil(y, m, day) * 86400 + secs)
}

/// How long ago, the way people say it: "today", "3d ago", "5mo ago".
pub fn ago(now: i64, t: i64) -> String {
    if t <= 0 {
        return String::new();
    }
    let days = (now - t).max(0) / 86400;
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        2..14 => format!("{days}d ago"),
        14..60 => format!("{}w ago", days / 7),
        60..730 => format!("{}mo ago", days / 30),
        _ => format!("{}y ago", days / 365),
    }
}

/// Apple's podcast genres (`/ws/genres?id=26`). Feeds use the same names in `itunes:category`.
pub const GENRES: [(&str, u32); 19] = [
    ("Arts", 1301),
    ("Business", 1321),
    ("Comedy", 1303),
    ("Education", 1304),
    ("Fiction", 1483),
    ("Government", 1511),
    ("Health & Fitness", 1512),
    ("History", 1487),
    ("Kids & Family", 1305),
    ("Leisure", 1502),
    ("Music", 1310),
    ("News", 1489),
    ("Religion & Spirituality", 1314),
    ("Science", 1533),
    ("Society & Culture", 1324),
    ("Sports", 1545),
    ("TV & Film", 1309),
    ("Technology", 1318),
    ("True Crime", 1488),
];

/// Apple's podcast subgenres (`/ws/genres?id=26`), also the names feeds nest in `itunes:category`.
pub const SUBGENRES: [(&str, u32); 91] = [
    ("Books", 1482),
    ("Design", 1402),
    ("Fashion & Beauty", 1459),
    ("Food", 1306),
    ("Performing Arts", 1405),
    ("Visual Arts", 1406),
    ("Careers", 1410),
    ("Entrepreneurship", 1493),
    ("Investing", 1412),
    ("Management", 1491),
    ("Marketing", 1492),
    ("Non-Profit", 1494),
    ("Comedy Interviews", 1496),
    ("Improv", 1495),
    ("Stand-Up", 1497),
    ("Courses", 1501),
    ("How To", 1499),
    ("Language Learning", 1498),
    ("Self-Improvement", 1500),
    ("Comedy Fiction", 1486),
    ("Drama", 1484),
    ("Science Fiction", 1485),
    ("Alternative Health", 1513),
    ("Fitness", 1514),
    ("Medicine", 1518),
    ("Mental Health", 1517),
    ("Nutrition", 1515),
    ("Sexuality", 1516),
    ("Education for Kids", 1519),
    ("Parenting", 1521),
    ("Pets & Animals", 1522),
    ("Stories for Kids", 1520),
    ("Animation & Manga", 1510),
    ("Automotive", 1503),
    ("Aviation", 1504),
    ("Crafts", 1506),
    ("Games", 1507),
    ("Hobbies", 1505),
    ("Home & Garden", 1508),
    ("Video Games", 1509),
    ("Music Commentary", 1523),
    ("Music History", 1524),
    ("Music Interviews", 1525),
    ("Business News", 1490),
    ("Daily News", 1526),
    ("Entertainment News", 1531),
    ("News Commentary", 1530),
    ("Politics", 1527),
    ("Sports News", 1529),
    ("Tech News", 1528),
    ("Buddhism", 1438),
    ("Christianity", 1439),
    ("Hinduism", 1463),
    ("Islam", 1440),
    ("Judaism", 1441),
    ("Religion", 1532),
    ("Spirituality", 1444),
    ("Astronomy", 1538),
    ("Chemistry", 1539),
    ("Earth Sciences", 1540),
    ("Life Sciences", 1541),
    ("Mathematics", 1536),
    ("Natural Sciences", 1534),
    ("Nature", 1537),
    ("Physics", 1542),
    ("Social Sciences", 1535),
    ("Documentary", 1543),
    ("Personal Journals", 1302),
    ("Philosophy", 1443),
    ("Places & Travel", 1320),
    ("Relationships", 1544),
    ("Baseball", 1549),
    ("Basketball", 1548),
    ("Cricket", 1554),
    ("Fantasy Sports", 1560),
    ("Football", 1547),
    ("Golf", 1553),
    ("Hockey", 1550),
    ("Rugby", 1552),
    ("Running", 1551),
    ("Soccer", 1546),
    ("Swimming", 1558),
    ("Tennis", 1556),
    ("Volleyball", 1557),
    ("Wilderness", 1559),
    ("Wrestling", 1555),
    ("After Shows", 1562),
    ("Film History", 1564),
    ("Film Interviews", 1565),
    ("Film Reviews", 1563),
    ("TV Reviews", 1561),
];

pub fn subgenre_id(name: &str) -> Option<u32> {
    SUBGENRES.iter().find(|(n, _)| n.eq_ignore_ascii_case(name.trim())).map(|g| g.1)
}

/// A genre's or subgenre's name by its id.
pub fn genre_name(id: u32) -> Option<&'static str> {
    GENRES.iter().chain(SUBGENRES.iter()).find(|g| g.1 == id).map(|g| g.0)
}

pub fn genre_id(name: &str) -> Option<u32> {
    GENRES.iter().find(|(n, _)| n.eq_ignore_ascii_case(name.trim())).map(|g| g.1)
}

/// The store country from the locale ("en_CA.UTF-8" → "ca"); "us" when it can't tell.
pub fn country() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find_map(|l| {
            let cc = l.split(['.', '@']).next()?.split_once('_')?.1.to_ascii_lowercase();
            (cc.len() == 2 && cc.chars().all(|c| c.is_ascii_alphabetic())).then_some(cc)
        })
        .unwrap_or_else(|| "us".into())
}

fn card(r: &Value, tag: String) -> Option<Card> {
    let feed = r["feedUrl"].as_str()?.trim();
    (!feed.is_empty()).then(|| Card {
        title: r["collectionName"].as_str().or(r["trackName"].as_str()).unwrap_or("").to_string(),
        author: r["artistName"].as_str().unwrap_or("").to_string(),
        feed: feed.to_string(),
        genre: r["primaryGenreName"].as_str().unwrap_or("").to_string(),
        episodes: r["trackCount"].as_u64().unwrap_or(0) as u32,
        updated: r["releaseDate"].as_str().and_then(iso_date).unwrap_or(0),
        explicit: r["collectionExplicitness"].as_str() == Some("explicit"),
        tag,
    })
}

/// iTunes search/lookup results → cards (shows without a public feed are left out).
pub fn parse_results(v: &Value) -> Vec<Card> {
    v["results"].as_array().into_iter().flatten().filter_map(|r| card(r, String::new())).collect()
}

/// A chart (the legacy RSS JSON) → Apple ids in chart order.
pub fn parse_chart(v: &Value) -> Vec<String> {
    v["feed"]["entry"].as_array().into_iter().flatten().filter_map(|e| Some(e["id"]["attributes"]["im:id"].as_str()?.to_string())).collect()
}

async fn get_json(client: &reqwest::Client, url: &str) -> Result<Value, String> {
    let resp = client.get(url).send().await.map_err(|e| e.without_url().to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let body = resp.bytes().await.map_err(|e| e.without_url().to_string())?;
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub async fn search(client: &reqwest::Client, term: &str, country: &str) -> Result<Vec<Card>, String> {
    let url = format!("https://itunes.apple.com/search?media=podcast&entity=podcast&limit=50&country={country}&term={}", enc(term));
    Ok(parse_results(&get_json(client, &url).await?))
}

/// A top chart (all podcasts, or one genre or subgenre), in chart order, tagged with the
/// position. Kept on disk for 12 hours, so Discover opens without asking Apple most of the
/// time; when Apple can't be reached, an older copy is better than nothing.
pub async fn top(client: &reqwest::Client, country: &str, genre: Option<u32>) -> Result<Vec<Card>, String> {
    let path = crate::paths::cache_dir().join("charts").join(format!("{country}-{}.tsv", genre.unwrap_or(0)));
    let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
    let cached = || std::fs::read_to_string(&path).ok().map(|t| cards_from_tsv(&t)).filter(|c| !c.is_empty());
    if age.is_some_and(|a| a.as_secs() < CHART_FRESH_S)
        && let Some(c) = cached()
    {
        return Ok(c);
    }
    match fetch_top(client, country, genre).await {
        Ok(c) => {
            if !c.is_empty() {
                let _ = crate::paths::write_atomic(&path, cards_to_tsv(&c).as_bytes());
            }
            Ok(c)
        }
        Err(e) => cached().ok_or(e),
    }
}

const CHART_FRESH_S: u64 = 12 * 3600;

async fn fetch_top(client: &reqwest::Client, country: &str, genre: Option<u32>) -> Result<Vec<Card>, String> {
    let g = genre.map_or(String::new(), |g| format!("/genre={g}"));
    let ids = parse_chart(&get_json(client, &format!("https://itunes.apple.com/{country}/rss/toppodcasts/limit=50{g}/json")).await?);
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let found = get_json(client, &format!("https://itunes.apple.com/lookup?entity=podcast&country={country}&id={}", ids.join(","))).await?;
    let by_id: Vec<(String, Card)> = found["results"].as_array().into_iter().flatten().filter_map(|r| Some((r["collectionId"].as_u64()?.to_string(), card(r, String::new())?))).collect();
    Ok(ids
        .iter()
        .enumerate()
        .filter_map(|(i, id)| by_id.iter().find(|(k, _)| k == id).map(|(_, c)| Card { tag: format!("#{}", i + 1), ..c.clone() }))
        .collect())
}

fn cards_to_tsv(cards: &[Card]) -> String {
    let f = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    cards.iter().map(|c| format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n", f(&c.title), f(&c.author), f(&c.feed), f(&c.genre), c.episodes, c.updated, c.explicit as u8, f(&c.tag))).collect()
}

fn cards_from_tsv(text: &str) -> Vec<Card> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let [title, author, feed, genre, episodes, updated, explicit, tag] = f[..] else { return None };
            Some(Card {
                title: title.into(),
                author: author.into(),
                feed: feed.into(),
                genre: genre.into(),
                episodes: episodes.parse().unwrap_or(0),
                updated: updated.parse().unwrap_or(0),
                explicit: explicit == "1",
                tag: tag.into(),
            })
        })
        .collect()
}

/// A show you follow, as For you sees it: how much you listen to it, and what it's about.
pub struct Taste<'a> {
    /// 1 for following it, plus the hours you've listened lately.
    pub weight: f64,
    pub genres: &'a [String],
    pub subgenres: &'a [String],
}

/// The charts worth asking for: your genres and subgenres by how much you listen to them, the
/// sharper subgenres ahead of their broad parents. Weights add up to 1.
pub fn taste_genres(shows: &[Taste], max: usize) -> Vec<(u32, f64)> {
    let mut w: Vec<(u32, f64)> = Vec::new();
    let mut add = |id: u32, x: f64| match w.iter_mut().find(|(g, _)| *g == id) {
        Some(e) => e.1 += x,
        None => w.push((id, x)),
    };
    for s in shows {
        let subs: Vec<u32> = s.subgenres.iter().filter_map(|n| subgenre_id(n)).collect();
        let tops: Vec<u32> = s.genres.iter().filter_map(|n| genre_id(n)).collect();
        // a show that names a subgenre is mostly about that; its broad genre counts half
        let top_share = if subs.is_empty() { 1.0 } else { 0.5 };
        for &g in &subs {
            add(g, s.weight / subs.len() as f64);
        }
        for &g in &tops {
            add(g, top_share * s.weight / tops.len() as f64);
        }
    }
    w.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    w.truncate(max);
    let total: f64 = w.iter().map(|g| g.1).sum();
    w.iter().map(|&(g, x)| (g, x / total)).collect()
}

/// For you: the charts of your genres (fetched at once), each show scored by how much its
/// charts matter to you and how high it sits in them, a show in several of them first, and
/// shows that went quiet pushed down. `hide`: shows you follow or said no to.
pub async fn for_you(client: &reqwest::Client, country: &str, genres: &[(u32, f64)], hide: &(dyn Fn(&Card) -> bool + Sync)) -> Result<Vec<Card>, String> {
    let mut set = tokio::task::JoinSet::new();
    for (i, &(g, _)) in genres.iter().enumerate() {
        let (client, country) = (client.clone(), country.to_string());
        set.spawn(async move { (i, top(&client, &country, Some(g)).await) });
    }
    let mut charts: Vec<Option<Vec<Card>>> = vec![None; genres.len()];
    let mut err = String::new();
    while let Some(done) = set.join_next().await {
        match done {
            Ok((i, Ok(c))) => charts[i] = Some(c),
            Ok((_, Err(e))) => err = e,
            Err(e) => err = e.to_string(),
        }
    }
    // one chart failing still leaves the others
    if !genres.is_empty() && charts.iter().all(Option::is_none) {
        return Err(err);
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    Ok(rank(genres, &charts, hide, now, 30))
}

/// How much a show's last episode date counts: quiet for months is a bad sign.
fn freshness(updated: i64, now: i64) -> f64 {
    if updated <= 0 {
        return 0.8;
    }
    match (now - updated) / 86400 {
        ..=30 => 1.0,
        31..=90 => 0.6,
        _ => 0.2,
    }
}

fn rank(genres: &[(u32, f64)], charts: &[Option<Vec<Card>>], hide: &dyn Fn(&Card) -> bool, now: i64, max: usize) -> Vec<Card> {
    // (score, best single contribution, the genre it came from, card)
    let mut seen: Vec<(f64, f64, u32, Card)> = Vec::new();
    for (&(g, w), chart) in genres.iter().zip(charts) {
        for (pos, c) in chart.iter().flatten().enumerate() {
            if hide(c) {
                continue;
            }
            let x = w / (1.0 + pos as f64 / 10.0);
            match seen.iter_mut().find(|s| s.3.feed == c.feed || s.3.title.eq_ignore_ascii_case(&c.title)) {
                Some(s) => {
                    s.0 += x;
                    if x > s.1 {
                        (s.1, s.2) = (x, g);
                    }
                }
                None => seen.push((x, x, g, c.clone())),
            }
        }
    }
    for s in &mut seen {
        s.0 *= freshness(s.3.updated, now);
    }
    // best first, but each pick from a genre makes its next one count 15% less, so your top
    // genre leads without crowding out the others
    let mut out = Vec::new();
    let mut picked: Vec<u32> = Vec::new();
    while out.len() < max && !seen.is_empty() {
        let damp = |g: u32| 0.85f64.powi(picked.iter().filter(|&&p| p == g).count() as i32);
        let best = (0..seen.len()).max_by(|&i, &j| (seen[i].0 * damp(seen[i].2)).total_cmp(&(seen[j].0 * damp(seen[j].2)))).unwrap_or(0);
        let (_, _, g, c) = seen.swap_remove(best);
        picked.push(g);
        out.push(Card { tag: "for you".into(), genre: genre_name(g).map_or(c.genre.clone(), str::to_string), ..c });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(title: &str, feed: &str) -> Card {
        Card { title: title.into(), author: String::new(), feed: feed.into(), genre: String::new(), episodes: 0, updated: 0, explicit: false, tag: String::new() }
    }

    #[test]
    fn parses_search_and_charts() {
        let v: Value = serde_json::from_str(r#"{"resultCount":3,"results":[
            {"collectionId":1,"collectionName":"Planet Money","artistName":"NPR","feedUrl":"https://feeds.npr.org/510289/podcast.xml","primaryGenreName":"Business","trackCount":706,"releaseDate":"2026-11-05T09:00:00Z","collectionExplicitness":"explicit"},
            {"collectionId":2,"collectionName":"No feed","artistName":"X"},
            {"collectionId":3,"trackName":"Only track name","feedUrl":" https://x/feed "}]}"#).unwrap();
        let cards = parse_results(&v);
        assert_eq!(cards.len(), 2);
        assert_eq!((cards[0].title.as_str(), cards[0].genre.as_str(), cards[0].episodes), ("Planet Money", "Business", 706));
        assert_eq!((cards[0].updated, cards[0].explicit), (crate::feed::rfc2822("Thu, 05 Nov 2026 09:00:00 GMT").unwrap(), true));
        assert_eq!((cards[1].updated, cards[1].explicit), (0, false));
        assert_eq!((cards[1].title.as_str(), cards[1].feed.as_str()), ("Only track name", "https://x/feed"));
        let chart: Value = serde_json::from_str(r#"{"feed":{"entry":[{"id":{"attributes":{"im:id":"77"}}},{"id":{"attributes":{"im:id":"5"}}}]}}"#).unwrap();
        assert_eq!(parse_chart(&chart), ["77", "5"]);
    }

    #[test]
    fn for_you_scores_listening_and_charts() {
        let (a, b, x, y) = ("a".to_string(), "b".to_string(), "x".to_string(), "y".to_string());
        // a show you listen to a lot (Tech News, under Technology) and one you just follow (Comedy)
        let shows = [
            Taste { weight: 11.0, genres: &["Technology".into()], subgenres: &["Tech News".into()] },
            Taste { weight: 1.0, genres: &["Comedy".into()], subgenres: &[] },
        ];
        let g = taste_genres(&shows, 4);
        assert_eq!(g.iter().map(|g| g.0).collect::<Vec<_>>(), [1528, 1318, 1303], "subgenre first, then its parent, then the rest");
        assert!((g.iter().map(|g| g.1).sum::<f64>() - 1.0).abs() < 1e-9);

        let now = 1_800_000_000;
        let card = |t: &String, days: i64| Card { updated: now - days * 86400, ..c(t, t) };
        let charts = vec![
            Some(vec![card(&a, 1), card(&x, 1), card(&b, 400)]), // tech news
            Some(vec![card(&x, 1)]),                            // technology: x is in both
            None,                                               // comedy didn't load
        ];
        let got = rank(&g, &charts, &|c| c.feed == "y", now, 10);
        let names: Vec<&str> = got.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(names, ["x", "a", "b"], "in two charts beats first in one; a quiet show sinks");
        assert_eq!((got[0].tag.as_str(), got[1].genre.as_str()), ("for you", "Tech News"), "the genre says why");
        assert!(rank(&g, &charts, &|c| c.feed == "x" || c.feed == "a" || c.feed == "b", now, 10).is_empty(), "hidden and followed stay out");
        // one heavy genre with a long chart, one light genre: the light one still gets in early
        let heavy: Vec<Card> = (0..10).map(|i| card(&format!("h{i}"), 1)).collect();
        let got = rank(&[(1, 0.7), (2, 0.3)], &[Some(heavy), Some(vec![card(&"l0".to_string(), 1)])], &|_| false, now, 10);
        let at = got.iter().position(|c| c.title == "l0").unwrap();
        assert!(at <= 4, "a 0.3 genre's #1 comes by the 5th pick, not after the 0.7 genre's 10 (at {at})");
        let _ = y;
    }

    #[test]
    fn chart_cache_round_trip() {
        let cards = vec![Card { episodes: 7, updated: 5, explicit: true, tag: "#1".into(), ..c("Tab\there", "https://f") }];
        let back = cards_from_tsv(&cards_to_tsv(&cards));
        assert_eq!(back[0], Card { title: "Tab here".into(), ..cards[0].clone() });
    }

    #[test]
    fn ages() {
        let now = 1_800_000_000;
        assert_eq!(ago(now, 0), "");
        assert_eq!(ago(now, now - 3600), "today");
        assert_eq!(ago(now, now - 86400), "yesterday");
        assert_eq!(ago(now, now - 5 * 86400), "5d ago");
        assert_eq!(ago(now, now - 21 * 86400), "3w ago");
        assert_eq!(ago(now, now - 200 * 86400), "6mo ago");
        assert_eq!(ago(now, now - 3 * 365 * 86400), "3y ago");
    }

    #[test]
    fn genres_and_countries() {
        assert_eq!(genre_id("business"), Some(1321));
        assert_eq!(genre_id(" True Crime "), Some(1488));
        assert_eq!(genre_id("Podcasts"), None);
        assert_eq!(enc("tom & jerry/ü"), "tom+%26+jerry%2F%C3%BC");
    }
}
