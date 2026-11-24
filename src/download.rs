//! Downloads for listening offline: one file per episode in `~/.local/share/hark/downloads`,
//! written to a `.part` file first and resumed from where it stopped (an HTTP range request,
//! only when the server vouches it's the same file: `If-Range` with a strong ETag).

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub fn dir() -> PathBuf {
    crate::paths::data_dir().join("downloads")
}

/// The file an episode downloads to: its guid's hash, with the audio's own extension.
pub fn file_name(guid: &str, url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).filter(|e| matches!(e.as_str(), "mp3" | "m4a" | "mp4" | "aac")).unwrap_or_else(|| "mp3".into());
    format!("{:016x}.{ext}", crate::paths::fnv(guid))
}

/// Download `url` to `dest`, resuming a `.part` left from before. `progress(done, total)` is
/// called as bytes arrive (total 0 = unknown).
pub async fn fetch(client: &reqwest::Client, url: &str, dest: &Path, progress: impl Fn(u64, u64)) -> Result<(), String> {
    if let Some(d) = dest.parent() {
        fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let part = dest.with_extension("part");
    // what the first response said the file was: resume only into that same file. Shows with
    // ads stitched in on the fly hand out a different file every time, and the end of one
    // glued to the start of another isn't an episode.
    let tag = dest.with_extension("part-version");
    let version = fs::read_to_string(&tag).unwrap_or_default();
    let have = if version.is_empty() { 0 } else { fs::metadata(&part).map_or(0, |m| m.len()) };
    let mut req = client.get(url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-")).header(reqwest::header::IF_RANGE, version.trim());
    }
    let mut resp = req.send().await.map_err(|e| e.without_url().to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    // 206: the same file, from where the .part ends; 200: a new (or changed) file, from the start
    let resumed = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    if !resumed {
        let h = |k| resp.headers().get(k).and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok()).unwrap_or("").to_string();
        // a strong ETag is the only safe version; a weak one (W/) or a date can't promise the bytes match
        let etag = h(reqwest::header::ETAG);
        let v = if !etag.is_empty() && !etag.starts_with("W/") { etag } else { String::new() };
        let _ = fs::write(&tag, v);
    }
    let mut done = if resumed { have } else { 0 };
    let total = resp.content_length().map_or(0, |n| n + done);
    let mut file = OpenOptions::new().create(true).write(true).append(resumed).truncate(!resumed).open(&part).map_err(|e| e.to_string())?;
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.without_url().to_string())? {
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        done += chunk.len() as u64;
        progress(done, total);
    }
    if total > 0 && done < total {
        return Err("the download stopped early; d resumes it".into());
    }
    file.sync_all().map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&tag);
    fs::rename(&part, dest).map_err(|e| e.to_string())
}

#[test]
fn file_names() {
    assert_eq!(file_name("g", "https://x/ep.MP3?id=1"), format!("{:016x}.mp3", crate::paths::fnv("g")));
    assert!(file_name("g", "https://x/ep.m4a").ends_with(".m4a"));
    assert!(file_name("g", "https://x/play/123").ends_with(".mp3"), "no extension: mp3");
}
