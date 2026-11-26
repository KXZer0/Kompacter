#!/usr/bin/env bash
# Notarize a signed Kompacter disk image with Apple and staple the ticket to it.
#
#   APPLE_ID=... APPLE_TEAM_ID=... APPLE_APP_PASSWORD=... packaging/macos/notarize.sh Kompacter.dmg
#
# APPLE_APP_PASSWORD is an app-specific password for APPLE_ID (appleid.apple.com).
# The app inside must be signed with a Developer ID Application identity and the
# hardened runtime (build-app.sh with KOMPACTER_SIGN_IDENTITY).
set -euo pipefail

dmg="$1"
: "${APPLE_ID:?}" "${APPLE_TEAM_ID:?}" "${APPLE_APP_PASSWORD:?}"

result="$(mktemp)"
trap 'rm -f "$result"' EXIT
xcrun notarytool submit "$dmg" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
    --password "$APPLE_APP_PASSWORD" --wait --timeout 1h --output-format json | tee "$result"
status="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["status"])' "$result")"
if [[ "$status" != Accepted ]]; then
    id="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["id"])' "$result")"
    xcrun notarytool log "$id" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
        --password "$APPLE_APP_PASSWORD" || true
    echo "Notarization finished with status: $status" >&2
    exit 1
fi
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"
spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
