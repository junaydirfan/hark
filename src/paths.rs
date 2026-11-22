//! Where hark keeps things, and the two small helpers every saved file needs.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

fn base(xdg: &str, unix: &str, mac: &str, win: &str) -> PathBuf {
    if let Some(p) = std::env::var_os(xdg).filter(|v| !v.is_empty()) {
        return PathBuf::from(p).join("hark");
    }
    let home = |var| std::env::var_os(var).map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    if cfg!(windows) {
        home(win).join("hark")
    } else if cfg!(target_os = "macos") {
        home("HOME").join(mac).join("hark")
    } else {
        home("HOME").join(unix).join("hark")
    }
}

/// Subscriptions, listening state and preferences: `~/.local/share/hark`.
pub fn data_dir() -> PathBuf {
    base("XDG_DATA_HOME", ".local/share", "Library/Application Support", "APPDATA")
}

/// Feed caches and in-progress streams, all safe to delete: `~/.cache/hark`.
pub fn cache_dir() -> PathBuf {
    base("XDG_CACHE_HOME", ".cache", "Library/Caches", "LOCALAPPDATA")
}

/// FNV-1a: a hash that stays the same across Rust versions, for file names on disk.
pub fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// Write to a temporary file, then rename it over `path`: a crash mid-write never leaves a
/// half-written file behind.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

#[test]
fn fnv_is_stable() {
    assert_eq!(fnv(""), 0xcbf29ce484222325);
    assert_eq!(fnv("a"), 0xaf63dc4c8601ec8c);
}
