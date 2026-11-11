#!/usr/bin/env bash
# Builds pack-sync and pack-sync-hub for Linux x86-64 in Docker, so no server compiles anything.
# Run from Git Bash or any Linux shell. pack-sync needs your pack's settings baked in.
# Usage: bash scripts/build-linux.sh <path to your pack-sync.toml>
# Debian bookworm keeps glibc at 2.36, old enough for current Debian and Ubuntu servers.
set -euo pipefail

config="${1:?usage: build-linux.sh <path to pack-sync.toml>}"
[ -f "$config" ] || { echo "no such file: $config" >&2; exit 2; }
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${root}/.build/linux"
mkdir -p "$out"

# Git Bash on Windows hands Docker Windows paths; elsewhere plain paths work.
native_path() {
    if command -v cygpath >/dev/null; then cygpath -w "$1"; else printf '%s' "$1"; fi
}
config_dir="$(cd "$(dirname "$config")" && pwd)"
config_name="$(basename "$config")"
export MSYS_NO_PATHCONV=1

docker run --rm \
    -v "$(native_path "$root"):/src:ro" \
    -v "$(native_path "$config_dir"):/config:ro" \
    -v "$(native_path "$out"):/out" \
    -v pack-sync-linux-target:/build/target \
    -v pack-sync-cargo-registry:/usr/local/cargo/registry \
    -e "PACK_SYNC_CONFIG=/config/${config_name}" \
    rust:1.99-bookworm bash -c '
set -euo pipefail
tar -C /src --exclude=./target --exclude=./.build --exclude=./.git -cf - . | tar -C /build -xf -
cd /build
cargo build --release --locked --quiet -p pack-sync -p pack-sync-hub
cp target/release/pack-sync target/release/pack-sync-hub /out/
'
echo "Built ${out}/pack-sync and ${out}/pack-sync-hub"
