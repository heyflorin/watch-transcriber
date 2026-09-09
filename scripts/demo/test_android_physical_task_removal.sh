#!/usr/bin/env bash
set -euo pipefail

if [[ "${ECHOWALL_ANDROID_PHYSICAL_CONFIRM:-}" != "authorized" ]]; then
  echo "physical Android task-removal proof requires ECHOWALL_ANDROID_PHYSICAL_CONFIRM=authorized" >&2
  exit 1
fi
if [[ "${ECHOWALL_ANDROID_TASK_REMOVAL_LOCK_RISK:-}" != "authorized" ]]; then
  echo "task removal may trigger an OEM keyguard; explicit lock-risk authorization is required" >&2
  exit 1
fi

adb_bin="${ADB_BIN:-adb}"
target_package="ai.ax.watch_transcriber"
test_package="ai.ax.watch_transcriber.test"
runner="${test_package}/androidx.test.runner.AndroidJUnitRunner"
test_class="ai.ax.watch_transcriber.capture.RecordingProcessDeathHostTest"
phase_log=""
phase_host_pid=""
original_stay_awake=""

device_list=$("$adb_bin" devices | awk 'NR > 1 && $2 == "device" { print $1 }')
device_count=$(printf '%s\n' "$device_list" | awk 'NF { count += 1 } END { print count + 0 }')
if [[ "$device_count" -ne 1 ]]; then
  echo "expected exactly one connected physical Android device" >&2
  exit 1
fi
serial=$(printf '%s\n' "$device_list" | awk 'NF { print; exit }')
adb_cmd=("$adb_bin" -s "$serial")
if [[ "$("${adb_cmd[@]}" shell getprop ro.kernel.qemu | tr -d '\r')" == "1" ]]; then
  echo "physical task-removal proof refuses Android emulators" >&2
  exit 1
fi
for package_name in "$target_package" "$test_package"; do
  if ! "${adb_cmd[@]}" shell pm path "$package_name" >/dev/null 2>&1; then
    echo "required installed package is missing: $package_name" >&2
    exit 1
  fi
done
current_focus=$("${adb_cmd[@]}" shell dumpsys window | grep -m 1 'mCurrentFocus=' | tr -d '\r')
if [[ "$current_focus" != *"$target_package"* ]]; then
  echo "unlock the device and leave EchoWall visibly in front before this proof" >&2
  exit 1
fi
capture_file_count=$("${adb_cmd[@]}" shell run-as "$target_package" \
  find files/capture -type f 2>/dev/null | wc -l | tr -d ' ')
if [[ "$capture_file_count" -ne 0 ]]; then
  echo "physical task-removal proof requires an empty App capture root" >&2
  exit 1
fi
original_stay_awake=$("${adb_cmd[@]}" shell settings get global stay_on_while_plugged_in | tr -d '\r')
if [[ "$original_stay_awake" != "0" ]]; then
  echo "physical task-removal proof requires the default stay-awake value 0" >&2
  exit 1
fi

run_cleanup_phase() {
  if "${adb_cmd[@]}" shell run-as "$target_package" \
    test -f files/physical-task-removal-ready 2>/dev/null ||
     "${adb_cmd[@]}" shell run-as "$target_package" \
    test -f files/physical-task-removal-survived 2>/dev/null; then
    "${adb_cmd[@]}" shell am instrument -w -r \
      -e class "${test_class}#cleanupPhysicalTaskRemovalFixture" \
      "$runner" >/dev/null
  fi
}

cleanup() {
  "${adb_cmd[@]}" shell am force-stop "$target_package" >/dev/null 2>&1 || true
  if [[ -n "$phase_host_pid" ]]; then
    wait "$phase_host_pid" 2>/dev/null || true
  fi
  run_cleanup_phase || true
  "${adb_cmd[@]}" shell svc power stayon false >/dev/null 2>&1 || true
  [[ -z "$phase_log" || ! -e "$phase_log" ]] || unlink "$phase_log"
}
trap cleanup EXIT

"${adb_cmd[@]}" shell svc power stayon usb
phase_log=$(mktemp -t echowall-android-physical-task.XXXXXX)
"${adb_cmd[@]}" shell am instrument -w -r \
  -e class "${test_class}#physicalTaskRemovalLeavesForegroundRecorderAlive" \
  "$runner" >"$phase_log" 2>&1 &
phase_host_pid=$!

survived=false
for _ in {1..200}; do
  if "${adb_cmd[@]}" shell run-as "$target_package" \
    test -f files/physical-task-removal-survived 2>/dev/null; then
    survived=true
    break
  fi
  if ! kill -0 "$phase_host_pid" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
if [[ "$survived" != "true" ]]; then
  cat "$phase_log" >&2
  echo "ColorOS did not preserve the foreground recorder across task removal" >&2
  exit 1
fi

session_id=$("${adb_cmd[@]}" shell run-as "$target_package" \
  cat files/physical-task-removal-survived | tr -d '\r')
if [[ ! "$session_id" =~ ^[0-9a-f-]{36}$ ]]; then
  echo "physical task-removal marker contains an invalid session ID" >&2
  exit 1
fi
before_pid=$("${adb_cmd[@]}" shell pidof "$target_package" | tr -d '\r')
if [[ -z "$before_pid" ]]; then
  echo "foreground recorder process is absent after task removal" >&2
  exit 1
fi
if ! "${adb_cmd[@]}" shell dumpsys activity services \
  "$target_package/.capture.RecordingService" | grep -q 'ServiceRecord'; then
  echo "foreground recorder service is absent after task removal" >&2
  exit 1
fi
segment="files/capture/sessions/$session_id/segment-0000.wav"
before_bytes=$("${adb_cmd[@]}" shell run-as "$target_package" stat -c %s "$segment" | tr -d '\r')
sleep 2
after_bytes=$("${adb_cmd[@]}" shell run-as "$target_package" stat -c %s "$segment" | tr -d '\r')
if [[ ! "$before_bytes" =~ ^[0-9]+$ || ! "$after_bytes" =~ ^[0-9]+$ ||
      "$after_bytes" -le "$before_bytes" ]]; then
  snapshot=$("${adb_cmd[@]}" shell run-as "$target_package" \
    cat files/capture/active-session.json 2>/dev/null | tr -d '\r' || true)
  events=$("${adb_cmd[@]}" shell run-as "$target_package" \
    tail -n 1 files/capture/session-events.ndjson 2>/dev/null | tr -d '\r' || true)
  state=$(printf '%s\n' "$snapshot" | sed -n 's/.*"state":"\([A-Z_]*\)".*/\1/p')
  event_kind=$(printf '%s\n' "$events" | sed -n 's/.*"kind":"\([a-z_]*\)".*/\1/p')
  event_code=$(printf '%s\n' "$events" | sed -n 's/.*"code":"\([a-z_]*\)".*/\1/p')
  current_pid=$("${adb_cmd[@]}" shell pidof "$target_package" | tr -d '\r' || true)
  service_count=$("${adb_cmd[@]}" shell dumpsys activity services \
    "$target_package/.capture.RecordingService" | grep -c 'ServiceRecord' || true)
  printf 'failure_state=%s event_kind=%s event_code=%s pid_present=%s service_count=%s bytes=%s->%s\n' \
    "${state:-unknown}" "${event_kind:-unknown}" "${event_code:-unknown}" \
    "$([[ -n "$current_pid" ]] && echo true || echo false)" "$service_count" \
    "$before_bytes" "$after_bytes" >&2
  echo "foreground recorder WAV did not continue growing after task removal" >&2
  exit 1
fi

"${adb_cmd[@]}" shell am force-stop "$target_package"
wait "$phase_host_pid" 2>/dev/null || true
phase_host_pid=""
run_cleanup_phase
remaining=$("${adb_cmd[@]}" shell run-as "$target_package" \
  find files/capture -type f 2>/dev/null | wc -l | tr -d ' ')
if [[ "$remaining" -ne 0 ]]; then
  echo "physical task-removal fixture cleanup left capture files" >&2
  exit 1
fi
"${adb_cmd[@]}" shell svc power stayon false
"${adb_cmd[@]}" shell am start -n "$target_package/.MainActivity" >/dev/null
trap - EXIT
[[ ! -e "$phase_log" ]] || unlink "$phase_log"
phase_log=""
echo "Android physical task-removal foreground recording passed on $serial: bytes $before_bytes -> $after_bytes"
