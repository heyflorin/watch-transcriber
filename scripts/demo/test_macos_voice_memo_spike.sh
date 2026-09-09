#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "macOS Voice Memo spike requires Darwin" >&2
  exit 1
fi
if [[ "${ECHOWALL_AMBIENT_MIC_CONFIRM:-}" != "authorized" ]]; then
  echo "physical microphone capture requires ECHOWALL_AMBIENT_MIC_CONFIRM=authorized" >&2
  exit 1
fi

duration_seconds="${ECHOWALL_AMBIENT_MIC_SECONDS:-5}"
microphone="${ECHOWALL_AMBIENT_MIC_DEVICE:-MacBook Pro Microphone}"
if [[ ! "$duration_seconds" =~ ^[0-9]+$ ]] ||
   [[ "$duration_seconds" -lt 3 ]] || [[ "$duration_seconds" -gt 7200 ]]; then
  echo "ECHOWALL_AMBIENT_MIC_SECONDS must be an integer from 3 to 7200" >&2
  exit 1
fi

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
fixture_root=$(mktemp -d -t echowall-voice-memo.XXXXXX)
case "$fixture_root" in
  /tmp/*|/private/tmp/*|/var/folders/*|/private/var/folders/*) ;;
  *)
    echo "Voice Memo fixture root must be under the system temp directory" >&2
    exit 1
    ;;
esac

cleanup() {
  find "$fixture_root" -type l -exec unlink {} \;
  find "$fixture_root" -type f -exec unlink {} \;
  find "$fixture_root" -depth -type d -exec rmdir {} \;
}
trap cleanup EXIT

cargo test --manifest-path "$repo_root/desktop/src-tauri/Cargo.toml" \
  --test macos_capture_spike --no-run

ECHOWALL_AMBIENT_MIC_CONFIRM=authorized \
ECHOWALL_AMBIENT_MIC_SECONDS="$duration_seconds" \
ECHOWALL_AMBIENT_MIC_DEVICE="$microphone" \
ECHOWALL_AMBIENT_MIC_OUTPUT="$fixture_root" \
  cargo test --manifest-path "$repo_root/desktop/src-tauri/Cargo.toml" \
    --test macos_capture_spike \
    authorized_voice_memo_captures_nonzero_physical_microphone_audio \
    -- --exact --ignored --nocapture
