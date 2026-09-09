#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "macOS capture spike requires Darwin" >&2
  exit 1
fi
duration_seconds="${ECHOWALL_CAPTURE_SPIKE_SECONDS:-8}"
mic_device="${ECHOWALL_CAPTURE_SPIKE_MIC_DEVICE:-BlackHole 2ch}"
system_device="${ECHOWALL_CAPTURE_SPIKE_SYSTEM_DEVICE:-BlackHole 2ch}"
physical_mic="${ECHOWALL_CAPTURE_PHYSICAL_MIC:-0}"
capture_mode="${ECHOWALL_CAPTURE_MODE:-meeting}"
test_tone_sink_confirm="${ECHOWALL_TEST_TONE_SINK_CONFIRM:-}"
pause_seconds=0
source_exit_after="${ECHOWALL_CAPTURE_SPIKE_SOURCE_EXIT_AFTER:-0}"
if [[ -n "${ECHOWALL_CAPTURE_SPIKE_PAUSE_AFTER:-}" ]]; then
  pause_seconds="${ECHOWALL_CAPTURE_SPIKE_PAUSE_SECONDS:-2}"
fi
if [[ ! "$duration_seconds" =~ ^[0-9]+$ ]] ||
   [[ "$duration_seconds" -lt 3 ]] || [[ "$duration_seconds" -gt 7200 ]]; then
  echo "ECHOWALL_CAPTURE_SPIKE_SECONDS must be an integer from 3 to 7200" >&2
  exit 1
fi
if [[ "$physical_mic" != "0" && "$physical_mic" != "1" ]]; then
  echo "ECHOWALL_CAPTURE_PHYSICAL_MIC must be 0 or 1" >&2
  exit 1
fi
if [[ "$physical_mic" == "0" && "$mic_device" == "$system_device" ]]; then
  echo "virtual mic and system fixtures require separate non-monitored devices" >&2
  exit 1
fi
if [[ "$test_tone_sink_confirm" != "non_monitored" ]]; then
  echo "generated system audio requires ECHOWALL_TEST_TONE_SINK_CONFIRM=non_monitored" >&2
  exit 1
fi
case "$capture_mode" in
  meeting) capture_test="generated_meeting_capture_isolated_and_clock_aligned" ;;
  system_capture) capture_test="generated_system_capture_is_clock_aligned" ;;
  *)
    echo "ECHOWALL_CAPTURE_MODE must be meeting or system_capture" >&2
    exit 1
    ;;
esac
if [[ ! "$pause_seconds" =~ ^[0-9]+$ ]] || [[ "$pause_seconds" -gt 10 ]]; then
  echo "ECHOWALL_CAPTURE_SPIKE_PAUSE_SECONDS must be an integer from 0 to 10" >&2
  exit 1
fi
if [[ ! "$source_exit_after" =~ ^[0-9]+$ ]] ||
   [[ "$source_exit_after" -ne 0 &&
      ( "$source_exit_after" -lt 2 || "$source_exit_after" -ge $((duration_seconds - 1)) ) ]]; then
  echo "ECHOWALL_CAPTURE_SPIKE_SOURCE_EXIT_AFTER must be 0 or leave a 2s capture margin" >&2
  exit 1
fi
if [[ "$source_exit_after" -ne 0 && -n "${ECHOWALL_CAPTURE_SPIKE_PAUSE_AFTER:-}" ]]; then
  echo "source-exit and pause fixtures must run separately" >&2
  exit 1
fi
if [[ "$capture_mode" != "meeting" && "$source_exit_after" -ne 0 ]]; then
  echo "source-exit fixture requires Meeting mode" >&2
  exit 1
fi
repo_root=$(cd "$(dirname "$0")/../.." && pwd)
fixture_source="$repo_root/scripts/demo/macos_capture_fixture/SyntheticTone.swift"
fixture_plist="$repo_root/scripts/demo/macos_capture_fixture/Info.plist"
fixture_root=$(mktemp -d -t echowall-mac-capture.XXXXXX)
system_app="$fixture_root/SystemTone.app"
mic_app="$fixture_root/MicTone.app"
system_pid=""
mic_pid=""
caffeinate_pid=""
source_exit_pid=""
capture_started_marker="$fixture_root/capture-started"

cleanup() {
  for process_id in "$system_pid" "$mic_pid" "$caffeinate_pid" "$source_exit_pid"; do
    [[ -z "$process_id" ]] || kill "$process_id" >/dev/null 2>&1 || true
  done
  for process_id in "$system_pid" "$mic_pid" "$caffeinate_pid" "$source_exit_pid"; do
    [[ -z "$process_id" ]] || wait "$process_id" 2>/dev/null || true
  done
  find "$fixture_root" -type l -exec unlink {} \;
  find "$fixture_root" -type f -exec unlink {} \;
  find "$fixture_root" -depth -type d -exec rmdir {} \;
}
trap cleanup EXIT

permissions=$(swift -e 'import CoreGraphics; import AVFoundation; print("\(CGPreflightScreenCaptureAccess()) \(AVCaptureDevice.authorizationStatus(for: .audio).rawValue)")')
if [[ "$permissions" != "true 3" ]]; then
  echo "Screen Recording and microphone permission must already be granted; this harness never prompts" >&2
  exit 1
fi
for device in "$mic_device" "$system_device"; do
  if ! system_profiler SPAudioDataType -json | grep -Fq "\"_name\" : \"$device\""; then
    echo "required fabricated-audio virtual device is missing: $device" >&2
    exit 1
  fi
done

# Compile before starting the duration-bounded fixture Apps. A cold Cargo build
# can outlive the fixtures and make ScreenCaptureKit truthfully report that the
# selected process has already exited.
cargo test --manifest-path "$repo_root/desktop/src-tauri/Cargo.toml" \
  --test macos_capture_spike --no-run

mkdir -p "$system_app/Contents/MacOS" "$fixture_root/capture"
cp "$fixture_plist" "$system_app/Contents/Info.plist"
/usr/libexec/PlistBuddy \
  -c 'Set :CFBundleIdentifier ai.ax.echowall.synthetic-system' \
  -c 'Set :CFBundleName EchoWall Synthetic System' \
  "$system_app/Contents/Info.plist"
swiftc "$fixture_source" \
  -o "$system_app/Contents/MacOS/SyntheticTone" \
  -framework AppKit -framework AVFoundation -framework AudioToolbox -framework CoreAudio
codesign --force --sign - --identifier ai.ax.echowall.synthetic-system "$system_app" >/dev/null
if [[ "$physical_mic" == "0" ]]; then
  mkdir -p "$mic_app/Contents/MacOS"
  cp "$fixture_plist" "$mic_app/Contents/Info.plist"
  /usr/libexec/PlistBuddy \
    -c 'Set :CFBundleIdentifier ai.ax.echowall.synthetic-mic' \
    -c 'Set :CFBundleName EchoWall Synthetic Mic' \
    "$mic_app/Contents/Info.plist"
  cp "$system_app/Contents/MacOS/SyntheticTone" "$mic_app/Contents/MacOS/SyntheticTone"
  codesign --force --sign - --identifier ai.ax.echowall.synthetic-mic "$mic_app" >/dev/null
fi

fixture_duration=$((duration_seconds + pause_seconds + 12))
caffeinate -u -t "$fixture_duration" &
caffeinate_pid=$!
"$system_app/Contents/MacOS/SyntheticTone" \
  --device "$system_device" --frequency 997 --duration "$fixture_duration" \
  >"$fixture_root/system.log" 2>&1 &
system_pid=$!
if [[ "$physical_mic" == "0" ]]; then
  "$mic_app/Contents/MacOS/SyntheticTone" \
    --device "$mic_device" --frequency 443 --duration "$fixture_duration" \
    >"$fixture_root/mic.log" 2>&1 &
  mic_pid=$!
fi
sleep 1
kill -0 "$system_pid"
if [[ "$physical_mic" == "0" ]]; then
  kill -0 "$mic_pid"
fi
if [[ "$source_exit_after" -ne 0 ]]; then
  (
    for _ in {1..300}; do
      [[ ! -f "$capture_started_marker" ]] || break
      sleep 0.1
    done
    [[ -f "$capture_started_marker" ]] || exit 0
    sleep "$source_exit_after"
    kill "$system_pid" >/dev/null 2>&1 || true
  ) &
  source_exit_pid=$!
fi

ECHOWALL_FIXTURE_SYSTEM_PID="$system_pid" \
ECHOWALL_FIXTURE_MIC_DEVICE="$mic_device" \
ECHOWALL_FIXTURE_CAPTURE_STARTED_MARKER="$capture_started_marker" \
ECHOWALL_FIXTURE_DURATION_SECONDS="$duration_seconds" \
ECHOWALL_FIXTURE_OUTPUT="$fixture_root/capture" \
ECHOWALL_CAPTURE_PHYSICAL_MIC="$physical_mic" \
ECHOWALL_CAPTURE_SYSTEM_ONLY="${ECHOWALL_CAPTURE_SYSTEM_ONLY:-0}" \
ECHOWALL_PROCESS_TAP_ENABLED=1 \
  cargo test --manifest-path "$repo_root/desktop/src-tauri/Cargo.toml" \
    --test macos_capture_spike "$capture_test" \
    -- --ignored --nocapture
