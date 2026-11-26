# macOS review build

Run on a Mac with Xcode Command Line Tools, Rust, Homebrew, and Python 3. The
bundle targets the build Mac's architecture. GitHub Actions builds the Apple
Silicon ZIP on macOS 15 and attaches it to the **macOS review build** run.

```sh
brew install ffmpeg
packaging/macos/build-app.sh
cargo build --locked -p kompacter-cli
packaging/macos/smoke.sh target/macos/Kompacter.app target/debug/kompacter
```

The build leaves `Kompacter.app` and `Kompacter-<architecture>.zip` in
`target/macos/`. The ZIP is ad-hoc signed for review. Developer ID signing,
notarization, and public distribution are outside this build. The app includes
the Homebrew FFmpeg and ffprobe binaries, their non-system dylibs, formula
metadata, and the FFmpeg build configuration. The packaging smoke test verifies
the signature and compresses a generated MOV to H.264 using the bundled tools.

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
