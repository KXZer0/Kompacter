# Releasing

Installers are built by GitHub Actions; nobody has to build from source.

## Make a release

0. Add the version's "What's new" lines to `apps/kompacter/src/news.rs` (3–4 short lines; no
   entry means no What's new screen).
1. Tag a commit and push the tag (GitHub Desktop: History → right-click → Create Tag → Push).
   The version comes from the tag: `v0.0.3-test` builds version 0.0.3, marked as a pre-release.
2. The **Release** workflow builds `Kompacter-0.0.3-setup.exe` (ffmpeg included, NSIS script in
   `apps/kompacter/installer/`) and creates a **draft** release. Check it, then press **Publish**.

3. Once it's published (pre-releases count; drafts don't), installed copies find it within a
   day, download it in the background, check its `.sha256` and install it on their next restart.

You can also start it by hand: Actions → Release → Run workflow (the installer is attached to the run).

The installer is per-user (no admin), into `%LOCALAPPDATA%\Programs\Kompacter`, with a Start menu
entry and an uninstaller in Settings → Apps. ffmpeg's GPL license is installed next to the app as
`FFMPEG-LICENSE.txt`.

## Before calling it "official"

| Step | Why | How |
|---|---|---|
| Code signing | Without it, Windows SmartScreen warns "unknown publisher" | Free for open source via [SignPath Foundation](https://signpath.org), or Azure Trusted Signing (~$10/month) |
| winget | `winget install Kompacter` | Submit a manifest to `microsoft/winget-pkgs` after the first signed release |
| macOS / Linux | More users | Add jobs to `release.yml` (needs Mac/Linux ffmpeg and clipboard support) |
