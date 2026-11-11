# Security model

pack-sync installs native code on players' computers and on your server. Horizon mods are not sandboxed, so whoever can make pack-sync install a build can run anything on those machines. This page says what protects that, and what does not.

## Trust in one place: your signing key

Every build has a manifest listing the exact size and SHA-256 of its client and server files. The manifest is signed with your ed25519 key, and `pack-sync` carries your public key from the moment you build it.

Before using anything, pack-sync:

1. checks the signature over the exact bytes of the manifest (`verify_strict`), and only then reads it;
2. refuses a build number lower than the one installed, so an old build cannot be replayed;
3. checks the size and SHA-256 of each downloaded file against the signed manifest, before unpacking it.

The private key never leaves your PC. `publish.ps1` signs locally and uploads only the result.

## What each piece can and cannot do

| If an attacker controls… | They can | They cannot |
|---|---|---|
| Your GitHub account or the pack repository | Delete or withhold builds, so updates stop | Install code: they cannot sign it |
| The hub or the server it runs on | Withhold builds, read the GitHub token, issue codes | Install code: they cannot sign it |
| The network between a player and the hub | Nothing beyond blocking: everything is HTTPS, and plain http is refused except to 127.0.0.1 | Read a player's code or swap a file |
| A player's code | Download client builds the server has confirmed | Confirm builds, get the server build, get the token |
| The game server's key (`pack-server.key`) | Confirm any published build for players, see the newest build | Install unsigned code |
| **Your private signing key** | **Everything: sign a build that runs on every machine** | |

So: keep `pack.key` offline and private above all else.

## Secrets and where they live

| Secret | Lives | Reaches |
|---|---|---|
| Signing key `pack.key` | Your PC | Nothing; it only signs |
| GitHub token | The hub's `hub.env` (root and the service only) | GitHub. Download links GitHub returns are short-lived and are fetched without the token |
| Game server key | `pack-server.key` on your PC, `/etc/pack-sync/server.env` on the game server, `hub.env` on the hub | The hub, over HTTPS |
| Players' codes | Shown once; the hub stores only SHA-256 hashes | The hub, over HTTPS |

Deploy scripts pass secrets on stdin, so they never appear in a process list, a command line or a shell history. `PackConfig`'s debug output leaves out the token and the key.

## Other protections

- **Players never get the server build.** It contains resources made from the game's files; the hub serves players only files named `client-*`.
- **Players never get ahead of the server.** They receive only the build the server confirmed after it started cleanly.
- **Rate limit.** 60 requests a minute per address (per /64 for IPv6). `X-Forwarded-For` is trusted only from a proxy on the same machine.
- **Unpacking.** Archives are unpacked with the `tar` crate, which refuses paths that climb out of the target folder, and only after their hash matched.
- **Failure is safe.** A player whose update fails plays on the build they had. A server build that does not start is rolled back and never retried.
- **Consent.** The installer tells the player that mods run with full rights and installs only after an explicit yes. Building with `--trust-native-code` is the owner's own decision; the scripts require you to pass it yourself.

## Known limits

- **A fresh install has no history.** An attacker controlling the hub could offer a new player an older, genuinely signed build. Installed copies refuse it, because build numbers only grow.
- **Stale builds.** A manifest carries no expiry, so a compromised hub can keep players on an old signed build by never offering a newer one.
- **Losing the key.** There is no key rotation: a new key means a new `pack-sync.exe` for every player.

Found a problem? Open an issue, or for anything sensitive, contact the maintainer privately through GitHub.
