#!/usr/bin/env bash
# Run from the repository root with nFPM 2.49 installed.
set -euo pipefail
export PKG_VERSION=${1:?version required}
export CPU=${2:?expected x86_64 or aarch64}
export ASSETS
ASSETS=$(realpath "${3:?release binary directory required}")
output=${4:?output directory required}
case "$CPU" in
    x86_64) export PKG_ARCH=amd64 ;;
    aarch64) export PKG_ARCH=arm64 ;;
    *) echo "Unsupported architecture: $CPU" >&2; exit 1 ;;
esac
symbols=$(readelf --version-info "$ASSETS/hfdx-linux-$CPU" "$ASSETS/hfdx-gui-linux-$CPU")
export LIBC
LIBC=$(sed -nE 's/.*Name: GLIBC_([0-9.]+).*/\1/p' <<<"$symbols" | sort -V | tail -1)
[[ -n "$LIBC" ]]
mkdir -p "$output"
config=$(mktemp)
trap 'rm -f "$config"' EXIT
# shellcheck disable=SC2016 # envsubst receives the literal names of exported variables.
envsubst '${PKG_VERSION} ${CPU} ${PKG_ARCH} ${ASSETS} ${LIBC}' < packaging/nfpm.yaml > "$config"
for format in deb rpm archlinux; do
    nfpm package --config "$config" --packager "$format" --target "$output/"
done
