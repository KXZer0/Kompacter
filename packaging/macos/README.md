# macOS packaging

Run on a Mac with Xcode Command Line Tools, Rust, Homebrew, and Python 3. The
bundle targets the build Mac's architecture: Homebrew's ffmpeg is single-arch, so
there is no universal build. The **Release** workflow builds
`Kompacter-<version>-macos-arm64.dmg` on `macos-15` and
`Kompacter-<version>-macos-x86_64.dmg` on `macos-15-intel`; the
**macOS review build** workflow builds the Apple Silicon ZIP and DMG for review.

```sh
brew install ffmpeg
packaging/macos/build-app.sh
packaging/macos/make-dmg.sh target/macos/Kompacter.app target/macos/Kompacter-dev-macos-arm64.dmg
cargo build --locked -p kompacter-cli
packaging/macos/smoke.sh target/macos/Kompacter.app target/debug/kompacter
```

The build leaves `Kompacter.app` and `Kompacter-<architecture>.zip` in
`target/macos/`. The app includes the Homebrew FFmpeg and ffprobe binaries in
`Contents/MacOS` (Kompacter uses the pair next to its own binary), their
non-system dylibs in `Contents/Frameworks`, formula metadata, the FFmpeg license
and build configuration. The DMG holds the app, an Applications shortcut to drag
it onto, and Kompacter's license. The packaging smoke test verifies the signature
and compresses a generated MOV to H.264 using the bundled tools.

## Signing and notarization

`KOMPACTER_SIGN_IDENTITY` (a "Developer ID Application: ..." identity in the
keychain) makes `build-app.sh` sign the dylibs, ffmpeg, ffprobe, Kompacter and the
bundle with the hardened runtime and a secure timestamp, and `make-dmg.sh` sign
the DMG. `notarize.sh` then submits the DMG with `xcrun notarytool submit --wait`
(needs `APPLE_ID`, `APPLE_TEAM_ID` and an app-specific `APPLE_APP_PASSWORD`),
staples the ticket and checks it with `spctl`. Without an identity everything is
ad-hoc signed. The release workflow does all of this when the repository has the
secrets listed in [RELEASING.md](../../docs/RELEASING.md#macos-signing-and-notarization).

## Opening an unsigned (ad-hoc signed) build

Until releases are notarized, macOS refuses the first start of a downloaded build
("Kompacter can't be opened because Apple cannot check it for malicious
software"). Drag Kompacter from the DMG to Applications, then either:

- right-click (or Control-click) Kompacter in Applications, choose **Open**, then
  **Open** again in the dialog (on macOS 15 and later: try to open it once, then
  **System Settings → Privacy & Security → Open Anyway**); or
- run `xattr -dr com.apple.quarantine /Applications/Kompacter.app` in Terminal.

After that it opens normally. To update, download the new DMG and replace the
app in Applications.

## Manual checks

On a Mac, open the extracted app and check the desktop integration:

1. Close the window, then choose **Open Kompacter** from its menu-bar item.
   Close it again and click its Dock icon to reopen the window.
2. Run `open -a /path/to/Kompacter.app "/path/to/Example file.mov"` from a
   cold start, then run it again with a second file while the app is running.
   Finder's **Open With** and dragging a file onto the Dock icon should also
   send the file to the existing app.
3. Copy two files in Finder, then use Kompacter's **Paste** action. Compress
   an item, use **Copy**, and paste in Finder to verify the output is a file.
4. Use **Show in folder** on the output and check that Finder selects it.
