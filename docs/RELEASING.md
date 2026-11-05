# Releasing

Installers are built by GitHub Actions; nobody has to build from source.

## Make a release

1. Tag a commit and push the tag (GitHub Desktop: History → right-click → Create Tag → Push).
   The version comes from the tag: `v0.0.3-test` builds version 0.0.3, marked as a pre-release.
2. The **Release** workflow builds `Kompacter-0.0.3-setup.exe` (ffmpeg included, NSIS script in
   `apps/kompacter/installer/`) and creates a **draft** release. Check it, then press **Publish**.

You can also start it by hand: Actions → Release → Run workflow (the installer is attached to the run).

The installer is per-user (no admin), into `%LOCALAPPDATA%\Programs\Kompacter`, with a Start menu
entry and an uninstaller in Settings → Apps. ffmpeg's GPL license is installed next to the app as
`FFMPEG-LICENSE.txt`.

## Before calling it "official"

| Step | Why | How |
|---|---|---|
| Code signing | Without it, Windows SmartScreen warns "unknown publisher" | Free for open source via [SignPath Foundation](https://signpath.org), or Azure Trusted Signing (~$10/month) |
| Auto-updates | Users get new versions without re-downloading | Check GitHub Releases on start, download the new installer, run it |
| winget | `winget install Kompacter` | Submit a manifest to `microsoft/winget-pkgs` after the first signed release |
| macOS / Linux | More users | Add jobs to `release.yml` (needs Mac/Linux ffmpeg and clipboard support) |
