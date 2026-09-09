#!/usr/bin/env bash
set -euo pipefail

adb_bin="${ADB_BIN:-adb}"
target_package="ai.ax.watch_transcriber"
test_package="ai.ax.watch_transcriber.test"
runner="${test_package}/androidx.test.runner.AndroidJUnitRunner"
test_class="ai.ax.watch_transcriber.capture.RecordingProcessDeathHostTest"
repo_root=$(cd "$(dirname "$0")/../.." && pwd)
android_app="$repo_root/desktop/src-tauri/gen/android/app"
app_apk="$android_app/build/outputs/apk/arm64/debug/app-arm64-debug.apk"
test_apk="$android_app/build/outputs/apk/androidTest/arm64/debug/app-arm64-debug-androidTest.apk"
phase_one_log=""
phase_one_host_pid=""

device_list=$("$adb_bin" devices | awk 'NR > 1 && $2 == "device" { print $1 }')
device_count=$(printf '%s\n' "$device_list" | awk 'NF { count += 1 } END { print count + 0 }')
if [[ "$device_count" -ne 1 ]]; then
  echo "expected exactly one connected Android emulator" >&2
  exit 1
fi
serial=$(printf '%s\n' "$device_list" | awk 'NF { print; exit }')
adb_cmd=("$adb_bin" -s "$serial")
if [[ "$("${adb_cmd[@]}" shell getprop ro.kernel.qemu | tr -d '\r')" != "1" ]]; then
  echo "refusing to force-stop a physical Android device" >&2
  exit 1
fi
for artifact in "$app_apk" "$test_apk"; do
  if [[ ! -f "$artifact" ]]; then
    echo "missing Android test artifact: $artifact" >&2
    exit 1
  fi
done
for package_name in "$target_package" "$test_package"; do
  if "${adb_cmd[@]}" shell pm path "$package_name" >/dev/null 2>&1; then
    echo "refusing to replace an existing $package_name install; use a disposable clean emulator" >&2
    exit 1
  fi
done

cleanup() {
  "${adb_cmd[@]}" shell am force-stop "$target_package" >/dev/null 2>&1 || true
  if [[ -n "$phase_one_host_pid" ]]; then
    kill "$phase_one_host_pid" >/dev/null 2>&1 || true
    wait "$phase_one_host_pid" 2>/dev/null || true
  fi
  [[ -z "$phase_one_log" || ! -e "$phase_one_log" ]] || unlink "$phase_one_log"
  "${adb_cmd[@]}" uninstall "$test_package" >/dev/null 2>&1 || true
  "${adb_cmd[@]}" uninstall "$target_package" >/dev/null 2>&1 || true
}
trap cleanup EXIT
"${adb_cmd[@]}" install -r "$app_apk" >/dev/null
"${adb_cmd[@]}" install -r "$test_apk" >/dev/null

start_blocked_phase() {
  local method="$1"
  local marker="$2"
  local ready=false
  phase_one_log=$(mktemp -t echowall-android-process-death.XXXXXX)
  "${adb_cmd[@]}" shell am instrument -w -r \
    -e class "${test_class}#${method}" \
    "$runner" >"$phase_one_log" 2>&1 &
  phase_one_host_pid=$!
  for _ in {1..200}; do
    if "${adb_cmd[@]}" shell run-as "$target_package" test -f "$marker" 2>/dev/null; then
      ready=true
      break
    fi
    if ! kill -0 "$phase_one_host_pid" 2>/dev/null; then
      break
    fi
    sleep 0.1
  done
  if [[ "$ready" != "true" ]]; then
    cat "$phase_one_log" >&2
    echo "Android host phase did not reach its verified marker: $method" >&2
    exit 1
  fi
}

stop_blocked_phase() {
  "${adb_cmd[@]}" shell am force-stop "$target_package"
  wait "$phase_one_host_pid" 2>/dev/null || true
  [[ ! -e "$phase_one_log" ]] || unlink "$phase_one_log"
  phase_one_log=""
  phase_one_host_pid=""
  for _ in {1..20}; do
    [[ -z "$("${adb_cmd[@]}" shell pidof "$target_package" | tr -d '\r')" ]] && break
    sleep 0.1
  done
  if [[ -n "$("${adb_cmd[@]}" shell pidof "$target_package" | tr -d '\r')" ]]; then
    echo "target process survived am force-stop" >&2
    exit 1
  fi
}

"${adb_cmd[@]}" shell am force-stop "$target_package"
start_blocked_phase phaseOneLeavesActiveRecordingForHostKill files/capture/host-kill-ready
before_pid=$("${adb_cmd[@]}" shell pidof "$target_package" | tr -d '\r' || true)
if [[ -z "$before_pid" ]]; then
  echo "recording process was not alive before forced termination" >&2
  exit 1
fi
snapshot=$("${adb_cmd[@]}" shell run-as "$target_package" \
  cat files/capture/active-session.json | tr -d '\r')
if [[ "$snapshot" != *'"state":"RECORDING"'* ]]; then
  printf 'unexpected active snapshot: %s\n' "$snapshot" >&2
  echo "durable RECORDING snapshot was missing before forced termination" >&2
  exit 1
fi

stop_blocked_phase

start_blocked_phase phaseTwoRecoversRecordingInRecreatedProcess \
  files/capture/host-recovery-ready
snapshot=$("${adb_cmd[@]}" shell run-as "$target_package" \
  cat files/capture/active-session.json | tr -d '\r')
if [[ "$snapshot" != *'"state":"INTERRUPTED"'* ]]; then
  printf 'unexpected recovered snapshot: %s\n' "$snapshot" >&2
  echo "Tauri mobile-status path did not persist interrupted recovery" >&2
  exit 1
fi
events=$("${adb_cmd[@]}" shell run-as "$target_package" \
  cat files/capture/session-events.ndjson | tr -d '\r')
if [[ "$events" != *'"kind":"interrupted"'* ]]; then
  echo "durable interruption event was missing after Tauri recovery" >&2
  exit 1
fi
stop_blocked_phase

"${adb_cmd[@]}" shell pm clear "$target_package" >/dev/null
start_blocked_phase nativeImportStagedForHostKill files/host-import-staged
staged_proof=$("${adb_cmd[@]}" shell run-as "$target_package" \
  cat files/host-import-staged | tr -d '\r')
staged_id=$(printf '%s\n' "$staged_proof" | sed -n 's/.*"importId":"\([0-9a-f-]*\)".*/\1/p')
if [[ ! "$staged_id" =~ ^[0-9a-f-]{36}$ ]]; then
  echo "native staging phase returned an invalid import ID" >&2
  exit 1
fi
if ! "${adb_cmd[@]}" shell run-as "$target_package" \
  test -f "files/inbox/$staged_id.wav"; then
  echo "native stable import copy was missing before forced termination" >&2
  exit 1
fi
stop_blocked_phase

start_blocked_phase nativeImportReachesRustInboxAfterProcessDeath files/host-import-ready
recording_id=$("${adb_cmd[@]}" shell run-as "$target_package" \
  cat files/host-import-ready | tr -d '\r')
if [[ ! "$recording_id" =~ ^[0-9a-f-]{36}$ ]]; then
  echo "native-to-Rust phase returned an invalid recording ID" >&2
  exit 1
fi
if ! "${adb_cmd[@]}" shell run-as "$target_package" \
  test -f "inbox/$recording_id/recording.json"; then
  echo "Rust recording envelope was missing after native import adoption" >&2
  exit 1
fi
native_wavs=$("${adb_cmd[@]}" shell run-as "$target_package" \
  find files/inbox -maxdepth 1 -name '*.wav' -type f 2>/dev/null || true)
if [[ -n "$native_wavs" ]]; then
  echo "native staging audio remained after Rust acknowledgement" >&2
  exit 1
fi
stop_blocked_phase

echo "Android forced process-death and native-to-Rust import recovery passed on emulator $serial"
