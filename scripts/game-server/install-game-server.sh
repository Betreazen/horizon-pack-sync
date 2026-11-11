#!/usr/bin/env bash
# Sets up the game VPS: Horizon's server under systemd, and pack-sync, which then installs the
# newest mod pack build by itself. Run as root from a folder holding pack-sync and the two units.
# The server key arrives on stdin, so it never shows in the process list.
# Usage: install-game-server.sh --site URL --player NICK --horizon-release TAG [--accept-eula]
#        [--memory-max 1400M --cpu-quota 120%]
# The limits are for a VPS shared with other services: the game alone is stopped when it runs out.
# Safe to run again: it updates pack-sync and the units and keeps the world.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
# Commands run as the game's user, who may not enter root's working directory.
cd /
user="minecraft"
home="/home/${user}"
server_dir="/srv/minecraft"
state_dir="/srv/pack-sync"
horizon_server="${home}/.local/bin/horizon-server"
installer="https://raw.githubusercontent.com/horizon-rust/horizon/main/packaging/server/install.sh"
game_port=25565

site=""
player=""
release=""
accept_eula=0
memory_max=""
cpu_quota=""
while [ $# -gt 0 ]; do
    case "$1" in
        --site) site="${2:?--site needs an address}"; shift 2 ;;
        --player) player="${2:?--player needs a nick}"; shift 2 ;;
        --horizon-release) release="${2:?--horizon-release needs a tag}"; shift 2 ;;
        --accept-eula) accept_eula=1; shift ;;
        --memory-max) memory_max="${2:?--memory-max needs a size}"; shift 2 ;;
        --cpu-quota) cpu_quota="${2:?--cpu-quota needs a percentage}"; shift 2 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done
[[ -z "$memory_max" || "$memory_max" =~ ^[0-9]+[KMG]$ ]] || { echo "bad --memory-max" >&2; exit 2; }
[[ -z "$cpu_quota" || "$cpu_quota" =~ ^[0-9]+%$ ]] || { echo "bad --cpu-quota" >&2; exit 2; }
[[ "$site" =~ ^https://[A-Za-z0-9.:/_-]+$ ]] || { echo "--site must be an https address" >&2; exit 2; }
[ -n "$player" ] || { echo "--player is required" >&2; exit 2; }
[ -n "$release" ] || { echo "--horizon-release is required" >&2; exit 2; }
[[ "$player" =~ ^[A-Za-z0-9_]{3,16}$ ]] || { echo "not a Minecraft nick: $player" >&2; exit 2; }
[[ "$release" =~ ^[A-Za-z0-9._-]+$ ]] || { echo "not a release tag: $release" >&2; exit 2; }

IFS= read -r server_key || true
if [ "${#server_key}" -lt 32 ]; then
    echo "the server key on stdin is missing or shorter than 32 characters" >&2
    exit 2
fi

if ! command -v curl >/dev/null || ! command -v openssl >/dev/null; then
    apt-get update -qq
    apt-get install -y -qq curl ca-certificates openssl
fi

if ! id -u "$user" >/dev/null 2>&1; then
    useradd --system --create-home --home-dir "$home" --shell /usr/sbin/nologin "$user"
fi

if [ ! -x "$horizon_server" ]; then
    runuser -u "$user" -- sh -c "curl -fsSL '$installer' | sh -s -- --version '$release'"
fi

install -d -o "$user" -g "$user" "$server_dir" "$state_dir"
if [ ! -f "${server_dir}/server.toml" ]; then
    if [ "$accept_eula" != 1 ]; then
        echo "read https://aka.ms/MinecraftEULA, then run again with --accept-eula" >&2
        exit 2
    fi
    runuser -u "$user" -- "$horizon_server" --dir "$server_dir" init --eula --world world \
        --port "$game_port" --whitelist --player "$player" --op "$player" --yes
fi

install -d -m 750 -o root -g "$user" /etc/pack-sync
(umask 027 && printf 'PACK_SERVER_KEY=%s\n' "$server_key" > /etc/pack-sync/server.env)
chown root:"$user" /etc/pack-sync/server.env

install -d /opt/pack-sync
install -m 755 "${here}/pack-sync" /opt/pack-sync/pack-sync
install -m 644 "${here}/horizon-server.service" /etc/systemd/system/
sed "s|__SITE__|${site}|" "${here}/pack-sync-server.service" > /etc/systemd/system/pack-sync-server.service
chmod 644 /etc/systemd/system/pack-sync-server.service

limits_dir="/etc/systemd/system/horizon-server.service.d"
if [ -n "$memory_max" ] || [ -n "$cpu_quota" ]; then
    install -d "$limits_dir"
    {
        echo "[Service]"
        [ -z "$memory_max" ] || printf 'MemoryMax=%s\nMemorySwapMax=0\n' "$memory_max"
        [ -z "$cpu_quota" ] || printf 'CPUQuota=%s\n' "$cpu_quota"
        echo "Nice=5"
    } > "${limits_dir}/limits.conf"
    # The engine's smaller-memory mode, so it lives within the limit instead of hitting it.
    runuser -u "$user" -- "$horizon_server" --dir "$server_dir" config set engine.low_memory=true \
        || echo "warning: could not turn on engine.low_memory" >&2
else
    rm -rf "$limits_dir"
fi
systemctl daemon-reload
systemctl enable --now horizon-server.service
systemctl restart pack-sync-server.service
systemctl enable pack-sync-server.service

if command -v ufw >/dev/null && ufw status | grep -q '^Status: active'; then
    ufw allow "${game_port}/tcp"
fi

echo "Installed. pack-sync installs the newest build within a few minutes:"
echo "  journalctl -u pack-sync-server -f"
