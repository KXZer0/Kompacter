//! Updates. Once a day Kompacter asks GitHub for its newest published release (drafts are
//! skipped; pre-releases count, since that's how Kompacter ships for now). What happens
//! with a newer one depends on how this copy was installed:
//!
//! - Windows: the installer is downloaded in the background and checked against the
//!   SHA-256 the release publishes next to it. It's installed silently on the next restart
//!   (the button in the window, or when Kompacter quits), and the installer opens the new
//!   version.
//! - Linux AppImage (`$APPIMAGE` is set): the new `.AppImage` is downloaded and checked the
//!   same way, then swapped in for the running file (same name, atomic rename) on the next
//!   restart, and started again.
//! - Everything else (macOS, the Linux .deb or tarball): nothing is downloaded; "install"
//!   opens the release page so the user can get the new build for their system (the .deb
//!   and tarball are no longer published, so on Linux that's the AppImage). Only a
//!   release that actually has a build for this system counts as an update.
//!
//! Only release builds know their version (the release workflow stamps
//! `KOMPACTER_VERSION`), so dev builds never update. The only host contacted is GitHub.
//! Downloads use the system's curl (Windows ships curl.exe; no extra libraries in the app).

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

/// A newer release, ready to install: a downloaded, checked file, or (where Kompacter
/// can't update itself) the page to get it from.
#[derive(Debug, Clone)]
pub struct Ready {
    pub version: String,
    /// The checked download (empty when there's nothing to download).
    pub installer: PathBuf,
    how: How,
}

/// How this copy of Kompacter updates itself.
#[derive(Debug, Clone, PartialEq, Eq)]
enum How {
    /// Run the downloaded Windows installer.
    Installer,
    /// Replace this AppImage file with the downloaded one.
    AppImage(PathBuf),
    /// Open the release page in the browser.
    Page(String),
}

/// What this copy looks for in a release: the file name's prefix and suffix, and whether
/// Kompacter downloads and installs it itself (then its `.sha256` must be there too).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Wanted {
    prefix: &'static str,
    suffix: &'static str,
    download: bool,
}

/// The release file for an `os`/`arch` (as in `std::env::consts`); `appimage` when
/// running from an AppImage. `None`: no builds are published for this system.
fn wanted(os: &str, arch: &str, appimage: bool) -> Option<Wanted> {
    let (prefix, suffix, download) = match (os, arch) {
        ("windows", _) => ("Kompacter-", "-setup.exe", true),
        ("linux", "x86_64") if appimage => ("Kompacter-", "-x86_64.AppImage", true),
        // Installed from a .deb or tarball: those aren't published any more, so the
        // AppImage tells it a new version is out (and the release page has it).
        ("linux", "x86_64") => ("Kompacter-", "-x86_64.AppImage", false),
        ("macos", "aarch64") => ("Kompacter-", "-macos-arm64.dmg", false),
        ("macos", "x86_64") => ("Kompacter-", "-macos-x86_64.dmg", false),
        _ => return None,
    };
    Some(Wanted {
        prefix,
        suffix,
        download,
    })
}

/// The running AppImage file, if Kompacter was started from one.
fn running_appimage() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_file())
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

/// curl for one URL: hidden window, HTTPS only, follows GitHub's redirects.
fn curl_cmd(url: &str, out: Option<&Path>) -> Command {
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
    cmd
}

/// Fetch a small file (the release list, a checksum) into memory.
fn curl(url: &str) -> Result<Vec<u8>, String> {
    let done = curl_cmd(url, None)
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

/// Download to `out`, reporting 0..1 by watching the file grow against `size`.
fn download(url: &str, out: &Path, size: u64, progress: &dyn Fn(f32)) -> Result<(), String> {
    let mut child = curl_cmd(url, Some(out))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    loop {
        let got = std::fs::metadata(out).map_or(0, |m| m.len());
        progress((got as f64 / size.max(1) as f64).min(0.99) as f32);
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) if status.success() => return Ok(()),
            Some(_) => return Err("download failed".into()),
            None => std::thread::sleep(std::time::Duration::from_millis(300)),
        }
    }
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

/// A release's file for this system, its checksum (only when Kompacter downloads it)
/// and the release's page.
#[derive(Debug)]
struct Asset {
    name: String,
    exe: String,
    /// The `.sha256` file's address, when there's one and GitHub gave no digest.
    sha: String,
    /// The file's SHA-256 as GitHub reports it (the asset's `digest`, lowercase hex).
    digest: Option<String>,
    size: u64,
    page: String,
}

fn on_github(url: &str) -> bool {
    HOSTS.iter().any(|h| url.starts_with(h))
}

fn assets(release: &Value, want: Wanted) -> Option<Asset> {
    let list = release["assets"].as_array()?;
    let find = |name: &str| {
        list.iter()
            .find(|a| a["name"].as_str() == Some(name))
            .filter(|a| a["browser_download_url"].as_str().is_some_and(on_github))
    };
    let name = list
        .iter()
        .filter_map(|a| a["name"].as_str())
        .find(|n| {
            n.len() > want.prefix.len() + want.suffix.len()
                && n.starts_with(want.prefix)
                && n.ends_with(want.suffix)
        })?
        .to_string();
    let exe = find(&name)?;
    // GitHub records every asset's SHA-256 itself, so no `.sha256` file is needed; it is
    // still read where GitHub gives none (releases from before it did).
    let digest = exe["digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase);
    let sha = if want.download && digest.is_none() {
        find(&format!("{name}.sha256"))?["browser_download_url"]
            .as_str()?
            .to_string()
    } else {
        String::new()
    };
    // Only ever open a page on GitHub itself.
    let page = match release["html_url"].as_str() {
        Some(url) if url.starts_with("https://github.com/") => url.to_string(),
        _ => format!(
            "https://github.com/KXZer0/Kompacter/releases/tag/{}",
            release["tag_name"].as_str()?
        ),
    };
    Some(Asset {
        exe: exe["browser_download_url"].as_str()?.to_string(),
        sha,
        size: exe["size"].as_u64().unwrap_or(0),
        digest: digest.filter(|_| want.download),
        name,
        page,
    })
}

/// The newest published release that has this system's file (and its checksum).
fn newest(list: &Value, want: Wanted) -> Option<(String, Asset)> {
    list.as_array()?
        .iter()
        .filter(|r| r["draft"].as_bool() != Some(true))
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            Some((parse(tag)?, tag.to_string(), assets(r, want)?))
        })
        .max_by_key(|(v, _, _)| *v)
        .map(|(_, tag, a)| (tag, a))
}

/// Check for a newer release and, where Kompacter installs updates itself, download it
/// into `dir`. `Ok(None)`: nothing newer (or a dev build, or no builds for this system).
/// A file that doesn't match its checksum is deleted, never returned. `progress` gets
/// the new version and 0..1 while the file downloads.
pub fn check(
    current: &str,
    dir: &Path,
    progress: &dyn Fn(&str, f32),
) -> Result<Option<Ready>, String> {
    let appimage = running_appimage();
    let Some(want) = wanted(
        std::env::consts::OS,
        std::env::consts::ARCH,
        appimage.is_some(),
    ) else {
        return Ok(None);
    };
    let body = curl(RELEASES)?;
    let list: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    let Some((tag, asset)) = newest(&list, want) else {
        return Ok(None);
    };
    if !newer(&tag, current) {
        return Ok(None);
    }
    let (a, b, c) = parse(&tag).unwrap_or_default();
    let version = format!("{a}.{b}.{c}");
    if !want.download {
        return Ok(Some(Ready {
            version,
            installer: PathBuf::new(),
            how: How::Page(asset.page),
        }));
    }
    let want_sha = match &asset.digest {
        Some(digest) => digest.clone(),
        None => String::from_utf8_lossy(&curl(&asset.sha)?)
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase(),
    };
    if want_sha.len() != 64 {
        return Err("the checksum file is broken".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let installer = dir.join(&asset.name);
    // Already downloaded on an earlier check.
    if sha256_hex(&installer).ok().as_deref() != Some(want_sha.as_str()) {
        download(&asset.exe, &installer, asset.size, &|p| {
            progress(&version, p)
        })?;
        let got = sha256_hex(&installer).map_err(|e| e.to_string())?;
        if got != want_sha {
            let _ = std::fs::remove_file(&installer);
            return Err("the download didn't match its checksum".into());
        }
    }
    let how = match appimage {
        Some(path) => How::AppImage(path),
        None => How::Installer,
    };
    Ok(Some(Ready {
        version,
        installer,
        how,
    }))
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

impl Ready {
    /// True when the update is applied as Kompacter quits (Windows installer, AppImage).
    /// Otherwise the release page only opens when the user asks for it.
    pub fn installs_on_quit(&self) -> bool {
        !matches!(self.how, How::Page(_))
    }
}

/// Install a checked update. Windows: run the installer silently; it closes Kompacter,
/// replaces it and opens the new version. AppImage: swap the file in and start it again.
/// Everywhere else: open the release page.
pub fn install(ready: &Ready) -> io::Result<()> {
    match &ready.how {
        How::Installer => Command::new(&ready.installer).arg("/S").spawn().map(drop),
        How::AppImage(target) => {
            replace_file(&ready.installer, target)?;
            // A moment for this process to exit, so the new one doesn't hand off to it.
            Command::new("/bin/sh")
                .args(["-c", "sleep 1; exec \"$0\""])
                .arg(target)
                .spawn()
                .map(drop)
        }
        How::Page(url) => open_url(url),
    }
}

/// Copy `new` next to `target` (same folder, so the final rename is atomic), make it
/// executable and rename it over `target`. A running copy keeps its already-open file.
fn replace_file(new: &Path, target: &Path) -> io::Result<()> {
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        return Err(io::Error::other("not a file path"));
    };
    let mut temp = std::ffi::OsString::from(".");
    temp.push(name);
    temp.push(".update");
    let temp = dir.join(temp);
    let result = (|| {
        std::fs::copy(new, &temp)?;
        std::fs::File::open(&temp)?.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&temp, target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Open a web page in the default browser.
fn open_url(url: &str) -> io::Result<()> {
    #[cfg(windows)]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "/usr/bin/open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";
    Command::new(program).arg(url).spawn().map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOWS: Wanted = Wanted {
        prefix: "Kompacter-",
        suffix: "-setup.exe",
        download: true,
    };

    /// A release with every system's files, as the release workflow publishes them: no
    /// version in the names, so the README can link to releases/latest/download/<name>.
    fn full_release(v: &str) -> Value {
        release_with(
            v,
            &[
                "Kompacter-windows-setup.exe",
                "Kompacter-linux-x86_64.AppImage",
                "Kompacter-installer-macos-arm64.dmg",
            ],
        )
    }

    /// A release with these files, each with its `.sha256`.
    fn release_with(v: &str, names: &[&str]) -> Value {
        let base = format!("https://github.com/KXZer0/Kompacter/releases/download/v{v}");
        let assets: Vec<Value> = names
            .iter()
            .flat_map(|n| [n.to_string(), format!("{n}.sha256")])
            .map(|n| {
                serde_json::json!({
                    "name": n, "size": 7, "browser_download_url": format!("{base}/{n}")
                })
            })
            .collect();
        serde_json::json!({
            "tag_name": format!("v{v}"),
            "draft": false,
            "html_url": format!("https://github.com/KXZer0/Kompacter/releases/tag/v{v}"),
            "assets": assets,
        })
    }

    /// Talks to GitHub: `cargo test -p kompacter -- --ignored live`.
    #[test]
    #[ignore]
    fn live_check_against_github() {
        let dir = tempfile::tempdir().unwrap();
        let got = check("0.0.0", dir.path(), &|_, _| {});
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
        assert_eq!(newest(&list, WINDOWS).unwrap().0, "v0.0.8-App");
    }

    #[test]
    fn finds_the_installer_and_its_checksum() {
        let base = "https://github.com/KXZer0/Kompacter/releases/download/v0.0.8";
        let release = serde_json::json!({"tag_name": "v0.0.8", "assets": [
            {"name": "Kompacter-0.0.8-setup.exe", "size": 41_000_000, "browser_download_url": format!("{base}/Kompacter-0.0.8-setup.exe")},
            {"name": "Kompacter-0.0.8-setup.exe.sha256", "browser_download_url": format!("{base}/Kompacter-0.0.8-setup.exe.sha256")}
        ]});
        let a = assets(&release, WINDOWS).unwrap();
        assert_eq!(a.name, "Kompacter-0.0.8-setup.exe");
        assert!(a.exe.ends_with(".exe") && a.sha.ends_with(".sha256"));
        assert_eq!(a.size, 41_000_000);
        // Files hosted anywhere else are ignored.
        let elsewhere = serde_json::json!({"tag_name": "v0.0.8", "assets": [
            {"name": "Kompacter-0.0.8-setup.exe", "browser_download_url": "https://example.com/x.exe"},
            {"name": "Kompacter-0.0.8-setup.exe.sha256", "browser_download_url": format!("{base}/x.sha256")}
        ]});
        assert!(assets(&elsewhere, WINDOWS).is_none());
    }

    #[test]
    fn each_system_picks_its_own_file() {
        let release = full_release("0.2.0");
        let pick = |os, arch, appimage| {
            let want = wanted(os, arch, appimage)?;
            Some((assets(&release, want)?, want.download))
        };
        let (a, download) = pick("windows", "x86_64", false).unwrap();
        assert_eq!(a.name, "Kompacter-windows-setup.exe");
        assert!(download && a.sha.ends_with("-setup.exe.sha256"));

        let (a, download) = pick("linux", "x86_64", true).unwrap();
        assert_eq!(a.name, "Kompacter-linux-x86_64.AppImage");
        assert!(download && a.sha.ends_with(".AppImage.sha256"));

        // A .deb / tarball install and macOS only point at the release page.
        let (a, download) = pick("linux", "x86_64", false).unwrap();
        assert_eq!(a.name, "Kompacter-linux-x86_64.AppImage");
        assert!(!download && a.sha.is_empty());
        assert_eq!(
            a.page,
            "https://github.com/KXZer0/Kompacter/releases/tag/v0.2.0"
        );
        let (a, download) = pick("macos", "aarch64", false).unwrap();
        assert_eq!(a.name, "Kompacter-installer-macos-arm64.dmg");
        assert!(!download);

        // No Intel Mac or ARM Linux build in this release: no update for them.
        assert!(pick("macos", "x86_64", false).is_none());
        assert!(pick("linux", "aarch64", true).is_none());
        assert!(pick("freebsd", "x86_64", false).is_none());
    }

    #[test]
    fn githubs_digest_replaces_the_checksum_file() {
        let base = "https://github.com/KXZer0/Kompacter/releases/download/v0.3.0";
        let hex = "ab".repeat(32);
        let release = serde_json::json!({"tag_name": "v0.3.0", "assets": [
            {"name": "Kompacter-windows-setup.exe", "size": 7, "digest": format!("sha256:{}", hex.to_uppercase()),
             "browser_download_url": format!("{base}/Kompacter-windows-setup.exe")}
        ]});
        let a = assets(&release, WINDOWS).unwrap();
        assert_eq!(a.digest.as_deref(), Some(hex.as_str()));
        assert!(a.sha.is_empty());
        // No digest and no checksum file: not offered.
        let mut bare = release.clone();
        bare["assets"][0].as_object_mut().unwrap().remove("digest");
        assert!(assets(&bare, WINDOWS).is_none());
        // A broken digest falls back to the checksum file.
        let mut odd = full_release("0.3.0");
        odd["assets"][0]["digest"] = "sha256:nothex".into();
        let a = assets(&odd, WINDOWS).unwrap();
        assert!(a.digest.is_none() && a.sha.ends_with(".sha256"));
    }

    #[test]
    fn older_versioned_names_still_match() {
        // Releases up to 0.1.2 put the version in each name.
        let old = release_with(
            "0.1.2",
            &[
                "Kompacter-0.1.2-setup.exe",
                "Kompacter-0.1.2-x86_64.AppImage",
                "Kompacter-0.1.2-macos-arm64.dmg",
            ],
        );
        for (os, arch, appimage, name) in [
            ("windows", "x86_64", false, "Kompacter-0.1.2-setup.exe"),
            ("linux", "x86_64", true, "Kompacter-0.1.2-x86_64.AppImage"),
            ("macos", "aarch64", false, "Kompacter-0.1.2-macos-arm64.dmg"),
        ] {
            let a = assets(&old, wanted(os, arch, appimage).unwrap()).unwrap();
            assert_eq!(a.name, name);
        }
        // And a new release beats it.
        let list = serde_json::json!([old, full_release("0.2.0")]);
        assert_eq!(newest(&list, WINDOWS).unwrap().0, "v0.2.0");
    }

    #[test]
    fn a_release_without_this_systems_build_is_skipped() {
        let mut windows_only = full_release("0.3.0");
        windows_only["assets"]
            .as_array_mut()
            .unwrap()
            .retain(|a| a["name"].as_str().unwrap().contains("-setup.exe"));
        let list = serde_json::json!([windows_only, full_release("0.2.0")]);
        let mac = wanted("macos", "aarch64", false).unwrap();
        assert_eq!(newest(&list, mac).unwrap().0, "v0.2.0");
        assert_eq!(newest(&list, WINDOWS).unwrap().0, "v0.3.0");
    }

    #[test]
    fn an_appimage_without_its_checksum_is_not_offered() {
        let mut release = full_release("0.2.0");
        release["assets"]
            .as_array_mut()
            .unwrap()
            .retain(|a| a["name"].as_str() != Some("Kompacter-linux-x86_64.AppImage.sha256"));
        assert!(assets(&release, wanted("linux", "x86_64", true).unwrap()).is_none());
    }

    #[test]
    fn the_release_page_stays_on_github() {
        let mut release = full_release("0.2.0");
        release["html_url"] = "https://example.com/elsewhere".into();
        let a = assets(&release, wanted("macos", "aarch64", false).unwrap()).unwrap();
        assert_eq!(
            a.page,
            "https://github.com/KXZer0/Kompacter/releases/tag/v0.2.0"
        );
    }

    #[cfg(unix)]
    #[test]
    fn replaces_the_appimage_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("download");
        let target = dir.path().join("Kompacter.AppImage");
        std::fs::write(&new, b"new").unwrap();
        std::fs::write(&target, b"old").unwrap();
        replace_file(&new, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        // No temporary file is left behind.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
