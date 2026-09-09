#!/bin/sh
set -eu

if [ "${ECHOWALL_LIVE_LONG_MIAOJI_CONFIRM:-}" != "two-hour-fabricated-audio-authorized" ]; then
  echo "long Miaoji provider proof requires explicit authorization" >&2
  exit 2
fi
if [ "${ECHOWALL_TEST_AUDIO_FILE_ONLY_CONFIRM:-}" != "file_only_no_playback" ]; then
  echo "long Miaoji provider proof requires the file-only audio guard" >&2
  exit 2
fi

for variable in \
  VOLC_TOS_ACCESS_KEY \
  VOLC_TOS_SECRET_KEY \
  VOLC_TOS_BUCKET \
  VOLC_TOS_REGION \
  VOLC_TOS_ENDPOINT \
  VOLC_API_KEY \
  GEMINI_API_KEY \
  GEMINI_MODEL; do
  eval "value=\${$variable:-}"
  if [ -z "$value" ]; then
    echo "missing required live credential field: $variable" >&2
    exit 2
  fi
done

script_directory=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_directory/../.." && pwd)
temporary_parent=$(python3 -c 'import tempfile; print(tempfile.gettempdir())')

if [ -n "${ECHOWALL_LIVE_LONG_MIAOJI_ROOT:-}" ]; then
  live_root=$ECHOWALL_LIVE_LONG_MIAOJI_ROOT
  created_root=false
else
  live_root=$(mktemp -d "$temporary_parent/echowall-long-miaoji.XXXXXX")
  created_root=true
fi
case "$live_root" in
  "$temporary_parent"/echowall-long-miaoji.*) ;;
  *)
    echo "long Miaoji root must be an isolated system-temporary directory" >&2
    exit 2
    ;;
esac
if [ ! -d "$live_root" ] || [ -L "$live_root" ]; then
  echo "long Miaoji root must be a real directory" >&2
  exit 2
fi

export ECHOWALL_LIVE_LONG_MIAOJI_ROOT="$live_root"
audio="$live_root/fabricated-provider-long-input.m4a"

if [ ! -f "$audio" ]; then
  zh_voice="$live_root/zh.aiff"
  en_voice="$live_root/en.aiff"
  pattern="$live_root/pattern.m4a"

  # `say -o` renders directly to a file. Nothing is sent to an output device.
  say -v Tingting -o "$zh_voice" \
    "这是回音壁的合成长录音验证。第一位发言人确认上传、轮询和精确清理流程。"
  say -v Samantha -o "$en_voice" \
    "This is a fabricated long recording check. The second speaker confirms the durable provider route."
  ffmpeg -hide_banner -loglevel error -y \
    -i "$zh_voice" \
    -i "$en_voice" \
    -filter_complex \
    '[0:a]aresample=16000,aformat=sample_fmts=fltp:channel_layouts=mono[zh];[1:a]aresample=16000,aformat=sample_fmts=fltp:channel_layouts=mono[en];anullsrc=r=16000:cl=mono:d=5[silence];[zh][silence][en]concat=n=3:v=0:a=1,apad=pad_dur=300[out]' \
    -map '[out]' \
    -t 300 \
    -c:a aac \
    -b:a 32k \
    "$pattern"
  ffmpeg -hide_banner -loglevel error -y \
    -stream_loop 23 \
    -i "$pattern" \
    -t 7140 \
    -c:a aac \
    -b:a 32k \
    "$audio"
  rm -f "$zh_voice" "$en_voice" "$pattern"
fi

duration=$(ffprobe -v error -show_entries format=duration -of default=nw=1:nk=1 "$audio")
size=$(stat -f '%z' "$audio")
echo "fabricated long fixture ready: duration_seconds=$duration bytes=$size"

if cargo test \
  --locked \
  --manifest-path "$repo_root/desktop/src-tauri/Cargo.toml" \
  processing::engine::tests::live_long_miaoji_route_is_durable_rate_limited_and_exactly_cleaned \
  -- \
  --ignored \
  --exact \
  --nocapture; then
  if [ "$created_root" = true ]; then
    find "$live_root" -depth -delete
  fi
  echo "long Miaoji provider proof passed with exact local cleanup"
else
  echo "long Miaoji provider proof stopped; resumable fabricated state retained at $live_root" >&2
  exit 1
fi
