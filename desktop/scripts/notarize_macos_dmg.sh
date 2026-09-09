#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: notarize_macos_dmg.sh <EchoWall.dmg>" >&2
  exit 2
fi

dmg=$1
if [ ! -f "$dmg" ]; then
  echo "macOS disk image is unavailable" >&2
  exit 1
fi

: "${APPLE_API_ISSUER:?APPLE_API_ISSUER is required}"
: "${APPLE_API_KEY:?APPLE_API_KEY is required}"
: "${APPLE_API_KEY_PATH:?APPLE_API_KEY_PATH is required}"

if [ ! -f "$APPLE_API_KEY_PATH" ]; then
  echo "App Store Connect API key file is unavailable" >&2
  exit 1
fi

# Tauri notarizes and staples the .app before it creates the disk image. The
# resulting DMG is signed, but it is a new distribution artifact and must be
# submitted and stapled independently before publication.
codesign --verify --strict "$dmg"
if ! codesign -d --verbose=4 "$dmg" 2>&1 \
  | grep -q '^Authority=Developer ID Application:'; then
  echo "macOS disk image is not signed with a Developer ID Application identity" >&2
  exit 1
fi

xcrun notarytool submit "$dmg" \
  --key "$APPLE_API_KEY_PATH" \
  --key-id "$APPLE_API_KEY" \
  --issuer "$APPLE_API_ISSUER" \
  --wait
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"
spctl -a -t open --context context:primary-signature -vv "$dmg"

echo "macOS disk image notarization verification passed"
