#!/usr/bin/env bash
# The memory self-test (apps/kompacter/src/app/selftest.rs) on this machine: makes a small
# video, audio file, picture, folder and zip with ffmpeg, then runs the debug app on them.
# Run from the repo root after `cargo build -p kompacter`. Needs ffmpeg and python3; on
# Linux without a screen, run it under xvfb-run. Extra settings (e.g. GPU drawing) can be
# passed as JSON: tools/selftest/run.sh '{"gpuDrawing": true}'
set -euo pipefail
root=$(pwd)
work="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/kompacter-selftest"
rm -rf "$work"
mkdir -p "$work/files/folder" "$work/home"
cd "$work/files"
ff=(ffmpeg -loglevel error -y)
"${ff[@]}" -f lavfi -i testsrc2=size=1920x1080:rate=30:duration=8 -f lavfi -i sine=duration=8 \
  -c:v libx264 -preset ultrafast -b:v 20M -c:a aac video.mp4
"${ff[@]}" -f lavfi -i sine=frequency=440:duration=90 -ac 2 -ar 48000 audio.wav
"${ff[@]}" -f lavfi -i "nullsrc=s=4000x3000,geq=random(1)*255:128:128" -frames:v 1 image.png
cp video.mp4 image.png folder/
seq 1 200000 > folder/notes.txt
python3 - <<'PY'
import os, zipfile
with zipfile.ZipFile("archive.zip", "w", zipfile.ZIP_STORED) as z:
    for f in sorted(os.listdir("folder")):
        z.write(os.path.join("folder", f), "folder/" + f)
PY

native() { if command -v cygpath >/dev/null; then cygpath -w "$1"; else echo "$1"; fi; }
list=""
for f in video.mp4 audio.wav image.png folder archive.zip; do
  list+="$(native "$work/files/$f");"
done

# A fresh settings file in a throwaway home, so the run starts from the main screen.
settings='{"welcomed": true, "startDefaultDone": true, "trayTipSeen": true, "autoHide": false}'
if [ -n "${1:-}" ]; then
  settings=$(python3 -c "import json,sys; a=json.loads(sys.argv[1]); a.update(json.loads(sys.argv[2])); print(json.dumps(a))" "$settings" "$1")
fi
case "$(uname -s)" in
  Darwin) cfg="$HOME/Library/Application Support/app.kompacter.desktop" ;;
  MINGW*|MSYS*|CYGWIN*) cfg="$APPDATA/app.kompacter.desktop" ;;
  *) export XDG_CONFIG_HOME="$work/home/.config"; cfg="$XDG_CONFIG_HOME/app.kompacter.desktop" ;;
esac
mkdir -p "$cfg"
echo "$settings" > "$cfg/settings.json"
echo "settings: $settings"

exe="$root/target/debug/Kompacter"
[ -f "$exe.exe" ] && exe="$exe.exe"
KOMPACTER_SELFTEST="$list" "$exe"
