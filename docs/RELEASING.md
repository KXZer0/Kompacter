# Releasing

Every download is built by GitHub Actions; nobody has to build from source.

## Make a release

0. Add the version's "What's new" lines to `apps/kompacter/src/app/news.rs` (3–4 short lines; no
   entry means no What's new screen).
1. Tag a commit and push the tag (GitHub Desktop: History → right-click → Create Tag → Push).
   The version comes from the tag: `v0.0.3-test` builds version 0.0.3, marked as a pre-release.
2. The **Release** workflow builds every download in parallel, then its `publish` job creates
   one **draft** release with all of them. Check it, then press **Publish**.

   | Job | Files (each with a `.sha256`) | Built from |
   |---|---|---|
   | `windows` | `Kompacter-windows-setup.exe` | NSIS script in `packaging/windows/`, ffmpeg included |
   | `linux` (Ubuntu 22.04) | `Kompacter-linux-x86_64.AppImage` (the .deb and tarball are still built as a check, not published) | `packaging/linux/appimage.sh` and `build.sh`; ffmpeg included |
   | `macos` (`macos-latest`, Apple silicon; no Intel build because Homebrew no longer supports Intel Macs) | `Kompacter-installer-macos-arm64.dmg` | `packaging/macos/build-app.sh`, `make-dmg.sh` and, with the signing secrets, `notarize.sh`; Homebrew ffmpeg included |

3. Once it's published (pre-releases count; drafts don't), installed copies find it within a
   day:
   - Windows downloads the installer in the background, checks its `.sha256` and installs it on
     the next restart.
   - The Linux AppImage downloads the new `.AppImage`, checks its `.sha256` and replaces itself
     (same file name) on the next restart.
   - macOS, and copies installed from an older .deb or tarball, offer the update and open the release page.

   The release page only opens when the user clicks the update pill, never on quit.

   A release only counts as an update for a system when it has that system's file, so keep the
   file names above (the updater matches them).

You can also start it by hand: Actions → Release → Run workflow. Manual runs build version 0.0.0
(an app that never updates itself) and attach the files to the run as artifacts instead of making
a release.

The Windows installer is per-user (no admin), into `%LOCALAPPDATA%\Programs\Kompacter`, with a
Start menu entry and an uninstaller in Settings → Apps. ffmpeg's GPL license is installed next to
the app as `FFMPEG-LICENSE.txt`; the AppImage has it in `usr/share/doc/kompacter/FFmpeg-LICENSE`,
the tarball as `FFmpeg-LICENSE` and the macOS app in `Contents/Resources/FFmpeg-LICENSE.txt`.

Before a release meant for everyone, go through [TESTING.md](TESTING.md) on a real PC.

Windows and Linux bundle the newest *stable* ffmpeg from BtbN's builds (the highest `nX.Y`
version; never the nightly), picked when the workflow runs: the win64 and the static linux64
GPL builds. macOS bundles Homebrew's ffmpeg for the runner's architecture.

The AppImage is built with a pinned `appimagetool` (`APPIMAGETOOL_URL` in `release.yml`). It
contains Kompacter, ffmpeg and ffprobe only; OpenGL/EGL, xkbcommon, Wayland/X11 and fontconfig come
from the user's system.

## macOS signing and notarization

Without secrets the DMGs are ad-hoc signed, and users have to right-click → **Open** the first time
(see [macOS packaging](../packaging/macos/README.md)). To sign and notarize, add these repository
secrets (Settings → Secrets and variables → Actions):

| Secret | What |
|---|---|
| `APPLE_CERTIFICATE_P12` | A **Developer ID Application** certificate with its private key, exported as `.p12` and base64-encoded (`base64 -i cert.p12 \| pbcopy`) |
| `APPLE_CERTIFICATE_PASSWORD` | The `.p12` export password |
| `APPLE_ID` | The Apple ID of the developer account |
| `APPLE_TEAM_ID` | The 10-character team ID |
| `APPLE_APP_PASSWORD` | An app-specific password for that Apple ID (appleid.apple.com) |

With the certificate, ffmpeg, ffprobe, their libraries and the app are signed with the hardened
runtime; with the other three as well, the DMG is notarized (`xcrun notarytool submit --wait`) and
stapled.

## Before calling it "official"

| Step | Why | How |
|---|---|---|
| Code signing | Without it, Windows SmartScreen warns "unknown publisher" | Free for open source via [SignPath Foundation](https://signpath.org), or Azure Trusted Signing (~$10/month) |
| Apple notarization | Without it, macOS blocks the first start | Apple Developer Program, then the secrets above |
| winget | `winget install Kompacter` | Submit a manifest to `microsoft/winget-pkgs` after the first signed release |
| Flathub | Linux app stores | The AppStream file is in `packaging/linux/` |

## Desktop review builds

- Linux: `packaging/linux/build.sh` builds the amd64 Debian package and the portable archive
  (with ffmpeg when `KOMPACTER_FFMPEG_DIR` is set), and `packaging/linux/appimage.sh` the
  AppImage. Run them on an x86-64 Linux machine with Debian packaging tools,
  `desktop-file-validate` and (for the AppImage) `appimagetool`. See
  [Linux packaging](../packaging/linux/README.md).
- The macOS review workflow builds an application bundle with FFmpeg and ffprobe,
  runs a compression fixture, builds a DMG and uploads the bundle archive, DMG and checksum.
  See [macOS packaging](../packaging/macos/README.md).
