#!/usr/bin/env bash
set -euo pipefail

app="$1"
cli="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
media="$(cd "$app/Contents/MacOS" && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

codesign --verify --deep --strict "$app"
# Place inputs and outputs in paths containing spaces and use only the
# app's media binaries plus system utilities.
export PATH="$media:/usr/bin:/bin"
export KOMPACTER_FFMPEG="$media/ffmpeg"
export KOMPACTER_FFPROBE="$media/ffprobe"
input="$fixture/Finder media.mov"
"$KOMPACTER_FFMPEG" -hide_banner -loglevel error -f lavfi \
    -i 'testsrc2=size=640x360:rate=30:duration=3' -c:v mjpeg -q:v 2 "$input"
mkdir "$fixture/Compressed files"
"$cli" compress "$input" --target 200kb --cpu --out "$fixture/Compressed files"
outputs=("$fixture/Compressed files/"*.mp4)
[[ "${#outputs[@]}" -eq 1 && -s "${outputs[0]}" ]]
[[ "$(stat -f '%z' "${outputs[0]}")" -le 200000 ]]
[[ "$("$KOMPACTER_FFPROBE" -v error -select_streams v:0 \
    -show_entries stream=codec_name -of default=nw=1:nk=1 "${outputs[0]}")" = h264 ]]
