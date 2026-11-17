//! Updates. Once a day Kompacter asks GitHub for its newest published release (drafts are
//! skipped; pre-releases count, since that's how Kompacter ships for now); if it's newer,
//! the installer is downloaded in the background and checked against the SHA-256 the
//! release publishes next to it. It's installed silently on the next restart (the button
//! in the window, or when Kompacter quits), and the installer opens the new version.
//!
//! Only release builds know their version (the release workflow stamps
//! `KOMPACTER_VERSION`), so dev builds never update. The only host contacted is GitHub.
//! Downloads use Windows' own curl.exe (no extra libraries in the app).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use sha2::{Digest, Sha256};

/// This build's version ("0.0.7"), or `None` for builds made outside the release workflow.
pub const VERSION: Option<&str> = option_env!("KOMPACTER_VERSION");

const RELEASES: &str = "https://api.github.com/repos/KXZer0/Kompacter/releases?per_page=20";
/// Where release files may come from: GitHub itself and its download host.
const HOSTS: [&str; 2] = [
    "https://github.com/",
    "https://objects.githubusercontent.com/",
];

/// A downloaded, checked installer, ready to run.
#[derive(Debug, Clone)]
pub struct Ready {
    pub version: String,
    pub installer: PathBuf,
}

/// "v0.0.7-test" (or "v.0.0.7-test") → (0, 0, 7). Anything after a dash is ignored.
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches(['v', '.']).split('-').next()?;
    let mut n = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((
        n.next()??,
        n.next().flatten().unwrap_or(0),
        n.next().flatten().unwrap_or(0),
    ))
}

/// True when `candidate` is a newer version than `current`.
pub fn newer(candidate: &str, current: &str) -> bool {
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Fetch a URL with curl (hidden window, HTTPS only, follows GitHub's redirects).
fn curl(url: &str, out: Option<&Path>) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("curl");
    cmd.args(["-fsSL", "--proto", "=https", "--proto-redir", "=https"])
        .args(["--max-time", "600", "-A", "Kompacter-updater"]);
    if url.starts_with("https://api.github.com/") {
        cmd.args(["-H", "Accept: application/vnd.github+json"]);
    }
    if let Some(out) = out {
        cmd.arg("-o").arg(out);
    }
    cmd.arg(url);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let done = cmd
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    if !done.status.success() {
        return Err(format!(
            "download failed: {}",
            String::from_utf8_lossy(&done.stderr).trim()
        ));
    }
    Ok(done.stdout)
}

fn sha256_hex(path: &Path) -> io::Result<String> {
    use std::io::Read;
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// The installer and its checksum file in a release's JSON: (name, installer url, sha url).
fn assets(release: &Value) -> Option<(String, String, String)> {
    let list = release["assets"].as_array()?;
    let url_of = |name: &str| {
        list.iter()
            .find(|a| a["name"].as_str() == Some(name))
            .and_then(|a| a["browser_download_url"].as_str())
            .filter(|u| HOSTS.iter().any(|h| u.starts_with(h)))
            .map(str::to_string)
    };
    let name = list
        .iter()
        .filter_map(|a| a["name"].as_str())
        .find(|n| n.starts_with("Kompacter-") && n.ends_with("-setup.exe"))?
        .to_string();
    let exe = url_of(&name)?;
    let sha = url_of(&format!("{name}.sha256"))?;
    Some((name, exe, sha))
}

/// The newest published release that has an installer and its checksum: (tag, assets).
fn newest(list: &Value) -> Option<(String, (String, String, String))> {
    list.as_array()?
        .iter()
        .filter(|r| r["draft"].as_bool() != Some(true))
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            Some((parse(tag)?, tag.to_string(), assets(r)?))
        })
        .max_by_key(|(v, _, _)| *v)
        .map(|(_, tag, a)| (tag, a))
}

/// Check for a newer release and download it into `dir`. `Ok(None)`: nothing newer (or
/// a dev build). A file that doesn't match its checksum is deleted, never returned.
pub fn check(current: &str, dir: &Path) -> Result<Option<Ready>, String> {
    let body = curl(RELEASES, None)?;
    let list: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    let Some((tag, (name, exe_url, sha_url))) = newest(&list) else {
        return Ok(None);
    };
    if !newer(&tag, current) {
        return Ok(None);
    }
    let want = String::from_utf8_lossy(&curl(&sha_url, None)?)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if want.len() != 64 {
        return Err("the checksum file is broken".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let installer = dir.join(&name);
    // Already downloaded on an earlier check.
    if sha256_hex(&installer).ok().as_deref() != Some(want.as_str()) {
        curl(&exe_url, Some(&installer))?;
        let got = sha256_hex(&installer).map_err(|e| e.to_string())?;
        if got != want {
            let _ = std::fs::remove_file(&installer);
            return Err("the download didn't match its checksum".into());
        }
    }
    let (a, b, c) = parse(&tag).unwrap_or_default();
    let version = format!("{a}.{b}.{c}");
    Ok(Some(Ready { version, installer }))
}

/// Where downloads wait until they're installed.
pub fn download_dir() -> PathBuf {
    std::env::temp_dir().join("Kompacter-update")
}

/// Remove downloaded installers. Run when Kompacter starts: anything there is either the
/// update that was just installed or one that wasn't (it's fetched again if still newer).
pub fn clear_downloads() {
    let _ = std::fs::remove_dir_all(download_dir());
}

/// Run the installer silently; it closes Kompacter, replaces it and opens the new version.
pub fn install(ready: &Ready) -> io::Result<()> {
    Command::new(&ready.installer).arg("/S").spawn().map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Talks to GitHub: `cargo test -p kompacter -- --ignored live`.
    #[test]
    #[ignore]
    fn live_check_against_github() {
        let dir = tempfile::tempdir().unwrap();
        let got = check("0.0.0", dir.path());
        assert!(got.is_ok(), "{got:?}");
    }

    #[test]
    fn hashes_match_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("abc");
        std::fs::write(&f, b"abc").unwrap();
        assert_eq!(
            sha256_hex(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn versions_compare() {
        assert!(newer("v0.0.8", "0.0.7"));
        assert!(newer("v0.1.0", "0.0.9"));
        assert!(newer("v1.0.0-test", "0.9.9"));
        assert!(!newer("v0.0.7", "0.0.7"));
        assert!(!newer("v0.0.6", "0.0.7"));
        assert!(!newer("nightly", "0.0.7"));
        assert!(newer("v.0.0.8-Kompacter-App", "0.0.7"));
    }

    #[test]
    fn picks_the_newest_published_release() {
        let rel = |tag: &str, draft: bool| {
            let base = format!("https://github.com/KXZer0/Kompacter/releases/download/{tag}");
            serde_json::json!({"tag_name": tag, "draft": draft, "prerelease": true, "assets": [
                {"name": "Kompacter-1-setup.exe", "browser_download_url": format!("{base}/a.exe")},
                {"name": "Kompacter-1-setup.exe.sha256", "browser_download_url": format!("{base}/a.sha256")}
            ]})
        };
        let list = serde_json::json!([
            rel("v0.0.9-App", true),
            rel("v0.0.8-App", false),
            rel("v0.0.10-App", true),
            {"tag_name": "v0.1.0", "draft": false, "assets": []},
            rel("v0.0.7-App", false),
        ]);
        assert_eq!(newest(&list).unwrap().0, "v0.0.8-App");
    }

    #[test]
    fn finds_the_installer_and_its_checksum() {
        let base = "https://github.com/KXZer0/Kompacter/releases/download/v0.0.8";
        let release = serde_json::json!({"assets": [
            {"name": "Kompacter-0.0.8-setup.exe", "browser_download_url": format!("{base}/Kompacter-0.0.8-setup.exe")},
            {"name": "Kompacter-0.0.8-setup.exe.sha256", "browser_download_url": format!("{base}/Kompacter-0.0.8-setup.exe.sha256")}
        ]});
        let (name, exe, sha) = assets(&release).unwrap();
        assert_eq!(name, "Kompacter-0.0.8-setup.exe");
        assert!(exe.ends_with(".exe") && sha.ends_with(".sha256"));
        // Files hosted anywhere else are ignored.
        let elsewhere = serde_json::json!({"assets": [
            {"name": "Kompacter-0.0.8-setup.exe", "browser_download_url": "https://example.com/x.exe"},
            {"name": "Kompacter-0.0.8-setup.exe.sha256", "browser_download_url": format!("{base}/x.sha256")}
        ]});
        assert!(assets(&elsewhere).is_none());
    }
}
