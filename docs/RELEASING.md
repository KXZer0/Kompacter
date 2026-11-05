# Releasing

Installers are built by GitHub Actions; nobody has to build from source.

## Make a release

1. Bump `version` in `apps/desktop/src-tauri/tauri.conf.json` and `Cargo.toml` (workspace).
2. Commit, then tag and push:
   ```bash
   git tag v0.1.0
   git push origin v0.1.0
   ```
3. The **Release** workflow builds `Kompacter_0.1.0_x64-setup.exe` (ffmpeg included) and
   creates a **draft** release with notes. Check it, then press **Publish**.

You can also start it by hand: Actions → Release → Run workflow.

The installer is per-user (no admin). ffmpeg's GPL license is installed next to the app as
`FFMPEG-LICENSE.txt`.

## Before calling it "official"

| Step | Why | How |
|---|---|---|
| Code signing | Without it, Windows SmartScreen warns "unknown publisher" | Free for open source via [SignPath Foundation](https://signpath.org), or Azure Trusted Signing (~$10/month) |
| Auto-updates | Users get new versions without re-downloading | `tauri-plugin-updater` + a signing key stored as a repo secret |
| winget | `winget install Kompacter` | Submit a manifest to `microsoft/winget-pkgs` after the first signed release |
| macOS / Linux | More users | Add jobs to `release.yml` (needs Mac/Linux ffmpeg and clipboard support) |
