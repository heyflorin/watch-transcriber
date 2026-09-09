#!/bin/sh
set -eu

script_directory=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
desktop_directory=$(CDPATH='' cd -- "$script_directory/.." && pwd)
repo_root=$(CDPATH='' cd -- "$desktop_directory/.." && pwd)
build_home=${HOME:?}

export ECHOWALL_REMAP_REPO_ROOT="$repo_root"
export ECHOWALL_REMAP_BUILD_HOME="$build_home"
if [ -n "${RUSTC_WRAPPER:-}" ]; then
  export ECHOWALL_INNER_RUSTC_WRAPPER="$RUSTC_WRAPPER"
fi
export RUSTC_WRAPPER="$script_directory/rustc_remap_wrapper.sh"

# Rust's remap flag does not affect C/C++ `__FILE__` strings compiled by the
# pinned whisper.cpp/llama.cpp/transcribe.cpp sys crates. Apply the compiler-native equivalent
# without discarding caller-provided optimization or SDK flags.
native_path_flags="-ffile-prefix-map=$repo_root=/workspace -ffile-prefix-map=$build_home=/build/home -fdebug-prefix-map=$repo_root=/workspace -fdebug-prefix-map=$build_home=/build/home -fmacro-prefix-map=$repo_root=/workspace -fmacro-prefix-map=$build_home=/build/home"
export CFLAGS="${CFLAGS:+$CFLAGS }$native_path_flags"
export CXXFLAGS="${CXXFLAGS:+$CXXFLAGS }$native_path_flags"
export OBJCFLAGS="${OBJCFLAGS:+$OBJCFLAGS }$native_path_flags"

exec "$desktop_directory/node_modules/.bin/tauri" build \
  --config "$desktop_directory/src-tauri/tauri.local.conf.json" \
  "$@"
