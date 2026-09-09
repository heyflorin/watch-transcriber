#!/bin/sh
set -eu

compiler=$1
shift
repo_root=${ECHOWALL_REMAP_REPO_ROOT:?}
build_home=${ECHOWALL_REMAP_BUILD_HOME:?}

if [ -n "${ECHOWALL_INNER_RUSTC_WRAPPER:-}" ]; then
  exec "$ECHOWALL_INNER_RUSTC_WRAPPER" "$compiler" \
    "--remap-path-prefix=$repo_root=/workspace" \
    "--remap-path-prefix=$build_home=/build/home" \
    "$@"
fi

exec "$compiler" \
  "--remap-path-prefix=$repo_root=/workspace" \
  "--remap-path-prefix=$build_home=/build/home" \
  "$@"
