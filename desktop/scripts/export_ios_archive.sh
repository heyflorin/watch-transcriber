#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
DESKTOP_DIR=$(dirname -- "$SCRIPT_DIR")

ARCHIVE_PATH=${1:-"$DESKTOP_DIR/src-tauri/gen/apple/build/desktop_iOS.xcarchive"}
EXPORT_OPTIONS=${2:-"$DESKTOP_DIR/src-tauri/gen/apple/ExportOptions.plist"}
EXPORT_PATH=${3:-"$DESKTOP_DIR/src-tauri/gen/apple/build/manual-ipa"}

if [ ! -d "$ARCHIVE_PATH" ]; then
  echo "iOS archive does not exist: $ARCHIVE_PATH" >&2
  exit 66
fi

if [ ! -f "$EXPORT_OPTIONS" ]; then
  echo "iOS export options do not exist: $EXPORT_OPTIONS" >&2
  exit 66
fi

if [ -e "$EXPORT_PATH" ]; then
  echo "iOS export path already exists; refusing to reuse stale output: $EXPORT_PATH" >&2
  exit 73
fi

mkdir -p "$(dirname -- "$EXPORT_PATH")"

# Xcode 26 invokes Apple's /usr/bin/rsync as the client, then resolves the
# local rsync server through PATH. A Homebrew rsync there is protocol-compatible
# but rejects Apple's --extended-attributes option, surfacing only as
# `exportArchive Copy failed`. Keep both halves on the Apple system toolchain.
SYSTEM_PATH=/usr/bin:/bin:/usr/sbin:/sbin:/Library/Apple/usr/bin
env PATH="$SYSTEM_PATH" /usr/bin/xcodebuild \
  -exportArchive \
  -archivePath "$ARCHIVE_PATH" \
  -exportOptionsPlist "$EXPORT_OPTIONS" \
  -exportPath "$EXPORT_PATH"
