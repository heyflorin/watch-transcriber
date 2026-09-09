#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "macOS process-tap spike requires Darwin" >&2
  exit 1
fi
if [[ "${ECHOWALL_TEST_TONE_SINK_CONFIRM:-}" != "non_monitored" ]]; then
  echo "generated system audio requires ECHOWALL_TEST_TONE_SINK_CONFIRM=non_monitored" >&2
  exit 1
fi
duration_seconds="${ECHOWALL_CAPTURE_SPIKE_SECONDS:-8}"
if [[ ! "$duration_seconds" =~ ^[0-9]+$ ]] ||
   [[ "$duration_seconds" -lt 3 ]] || [[ "$duration_seconds" -gt 1800 ]]; then
  echo "ECHOWALL_CAPTURE_SPIKE_SECONDS must be an integer from 3 to 1800" >&2
  exit 1
fi
repo_root=$(cd "$(dirname "$0")/../.." && pwd)
tone_source="$repo_root/scripts/demo/macos_capture_fixture/SyntheticTone.swift"
tone_plist="$repo_root/scripts/demo/macos_capture_fixture/Info.plist"
tap_source="$repo_root/scripts/demo/macos_process_tap_fixture/ProcessTapCapture.swift"
tap_plist="$repo_root/scripts/demo/macos_process_tap_fixture/Info.plist"
fixture_root=$(mktemp -d -t echowall-process-tap.XXXXXX)
system_app="$fixture_root/SystemTone.app"
unrelated_app="$fixture_root/UnrelatedTone.app"
capture_app="$fixture_root/ProcessTapCapture.app"
system_pid=""
unrelated_pid=""

cleanup() {
  for process_id in "$system_pid" "$unrelated_pid"; do
    [[ -z "$process_id" ]] || kill "$process_id" >/dev/null 2>&1 || true
  done
  for process_id in "$system_pid" "$unrelated_pid"; do
    [[ -z "$process_id" ]] || wait "$process_id" 2>/dev/null || true
  done
  find "$fixture_root" -type l -exec unlink {} \;
  find "$fixture_root" -type f -exec unlink {} \;
  find "$fixture_root" -depth -type d -exec rmdir {} \;
}
trap cleanup EXIT

for device in "BlackHole 2ch" "Steam Streaming Speakers"; do
  if ! system_profiler SPAudioDataType -json | grep -Fq "\"_name\" : \"$device\""; then
    echo "required fabricated-audio virtual device is missing: $device" >&2
    exit 1
  fi
done

mkdir -p "$system_app/Contents/MacOS" "$unrelated_app/Contents/MacOS" \
  "$capture_app/Contents/MacOS"
cp "$tone_plist" "$system_app/Contents/Info.plist"
cp "$tone_plist" "$unrelated_app/Contents/Info.plist"
cp "$tap_plist" "$capture_app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c 'Set :CFBundleIdentifier ai.ax.echowall.tap-system' \
  -c 'Set :CFBundleName EchoWall Tap System' "$system_app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c 'Set :CFBundleIdentifier ai.ax.echowall.tap-unrelated' \
  -c 'Set :CFBundleName EchoWall Tap Unrelated' "$unrelated_app/Contents/Info.plist"
swiftc "$tone_source" -o "$system_app/Contents/MacOS/SyntheticTone" \
  -framework AppKit -framework AVFoundation -framework AudioToolbox -framework CoreAudio
cp "$system_app/Contents/MacOS/SyntheticTone" "$unrelated_app/Contents/MacOS/SyntheticTone"
swiftc -parse-as-library "$tap_source" -o "$capture_app/Contents/MacOS/ProcessTapCapture" \
  -framework AVFoundation -framework AudioToolbox -framework CoreAudio
codesign --force --sign - --identifier ai.ax.echowall.tap-system "$system_app" >/dev/null
codesign --force --sign - --identifier ai.ax.echowall.tap-unrelated "$unrelated_app" >/dev/null
codesign --force --sign - --identifier ai.ax.echowall.process-tap-spike "$capture_app" >/dev/null

fixture_duration=$((duration_seconds + 5))
"$system_app/Contents/MacOS/SyntheticTone" --device "Steam Streaming Speakers" \
  --frequency 997 --duration "$fixture_duration" >"$fixture_root/system.log" 2>&1 &
system_pid=$!
"$unrelated_app/Contents/MacOS/SyntheticTone" --device "BlackHole 2ch" \
  --frequency 443 --duration "$fixture_duration" >"$fixture_root/unrelated.log" 2>&1 &
unrelated_pid=$!
sleep 1
kill -0 "$system_pid"
kill -0 "$unrelated_pid"

"$capture_app/Contents/MacOS/ProcessTapCapture" \
  --pid "$system_pid" --duration "$duration_seconds" --output "$fixture_root/selected.wav"
