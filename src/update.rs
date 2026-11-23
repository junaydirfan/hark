//! Updating from GitHub releases: `hark --update`, and a once-a-day check that puts
//! "vX is out" on the bottom line.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use crate::paths;

const API: &str = "https://api.github.com/repos/junaydirfan/hark/releases/latest";
const DAY: Duration = Duration::from_secs(24 * 3600);

/// The release download for this system, as CI names it.
const ASSET: Option<&str> = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("hark-linux-x86_64.tar.gz")
} else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("hark-macos-arm64.tar.gz")
} else if cfg!(all(windows, target_arch = "x86_64")) {
    Some("hark-windows-x86_64.zip")
} else {
    None
};
const EXE: &str = if cfg!(windows) { "hark.exe" } else { "hark" };

/// "v1.2.3" → (1, 2, 3); missing parts are 0, anything after a `-` is ignored.
fn parse(v: &str) -> (u32, u32, u32) {
    let mut n = v.trim().trim_start_matches('v').split('-').next().unwrap_or("").split('.').map(|p| p.parse().unwrap_or(0));
    (n.next().unwrap_or(0), n.next().unwrap_or(0), n.next().unwrap_or(0))
}

fn is_newer(tag: &str) -> bool {
    parse(tag) > parse(env!("CARGO_PKG_VERSION"))
}

fn tag_of(json: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(json).ok()?["tag_name"].as_str().map(String::from)
}

fn client() -> reqwest::Client {
    // GitHub refuses requests without a User-Agent
    reqwest::Client::builder().user_agent(concat!("hark/", env!("CARGO_PKG_VERSION"))).build().expect("http client")
}

async fn latest(client: &reqwest::Client) -> Result<String, String> {
    let resp = client.get(API).header("Accept", "application/vnd.github+json").timeout(Duration::from_secs(10)).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    tag_of(&resp.bytes().await.map_err(|e| e.to_string())?).ok_or_else(|| "no release tag in GitHub's answer".into())
}

/// The newest release's tag if it's newer than this build. Asks GitHub at most once a day
/// (the answer is kept in the cache dir); stays quiet when offline.
pub async fn check() -> Option<String> {
    let file = paths::cache_dir().join("latest_release");
    let fresh = fs::metadata(&file).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|d| d < DAY));
    let tag = if fresh {
        fs::read_to_string(&file).ok()?
    } else {
        let tag = latest(&client()).await.ok()?;
        let _ = paths::write_atomic(&file, tag.as_bytes());
        tag
    };
    let tag = tag.trim();
    is_newer(tag).then(|| tag.to_string())
}

/// `hark --update`: download the latest release and put it in place of this binary.
pub async fn run() -> Result<(), String> {
    let Some(asset) = ASSET else {
        return Err("there's no ready-made hark for this system. Update by building it:\n  \
                    cargo install --git https://github.com/junaydirfan/hark --force"
            .into());
    };
    let client = client();
    let tag = latest(&client).await.map_err(|e| format!("couldn't ask GitHub for the latest release ({e}). Are you online?"))?;
    let cache = paths::cache_dir();
    let _ = paths::write_atomic(&cache.join("latest_release"), tag.as_bytes());
    let have = env!("CARGO_PKG_VERSION");
    if !is_newer(&tag) {
        println!("hark {have} is up to date (latest release: {tag}).");
        return Ok(());
    }

    println!("Updating hark {have} → {tag}…");
    let staging = cache.join("update");
    let res = fetch_and_unpack(&client, &format!("https://github.com/junaydirfan/hark/releases/download/{tag}/{asset}"), &staging).await;
    let res = res.and_then(|new| {
        // a truncated or wrong download must never replace a working hark
        let out = Command::new(&new).arg("--version").output().map_err(|e| format!("the downloaded hark won't run: {e}"))?;
        let want = format!("hark {}", tag.trim_start_matches('v'));
        if String::from_utf8_lossy(&out.stdout).trim() != want {
            return Err(format!("the downloaded hark doesn't say it's {tag}; left the old one in place"));
        }
        swap(&new).map_err(|e| {
            format!("couldn't replace hark: {e}\nIf a package manager installed it, update it there; otherwise see the README's Updating section.")
        })
    });
    let _ = fs::remove_dir_all(&staging);
    let exe = res?;
    println!("Updated {} to {tag}. Restart hark to use it.", exe.display());
    Ok(())
}

/// Download a release archive into `staging` and unpack it; returns the hark inside.
async fn fetch_and_unpack(client: &reqwest::Client, url: &str, staging: &Path) -> Result<PathBuf, String> {
    let _ = fs::remove_dir_all(staging);
    fs::create_dir_all(staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let resp = client.get(url).send().await.map_err(|e| format!("{url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{url}: HTTP {}", resp.status()));
    }
    let archive = staging.join(url.rsplit('/').next().unwrap_or("download"));
    fs::write(&archive, resp.bytes().await.map_err(|e| format!("{url}: {e}"))?).map_err(|e| format!("{}: {e}", archive.display()))?;
    // tar detects gzip or zip itself (GNU tar, macOS bsdtar and Windows' tar.exe alike)
    let ok = Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(staging).status().is_ok_and(|s| s.success());
    let new = staging.join(EXE);
    if ok && new.is_file() { Ok(new) } else { Err(format!("couldn't unpack {url}")) }
}

/// Put `new` where the running hark is, as a rename so it's never half-written.
fn swap(new: &Path) -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let dir = exe.parent().unwrap_or(Path::new("."));
    // copy next to the old one first so the swap below is a rename on one disk
    let tmp = dir.join(".hark-update");
    fs::copy(new, &tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
    }
    // Windows can't replace a running .exe but can rename it; the old one goes next update.
    #[cfg(windows)]
    {
        let old = exe.with_extension("old");
        let _ = fs::remove_file(&old);
        fs::rename(&exe, &old)?;
    }
    if let Err(e) = fs::rename(&tmp, &exe) {
        #[cfg(windows)]
        let _ = fs::rename(exe.with_extension("old"), &exe);
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert_eq!(parse("v1.2"), (1, 2, 0));
        assert_eq!(parse("v0.10.1-rc1"), (0, 10, 1));
        assert!(parse("v0.10.0") > parse("0.9.9"));
        assert!(is_newer("v999.0.0"));
        assert!(!is_newer(concat!("v", env!("CARGO_PKG_VERSION"))));
        assert_eq!(tag_of(br#"{"tag_name":"v0.3.0","name":"x"}"#).as_deref(), Some("v0.3.0"));
    }
}
