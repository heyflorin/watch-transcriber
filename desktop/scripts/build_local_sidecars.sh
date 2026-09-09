#!/bin/sh
set -eu

script_directory=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
desktop_directory=$(CDPATH='' cd -- "$script_directory/.." && pwd)
worker_manifest="$desktop_directory/whisper-worker/Cargo.toml"
worker_target="$desktop_directory/whisper-worker/target"
diarization_package="$desktop_directory/diarization-worker"
summary_manifest="$desktop_directory/summary-worker/Cargo.toml"
summary_target="$desktop_directory/summary-worker/target"
qwen_manifest="$desktop_directory/qwen-worker/Cargo.toml"
qwen_target="$desktop_directory/qwen-worker/target"
moss_manifest="$desktop_directory/moss-worker/Cargo.toml"
moss_target="$desktop_directory/moss-worker/target"
bundle_binaries="$desktop_directory/src-tauri/binaries"
whisper_binary=echowall-whisper-worker
diarization_binary=echowall-diarization-worker
summary_binary=echowall-summary-worker
qwen_binary=echowall-qwen-worker
moss_binary=echowall-moss-worker

dry_run=0
case "${1:-}" in
  --dry-run) [ "$#" -eq 1 ] || exit 2; dry_run=1 ;;
  '') [ "$#" -eq 0 ] || exit 2 ;;
  *) echo "usage: build_local_sidecars.sh [--dry-run]" >&2; exit 2 ;;
esac

# Metadata only; catches lock/native/config drift before any compiler runs.
node "$script_directory/verify_local_sidecar_contract.mjs"

run() {
  if [ "$dry_run" -eq 1 ]; then
    printf 'dry-run:'
    printf ' %s' "$@"
    printf '\n'
  else
    "$@"
  fi
}

verify_no_network_linkage() {
  candidate=$1
  if [ "$dry_run" -eq 1 ]; then
    echo "dry-run: verify no network linkage for $candidate"
    return
  fi
  dependencies=$(otool -L "$candidate") || exit 1
  symbols=$(nm -u "$candidate") || exit 1
  if printf '%s\n' "$dependencies" | grep -Eq '/(CFNetwork|Network)\.framework'; then
    echo "local inference worker unexpectedly links a network framework" >&2
    exit 1
  fi
  if printf '%s\n' "$symbols" | grep -Eq 'URLSession|NSURLSession|CFNetwork|_nw_|_socket|_connect|_getaddrinfo|curl'; then
    echo "local inference worker unexpectedly imports a network symbol" >&2
    exit 1
  fi
}

make_universal_binary() {
  binary_name=$1
  arm_binary="$bundle_binaries/$binary_name-aarch64-apple-darwin"
  intel_binary="$bundle_binaries/$binary_name-x86_64-apple-darwin"
  universal_binary="$bundle_binaries/$binary_name-universal-apple-darwin"
  temporary_binary="$universal_binary.tmp.$$"

  if [ "$dry_run" -eq 1 ]; then
    run xcrun lipo -create "$arm_binary" "$intel_binary" -output "$universal_binary"
    run xcrun lipo "$universal_binary" -verify_arch arm64 x86_64
    verify_no_network_linkage "$universal_binary"
    return
  fi

  trap 'rm -f "$temporary_binary"' EXIT HUP INT TERM
  xcrun lipo -create \
    "$arm_binary" \
    "$intel_binary" \
    -output "$temporary_binary"
  xcrun lipo "$temporary_binary" -verify_arch arm64 x86_64
  chmod 755 "$temporary_binary"
  mv -f "$temporary_binary" "$universal_binary"
  trap - EXIT HUP INT TERM
  verify_no_network_linkage "$universal_binary"
}

run mkdir -p "$bundle_binaries"

# The universal desktop App still supports Intel Macs, where local STT is
# deliberately unavailable in Release 1. Build a tiny x86_64 stub from the
# same crate and the full Metal worker for Apple Silicon; Tauri/lipo can then
# produce a universal signed sidecar without making an Intel inference claim.
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  run cargo build \
    --locked \
    --manifest-path "$worker_manifest" \
    --release \
    --target "$target"
  run install -m 755 \
    "$worker_target/$target/release/$whisper_binary" \
    "$bundle_binaries/$whisper_binary-$target"
  run cargo build \
    --locked \
    --manifest-path "$summary_manifest" \
    --release \
    --target "$target"
  run install -m 755 \
    "$summary_target/$target/release/$summary_binary" \
    "$bundle_binaries/$summary_binary-$target"
  run cargo build \
    --locked \
    --manifest-path "$qwen_manifest" \
    --release \
    --target "$target"
  run install -m 755 \
    "$qwen_target/$target/release/$qwen_binary" \
    "$bundle_binaries/$qwen_binary-$target"
  # Same standalone crate: arm64 Metal inference, x86_64 explicit unsupported
  # stub. Native C++ remains outside the Tauri main process in both slices.
  run cargo build \
    --locked \
    --manifest-path "$moss_manifest" \
    --release \
    --target "$target"
  run install -m 755 \
    "$moss_target/$target/release/$moss_binary" \
    "$bundle_binaries/$moss_binary-$target"
done
verify_no_network_linkage \
  "$worker_target/aarch64-apple-darwin/release/$whisper_binary"
verify_no_network_linkage \
  "$summary_target/aarch64-apple-darwin/release/$summary_binary"
verify_no_network_linkage \
  "$qwen_target/aarch64-apple-darwin/release/$qwen_binary"
verify_no_network_linkage \
  "$moss_target/aarch64-apple-darwin/release/$moss_binary"
if [ "$dry_run" -eq 1 ]; then
  echo "dry-run: require explicit unsupported_platform marker in Intel MOSS stub"
else
  intel_moss_strings=$(strings -a "$moss_target/x86_64-apple-darwin/release/$moss_binary") || exit 1
  if ! printf '%s\n' "$intel_moss_strings" | grep -q 'unsupported_platform'; then
    echo "Intel MOSS sidecar is missing its explicit unsupported stub" >&2
    exit 1
  fi
fi

run swift build \
  --package-path "$diarization_package" \
  --configuration release \
  --product "$diarization_binary" \
  --arch arm64 \
  -Xswiftc -gnone \
  -Xcc -g0 \
  -Xcxx -g0
diarization_release="$diarization_package/.build/arm64-apple-macosx/release/$diarization_binary"

# This is a release invariant, not just a behavioral promise. The vendored
# target contains only direct local Core ML loading and the offline pipeline;
# fail packaging if a downloader or network framework enters the worker.
verify_no_network_linkage "$diarization_release"
run sh "$script_directory/verify_diarization_capabilities.sh" "$diarization_release"
if [ "$dry_run" -eq 0 ] && strings -a "$diarization_release" | grep -Eiq 'https?://|huggingface|modelhub|download(ing|ed)? .*model'; then
  echo "local diarization worker unexpectedly contains a downloader marker" >&2
  exit 1
fi
run install -m 755 \
  "$diarization_release" \
  "$bundle_binaries/$diarization_binary-aarch64-apple-darwin"
run cmp -s "$diarization_release" "$bundle_binaries/$diarization_binary-aarch64-apple-darwin"
run sh "$script_directory/verify_diarization_capabilities.sh" "$bundle_binaries/$diarization_binary-aarch64-apple-darwin"

# Local diarization is an Apple-Silicon/macOS-14+ product feature. The universal
# desktop bundle retains its Intel slice through this explicit fail-closed stub.
run xcrun clang \
  -arch x86_64 \
  -mmacosx-version-min=13.0 \
  "$diarization_package/stub.c" \
  -o "$bundle_binaries/$diarization_binary-x86_64-apple-darwin"

# Tauri resolves external binaries by the requested target triple. Its
# universal application build does not merge sidecars for us, so create and
# verify an explicit two-slice artifact for each fixed worker name.
make_universal_binary "$whisper_binary"
make_universal_binary "$diarization_binary"
make_universal_binary "$summary_binary"
make_universal_binary "$qwen_binary"
make_universal_binary "$moss_binary"
run sh "$script_directory/verify_diarization_capabilities.sh" "$bundle_binaries/$diarization_binary-universal-apple-darwin"
