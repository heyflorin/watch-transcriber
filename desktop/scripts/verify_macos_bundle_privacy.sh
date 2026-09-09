#!/bin/sh
set -eu

script_directory=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

if [ "$#" -ne 1 ]; then
  echo "usage: verify_macos_bundle_privacy.sh <EchoWall.app>" >&2
  exit 2
fi

fail() {
  echo "$1" >&2
  exit 1
}

bundle=$1
macos_directory="$bundle/Contents/MacOS"
[ -d "$macos_directory" ] && [ ! -L "$macos_directory" ] || fail "macOS App bundle is unavailable"

# Count alone can admit a missing MOSS worker replaced by an unrelated file.
workers='echowall-whisper-worker echowall-diarization-worker echowall-summary-worker echowall-qwen-worker echowall-moss-worker'
for name in $workers; do
  [ -f "$macos_directory/$name" ] && [ ! -L "$macos_directory/$name" ] && [ -x "$macos_directory/$name" ] \
    || fail "macOS App bundle must contain all five fixed local workers"
done
for worker in "$macos_directory"/echowall-*-worker; do
  case "$(basename "$worker")" in
    echowall-whisper-worker|echowall-diarization-worker|echowall-summary-worker|echowall-qwen-worker|echowall-moss-worker) ;;
    *) fail "macOS App bundle contains an unexpected local worker" ;;
  esac
done

for license in transcribe.cpp-LICENSE transcribe.cpp-THIRD-PARTY-LICENSES.md; do
  license_path="$bundle/Contents/Resources/licenses/$license"
  [ -s "$license_path" ] && [ ! -L "$license_path" ] || fail "MOSS native license notice is missing"
  case "$license" in
    transcribe.cpp-LICENSE) expected_license_hash=86a53633b56f6b029d3cb42158bcc7aac0cdff898aceb13e83b93e368bbc4ac6 ;;
    transcribe.cpp-THIRD-PARTY-LICENSES.md) expected_license_hash=6b55185de37d7a24be30e2db7f2fd41270f73d5609eefc073592ecb2a527c85c ;;
  esac
  actual_license_hash=$(shasum -a 256 "$license_path") || fail "MOSS native license inspection failed"
  [ "${actual_license_hash%% *}" = "$expected_license_hash" ] || fail "MOSS native license notice does not match its pinned text"
done

forbidden_files=$(find "$bundle" -type f \( \
  -name '*.p8' -o -name '*.p12' -o -name '*.mobileprovision' -o \
  -name '*.key' -o -name '*.py' -o -name '*.gguf' -o \
  -name '*.safetensors' \
\)) || fail "macOS App resource inspection failed"
if [ -n "$forbidden_files" ]; then
  fail "macOS App bundle contains a forbidden credential, Python, or model artifact"
fi

for executable in "$macos_directory"/*; do
  [ -f "$executable" ] || continue
  executable_strings=$(strings -a "$executable") || fail "macOS executable string inspection failed"
  if printf '%s\n' "$executable_strings" | grep -Fq 'echowall-isolated-qa-v1'; then
    fail "isolated QA builds are not release artifacts"
  fi
  if printf '%s\n' "$executable_strings" | grep -Eq \
    '/Users/|/home/runner/|/private/var/folders/|/var/folders/|BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY|TKT7W3R9WU|3bfb636b-b03d-4445-b18c-95e64c4fd1bd'; then
    fail "macOS executable contains a builder path or credential marker: $(basename "$executable")"
  fi
done

main_executable=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$bundle/Contents/Info.plist") \
  || fail "macOS App executable declaration is unavailable"
case "$main_executable" in ''|*/*|.|..) fail "macOS App executable declaration is invalid" ;; esac
[ -f "$macos_directory/$main_executable" ] && [ ! -L "$macos_directory/$main_executable" ] \
  || fail "macOS App executable is unavailable"
main_arches=$(xcrun lipo -archs "$macos_directory/$main_executable") || fail "macOS App architecture inspection failed"
[ -n "$main_arches" ] || fail "macOS App has no supported architecture"
for architecture in $main_arches; do
  case "$architecture" in arm64|x86_64) ;; *) fail "macOS App architecture is unsupported" ;; esac
done
main_arch_set=$(printf '%s\n' "$main_arches" | tr ' ' '\n' | sort -u)
if printf '%s\n' "$main_arch_set" | grep -qx arm64; then
  sh "$script_directory/verify_diarization_capabilities.sh" "$macos_directory/echowall-diarization-worker" > /dev/null \
    || fail "bundled diarization worker is stale or lacks current protocol/preset support"
fi

for name in $workers; do
  worker="$macos_directory/$name"
  dependencies=$(otool -L "$worker") || fail "local inference worker linkage inspection failed: $name"
  symbols=$(nm -u "$worker") || fail "local inference worker symbol inspection failed: $name"
  if printf '%s\n' "$dependencies" | grep -Eq '/(CFNetwork|Network)\.framework'; then
    fail "local inference worker links a network framework: $name"
  fi
  if printf '%s\n' "$symbols" | grep -Eq 'URLSession|NSURLSession|CFNetwork|_nw_|_socket|_connect|_getaddrinfo|curl'; then
    fail "local inference worker imports a network symbol: $name"
  fi
  worker_arches=$(xcrun lipo -archs "$worker") || fail "local inference worker architecture inspection failed: $name"
  worker_arch_set=$(printf '%s\n' "$worker_arches" | tr ' ' '\n' | sort -u)
  [ "$worker_arch_set" = "$main_arch_set" ] || fail "local inference worker architectures do not match the App: $name"
  if printf '%s\n' "$main_arch_set" | grep -qx arm64; then
    case "$name" in
      echowall-moss-worker|echowall-summary-worker|echowall-diarization-worker)
        owner_strings=$(strings -a "$worker") || fail "worker ownership inspection failed: $name"
        printf '%s\n' "$owner_strings" | grep -Fq ECHOWALL_WORKER_PARENT_PID \
          || fail "local worker lacks App-owner lifetime support: $name"
        ;;
    esac
  fi

  # Tauri signs every externalBin under the same hardened-runtime policy as
  # the current one-shot Metal peers. No JIT, library-validation bypass,
  # microphone, screen-recording or network entitlement is needed by a worker.
  codesign --verify --strict "$worker" || fail "local inference worker signature is invalid: $name"
  signature=$(codesign -d --verbose=4 "$worker" 2>&1) || fail "local inference worker signature inspection failed: $name"
  if ! printf '%s\n' "$signature" | grep -Eq 'flags=.*\([^)]*runtime'; then
    fail "local inference worker is missing hardened runtime: $name"
  fi
  entitlements=$(codesign -d --entitlements :- "$worker" 2>/dev/null) || fail "local inference worker entitlement inspection failed: $name"
  if [ -n "$entitlements" ]; then
    entitlement_xml=$(printf '%s' "$entitlements" | plutil -convert xml1 -o - -- -) \
      || fail "local inference worker entitlements are invalid: $name"
    if printf '%s\n' "$entitlement_xml" | grep -q '<key>'; then
      fail "local inference worker has unexpected entitlements: $name"
    fi
  fi
done

codesign --verify --deep --strict "$bundle"
echo "macOS bundle privacy verification passed (five fixed, signed local workers)"
