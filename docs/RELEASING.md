# Releasing

Every download is built by GitHub Actions; nobody has to build from source.

## Make a release

0. Add the version's "What's new" lines to `apps/kompacter/src/app/news.rs` (3–4 short lines; no
   entry means no What's new screen).
1. Tag a commit and push the tag (GitHub Desktop: History → right-click → Create Tag → Push).
   The version comes from the tag: `v0.0.3-test` builds version 0.0.3, marked as a pre-release.
2. The **Release** workflow builds every download in parallel, then its `publish` job creates
   one **draft** release with all of them. Check it, then press **Publish**.

   | Job | Files | Built from |
   |---|---|---|
   | `windows` | `Kompacter-windows-setup.exe` | NSIS script in `packaging/windows/`, ffmpeg included |
   | `linux` (Ubuntu 22.04) | `Kompacter-linux-x86_64.AppImage` (the .deb and tarball are still built as a check, not published) | `packaging/linux/appimage.sh` and `build.sh`; ffmpeg included |
   | `macos` (`macos-latest`, Apple silicon; no Intel build because Homebrew no longer supports Intel Macs) | `Kompacter-installer-macos-arm64.dmg` | `packaging/macos/build-app.sh`, `make-dmg.sh` and, with the signing secrets, `notarize.sh`; Homebrew ffmpeg included |

3. Once it's published (drafts don't count; pre-releases only for users who turned on "Use
   pre-release versions"), installed copies find it within a day and show "Update available".
   Nothing installs until the user presses Update:
   - Windows downloads the installer in the background, checks its SHA-256, and runs it when
     the user presses Update (Kompacter restarts).
   - The Linux AppImage downloads the new `.AppImage`, checks it and replaces itself (same
     file name) when the user presses Update.
   - macOS (in Applications or another folder it can write to) downloads the new `.dmg`, checks it and swaps its app bundle in place when the user presses Update, so there is never a second copy. Run from the disk image, Downloads or a read-only folder, and from an older .deb or tarball install, it opens the release page.

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

## Checksums

GitHub records every release file's SHA-256 (shown on the release page, and in the API as
the asset's `digest`), and the updater checks downloads against it. Windows and Linux still
publish a `.sha256` file next to their download, only because updaters up to 0.1.2 need it.
Once a release with the digest-reading updater has been out for a while, remove those two
from `release.yml` (the Checksum steps and the `publish` check), leaving three files.

## Update test

`update_end_to_end` (in `apps/kompacter/src/app/update.rs`) runs the app's own update on
a real file in a temp folder: Windows runs the installer silently, Linux swaps an AppImage,
macOS mounts the disk image and replaces a stand-in `Kompacter.app`.

- `release.yml` runs it on each freshly built file before upload, so a packaging change
  that would break updating stops the release.
- `macos-review.yml` runs it on the review DMG for every pull request.
- `update-test.yml` runs it against the newest *published* release (download, SHA-256
  check, install) on pull requests that touch the updater or packaging, weekly, and by
  hand. A system with no published file yet is skipped (drafts don't count).

Locally: `cargo test -p kompacter update_end_to_end -- --ignored --nocapture`, optionally
with `KOMPACTER_UPDATE_FILE=<a built file>`.
