//! Finding shows: Apple Podcasts' public directory (no account, no key). Search sends what you
//! type; charts send a genre and a country. Recommendations are built from the categories your
//! own feeds declare, so your list of shows never leaves the machine.

use serde_json::Value;

/// A show you might follow.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub title: String,
    pub author: String,
    pub feed: String,
    pub genre: String,
    pub episodes: u32,
    /// "for you", "#3" (chart position) or "" (search).
    pub tag: String,
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

/// A top chart (all podcasts, or one genre), in chart order, tagged with the position.
pub async fn top(client: &reqwest::Client, country: &str, genre: Option<u32>) -> Result<Vec<Card>, String> {
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

/// For you: the charts of the genres your shows are in, most common genre first, taken in
/// turns so one genre doesn't crowd out the rest; shows you follow are left out.
pub async fn for_you(client: &reqwest::Client, country: &str, genres: &[u32], follows: &(dyn Fn(&Card) -> bool + Sync)) -> Result<Vec<Card>, String> {
    let mut charts = Vec::new();
    for &g in genres.iter().take(3) {
        charts.push(top(client, country, Some(g)).await?);
    }
    Ok(interleave(charts, follows, 30))
}

/// Round-robin over the charts, skipping followed shows and repeats.
fn interleave(charts: Vec<Vec<Card>>, follows: &dyn Fn(&Card) -> bool, max: usize) -> Vec<Card> {
    let mut out: Vec<Card> = Vec::new();
    let longest = charts.iter().map(Vec::len).max().unwrap_or(0);
    for i in 0..longest {
        for c in charts.iter().filter_map(|ch| ch.get(i)) {
            if out.len() < max && !follows(c) && !out.iter().any(|o| o.feed == c.feed || o.title.eq_ignore_ascii_case(&c.title)) {
                out.push(Card { tag: "for you".into(), ..c.clone() });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(title: &str, feed: &str) -> Card {
        Card { title: title.into(), author: String::new(), feed: feed.into(), genre: String::new(), episodes: 0, tag: String::new() }
    }

    #[test]
    fn parses_search_and_charts() {
        let v: Value = serde_json::from_str(r#"{"resultCount":3,"results":[
            {"collectionId":1,"collectionName":"Planet Money","artistName":"NPR","feedUrl":"https://feeds.npr.org/510289/podcast.xml","primaryGenreName":"Business","trackCount":706},
            {"collectionId":2,"collectionName":"No feed","artistName":"X"},
            {"collectionId":3,"trackName":"Only track name","feedUrl":" https://x/feed "}]}"#).unwrap();
        let cards = parse_results(&v);
        assert_eq!(cards.len(), 2);
        assert_eq!((cards[0].title.as_str(), cards[0].genre.as_str(), cards[0].episodes), ("Planet Money", "Business", 706));
        assert_eq!((cards[1].title.as_str(), cards[1].feed.as_str()), ("Only track name", "https://x/feed"));
        let chart: Value = serde_json::from_str(r#"{"feed":{"entry":[{"id":{"attributes":{"im:id":"77"}}},{"id":{"attributes":{"im:id":"5"}}}]}}"#).unwrap();
        assert_eq!(parse_chart(&chart), ["77", "5"]);
    }

    #[test]
    fn for_you_takes_turns_and_skips_follows() {
        let biz = vec![c("A", "a"), c("Followed", "f"), c("B", "b")];
        let comedy = vec![c("X", "x"), c("a", "a2"), c("Y", "y")];
        let got: Vec<String> = interleave(vec![biz, comedy], &|c| c.feed == "f", 10).into_iter().map(|c| c.title).collect();
        assert_eq!(got, ["A", "X", "B", "Y"], "turns; the followed show and the same title twice are skipped");
    }

    #[test]
    fn genres_and_countries() {
        assert_eq!(genre_id("business"), Some(1321));
        assert_eq!(genre_id(" True Crime "), Some(1488));
        assert_eq!(genre_id("Podcasts"), None);
        assert_eq!(enc("tom & jerry/ü"), "tom+%26+jerry%2F%C3%BC");
    }
}
