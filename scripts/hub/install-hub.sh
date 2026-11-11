#!/usr/bin/env bash
# Installs pack-sync-hub as a systemd service. Run as root from a folder holding the
# pack-sync-hub program and pack-sync-hub.service. The GitHub token and the game server's key
# arrive on stdin, one per line, so they never show in the process list.
# Usage: printf '%s\n%s\n' TOKEN KEY | install-hub.sh --repo OWNER/REPO
# Safe to run again: it updates the program and the secrets and keeps the codes.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
cd /
user="pack-sync-hub"
data_dir="/var/lib/pack-sync-hub"
config_dir="/etc/pack-sync-hub"
health_url="http://127.0.0.1:8140/healthz"
health_attempts=15

repo=""
while [ $# -gt 0 ]; do
    case "$1" in
        --repo) repo="${2:?--repo needs OWNER/REPO}"; shift 2 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done
[[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || { echo "--repo must be OWNER/REPO" >&2; exit 2; }

IFS= read -r token || true
IFS= read -r key || true
[[ "$token" =~ ^github_pat_[A-Za-z0-9_]+$ ]] || { echo "the token on stdin is not a fine-grained GitHub token" >&2; exit 2; }
[[ "$key" =~ ^[A-Za-z0-9]{32,}$ ]] || { echo "the server key on stdin needs 32 letters or digits" >&2; exit 2; }

if ! id -u "$user" >/dev/null 2>&1; then
    useradd --system --no-create-home --shell /usr/sbin/nologin "$user"
fi
install -d -o "$user" -g "$user" -m 750 "$data_dir"
install -d -o root -g "$user" -m 750 "$config_dir"
(umask 027 && printf 'PACK_GITHUB_REPO=%s\nPACK_GITHUB_TOKEN=%s\nPACK_SERVER_KEY=%s\n' \
    "$repo" "$token" "$key" > "${config_dir}/hub.env")
chown root:"$user" "${config_dir}/hub.env"

install -d /opt/pack-sync-hub
install -m 755 "${here}/pack-sync-hub" /opt/pack-sync-hub/pack-sync-hub
install -m 644 "${here}/pack-sync-hub.service" /etc/systemd/system/
systemctl daemon-reload
systemctl enable pack-sync-hub.service >/dev/null
systemctl restart pack-sync-hub.service

for _ in $(seq 1 "$health_attempts"); do
    if curl -fs -o /dev/null "$health_url"; then
        status="$(curl -s -o /dev/null -w '%{http_code}' \
            -H "Authorization: Bearer ${key}" http://127.0.0.1:8140/api/pack/manifest)"
        case "$status" in
            200) echo "The hub is up and reads ${repo}." ;;
            404) echo "The hub is up; ${repo} has no pack-N release yet." ;;
            *) echo "The hub is up, but reading ${repo} failed (HTTP $status): check the token." >&2
               exit 1 ;;
        esac
        echo "Issue a player's code: sudo -u ${user} /opt/pack-sync-hub/pack-sync-hub --data ${data_dir} code add NAME"
        exit 0
    fi
    sleep 2
done
echo "The hub did not start: journalctl -u pack-sync-hub" >&2
exit 1
