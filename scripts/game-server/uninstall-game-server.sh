#!/usr/bin/env bash
# Removes what install-game-server.sh set up, for a test install on a VPS that has other work.
# Run as root. Usage: uninstall-game-server.sh [--keep-world]
# --keep-world leaves /srv/minecraft (the world and server.toml) in place.
set -euo pipefail

user="minecraft"
keep_world=0
[ "${1:-}" != "--keep-world" ] || keep_world=1

systemctl disable --now pack-sync-server.service horizon-server.service 2>/dev/null || true
rm -f /etc/systemd/system/pack-sync-server.service /etc/systemd/system/horizon-server.service
rm -rf /etc/systemd/system/horizon-server.service.d
systemctl daemon-reload

rm -rf /opt/pack-sync /etc/pack-sync /srv/pack-sync
if [ "$keep_world" != 1 ]; then
    rm -rf /srv/minecraft
fi
if id -u "$user" >/dev/null 2>&1; then
    userdel --remove "$user" 2>/dev/null || userdel "$user"
fi
if command -v ufw >/dev/null; then
    ufw delete allow 25565/tcp >/dev/null 2>&1 || true
fi
echo "Removed the game server."
