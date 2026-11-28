#!/usr/bin/env bash
# Screenshots of every screen of the debug app, as it draws on this system, for review:
#   cargo build -p kompacter && tools/selftest/screens.sh [out-dir] ['{"gpuDrawing": true}']
# Each screen is a separate run with KOMPACTER_DEMO=<screen> and KOMPACTER_SNAPSHOT, so one
# screen can't leave state behind for the next. On Linux without a screen, run it under
# xvfb-run. Fails if any screen can't be captured.
set -euo pipefail
root=$(pwd)
out=$(realpath -m "${1:-$root/target/screens}")
extra=${2:-}
work="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/kompacter-screens"
rm -rf "$work" "$out"
mkdir -p "$work/home" "$out"
exe="$root/target/debug/Kompacter"
[ -f "$exe.exe" ] && exe="$exe.exe"
native() { if command -v cygpath >/dev/null; then cygpath -w "$1"; else echo "$1"; fi; }
case "$(uname -s)" in
  Darwin) cfg="$HOME/Library/Application Support/app.kompacter.desktop" ;;
  MINGW*|MSYS*|CYGWIN*) cfg="$APPDATA/app.kompacter.desktop" ;;
  *) export XDG_CONFIG_HOME="$work/home/.config"; cfg="$XDG_CONFIG_HOME/app.kompacter.desktop" ;;
esac
mkdir -p "$cfg"

screens="idle big trim busy done toobig error settings welcome-preference news update-screen"
failed=0
for look in dark light discord; do
  case $look in
    discord) base='{"style": "discord", "discordTheme": "dark"}' ;;
    *) base="{\"theme\": \"$look\"}" ;;
  esac
  settings=$(python3 -c "
import json, sys
s = {'welcomed': True, 'startDefaultDone': True, 'trayTipSeen': True, 'autoHide': False}
for extra in sys.argv[1:]:
    if extra: s.update(json.loads(extra))
print(json.dumps(s))" "$base" "$extra")
  for screen in $screens; do
    echo "$settings" > "$cfg/settings.json"
    file="$out/$look-$screen.png"
    demo=$screen; [ "$screen" = idle ] && demo=none
    if KOMPACTER_DEMO=$demo KOMPACTER_SNAPSHOT="$(native "$file")" "$exe" >/dev/null 2>"$work/err.txt" && [ -s "$file" ]; then
      echo "ok   $look-$screen"
    else
      echo "FAIL $look-$screen: $(tail -n 3 "$work/err.txt")"
      failed=1
    fi
  done
done
echo "screenshots in $out"
exit $failed
