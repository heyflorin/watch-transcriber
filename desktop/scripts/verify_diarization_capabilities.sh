#!/bin/sh
set -eu

if [ "$#" -ne 1 ] || [ ! -f "$1" ] || [ -L "$1" ] || [ ! -x "$1" ]; then
  echo "usage: verify_diarization_capabilities.sh <arm64-or-universal-diarization-worker>" >&2
  exit 2
fi

# This read-only marker gate catches a stale sibling without loading Core ML,
# opening audio, or mistaking a valid old signature for current protocol proof.
# The source contract separately pins Rust/Swift protocol version agreement.
worker_strings=$(strings -a "$1") || exit 1
for marker in schema_version quality_preset speakerkit-pyannote-v3-exclusive-v1 speakerkit-pyannote-v3-exclusive-tail-context-v2 fluid-step015-embed040-v1 fluid-community-v1 ECHOWALL_WORKER_PARENT_PID; do
  if ! printf '%s\n' "$worker_strings" | grep -Fq "$marker"; then
    echo "local diarization worker lacks a required current protocol/preset marker" >&2
    exit 1
  fi
done
echo "local diarization protocol/preset marker verification passed"
