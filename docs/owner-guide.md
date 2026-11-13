# Owner guide

Everything a pack owner does, from an empty server to players updating by themselves. Commands are for PowerShell on the Windows PC you build on, unless they say otherwise.

- [What you need](#what-you-need)
- [1. The signing key](#1-the-signing-key)
- [2. The pack repository](#2-the-pack-repository)
- [3. The GitHub token](#3-the-github-token)
- [4. The hub](#4-the-hub)
- [5. The game server](#5-the-game-server)
- [6. Publishing a build](#6-publishing-a-build)
- [7. Players](#7-players)
- [Everyday work](#everyday-work)
- [When something goes wrong](#when-something-goes-wrong)

## What you need

| Where | What |
|---|---|
| Your Windows PC | The [Horizon launcher](https://github.com/horizon-rust/horizon), signed in, with the Horizon version your mods target installed and prepared; [Rust](https://rustup.rs); [Docker Desktop](https://www.docker.com/products/docker-desktop/); [Git for Windows](https://git-scm.com) (for Git Bash, ssh and scp); the [GitHub CLI](https://cli.github.com), signed in |
| A Linux server | x86-64 Debian or Ubuntu, reachable as `ssh <alias>` as root. It runs the game and, if you like, the hub |
| DNS | A domain name pointing at the hub's server, for HTTPS |

Clone this repository on the PC. Every path below is relative to it.

## 1. The signing key

```powershell
mkdir C:\keys
cargo run -p pack-sync -- keygen --out C:\keys\pack.key
```

`keygen` writes the private key and prints the public one. It refuses to overwrite an existing file.

- **The private key** signs every build. Whoever holds it can run code on every player's computer. Keep it on your PC only, out of every repository and chat, and back it up somewhere offline.
- **The public key** is not secret. It goes into `pack-sync.toml` in the next step.

If you lose the private key, players' copies of pack-sync cannot accept builds signed with a new one: you will have to hand out a new `pack-sync.exe`.

## 2. The pack repository

Create a **private** GitHub repository, for example `you/your-pack`, with two files.

**`pack.json`** says what goes into a build:

```json
{
  "repository": "you/your-pack",
  "horizon_base": "nightly-2026-11-08-fdc87ae",
  "mods": [
    { "repo": "you/your-mod", "tag": "v1.0.0" },
    { "path": "C:/mods/a-mod-in-progress" }
  ]
}
```

| Field | Meaning |
|---|---|
| `repository` | This repository. Builds are published as its releases `pack-1`, `pack-2`, … |
| `horizon_base` | The installed Horizon version (release or Nightly) whose game files the build plays with. It must be installed and prepared in your launcher |
| `mods` | Each mod is a public GitHub repository at a tag, or a local folder for trying things out. Every folder with a `horizon-mixin.toml` inside is built into the game. Every folder with a `horizon-mod.toml` is a content mod: its `horizon-mod.toml`, `assets/` and `data/` ship beside the game and are turned on for players and the server. Keep anything else with those files out of the tagged tree, because it ships too |

**`pack-sync.toml`** is baked into the `pack-sync` program you give players:

```toml
site = "https://pack.example.com"   # your hub, or your own site with the same API
profile = "My Server"               # the launcher profile the installer creates
language = "en"                     # the installer's language: en or ru
public_key = "…64 hex digits…"      # from step 1
```

Do not rename or delete releases later: the hub finds builds by their `pack-N` tags.

## 3. The GitHub token

The hub reads the private repository with a fine-grained token that can do nothing else. GitHub has no API for creating tokens, but this link opens the form already filled in:

**[Create the token](https://github.com/settings/personal-access-tokens/new?name=pack-sync-hub&description=Reads+mod+pack+releases&expires_in=366&contents=read)**

1. Under **Repository access**, choose **Only select repositories** and pick your pack repository. The link cannot fill this in.
2. Check that the only permission is **Contents: Read-only** (GitHub adds **Metadata: Read-only** by itself).
3. Generate it and copy it. You paste it once, in the next step.

When it expires after 366 days, updates simply stop: the server and players stay on the last confirmed build. Create a new token with the same link and run the hub's deploy again.

## 4. The hub

The hub serves three routes to pack-sync ([docs/api.md](api.md)). One command builds it for Linux, copies it to your server and installs it as the `pack-sync-hub` service:

```powershell
.\scripts\hub\deploy-hub.ps1 -SshHost myvps -PackDir C:\my-pack -ServerKeyFile C:\keys\pack-server.key
```

- It asks for the token without showing it.
- It creates `C:\keys\pack-server.key` on first use. This is the game server's own key: with it, a machine can confirm builds for everyone. Keep it private too.
- Both secrets reach the server on stdin and land in `/etc/pack-sync-hub/hub.env`, readable only by root and the service.
- At the end it asks the hub for the newest build, which proves the token works.

The hub listens on `127.0.0.1:8140` only. Give it HTTPS with Caddy, using [scripts/hub/Caddyfile.example](../scripts/hub/Caddyfile.example): put your domain in it, copy it to `/etc/caddy/Caddyfile` and reload Caddy. Your `site` in `pack-sync.toml` is then `https://your-domain`.

**Already have a website?** Mount the same API in it instead, as the author's site does: see [docs/api.md](api.md#using-the-hub-as-a-library).

## 5. The game server

```powershell
.\scripts\game-server\deploy-game-server.ps1 -SshHost myvps -PackDir C:\my-pack `
    -ServerKeyFile C:\keys\pack-server.key -Player YourNick -AcceptEula
```

- `-AcceptEula` means you have read the [Minecraft EULA](https://aka.ms/MinecraftEULA) and accept it for this server. The world is not created without it.
- `-Player` is added to the whitelist and made an operator.
- On a server that also runs other things, add limits so the game alone is stopped if it runs out of memory: `-MemoryMax 1400M -CpuQuota 120%`. Horizon's low-memory mode is turned on with them.

It installs `horizon-server` of the same Horizon version your mods are built on, creates the world in `/srv/minecraft`, and starts two services: `horizon-server` and `pack-sync-server`. Port 25565 is opened if `ufw` is active.

Before the first build arrives, `horizon-server` cannot start: it has no game files yet, and that is expected. The files come inside each build, and pack-sync points the server at them. Watch it happen:

```sh
ssh myvps 'journalctl -u pack-sync-server -f'
```

To remove everything later: `ssh myvps 'bash /tmp/pack-sync-kit/uninstall-game-server.sh'` (add `--keep-world` to keep the world). The kit sits in `/tmp`, so copy it again after a reboot.

## 6. Publishing a build

Read the code of the mods you ship, then:

```powershell
.\scripts\publish.ps1 -PackDir C:\my-pack -KeyFile C:\keys\pack.key -TrustNativeCode
```

`-TrustNativeCode` records your decision that these mods may run with full rights on your PC, your server and every player's computer. Horizon's rules say only a person makes it, never an AI agent, so the scripts never pass it for you.

The script:

1. Collects the mods from `pack.json`.
2. Builds `pack-sync` with your `pack-sync.toml` and checks that your private key matches its public key, before anything slow.
3. Compiles the Windows client with the launcher, in a profile named `pack-build`.
4. Compiles the Linux server engine in Docker. The game files your launcher prepared go into the container, because preparing them needs a signed-in account. The server archive keeps only what a dedicated server runs on, the engine and the prepared game files: Mojang's jars and libraries, the sounds (the server engine has no audio) and the test tools stay out, which takes it from about 770 MB to about 85 MB.
5. Packs the content mods, if any, into `client-content.tar.gz`, one folder per mod id. This step runs before the builds, so a folder named unlike its mod's id fails in seconds.
6. Signs a manifest for all of them and uploads the files as release `pack-N`.

The first build takes a while: Horizon compiles from scratch, especially the Linux one. Later builds reuse the caches.

Add `-NoUpload` for a trial: the files stay in `.build\out`. Check them the way players' updaters will:

```powershell
.\target\release\pack-sync.exe verify --manifest .build\out\manifest.signed.json --dir .build\out
```

## 7. Players

Give each player:

- **`target\release\pack-sync.exe`** from your last publish. It carries your settings and key, so it only ever accepts your builds from your hub.
- **A personal code**, made on the hub's server:
  ```sh
  sudo -u pack-sync-hub /opt/pack-sync-hub/pack-sync-hub --data /var/lib/pack-sync-hub code add Alex
  ```
  The code is printed once and stored only as a hash. Making a new code for the same name cancels the old one; `code revoke Alex` cancels it outright; `code list` shows who has one.

The player runs `pack-sync.exe setup`, agrees, pastes the code and plays with the new profile. Players see a build only after your game server confirms it, so the installer reports that there is no build yet until the server is up.

## Everyday work

**A mod changed.** Tag it in its repository, update the tag in `pack.json`, run `publish.ps1`. Within about five minutes the server warns players in chat (if anyone is online), backs up the world, restarts on the new build and confirms it. Players get it at their next start. Players already in the game must restart it: the server turns away a client with a different mod version.

**The first mod with content.** A pack-sync from before content mods ignores them, so before publishing such a build, update the copies that run it. On the server, run the game server's deploy again. Players run the new `target\release\pack-sync.exe setup` once. Without that, an older copy installs the build without its content, and the server turns away a player whose mod list differs. pack-sync puts content into `horizon-mods/` of the server folder and of the player's profile. It turns on only the mods it installed and leaves a player's own mods alone. If the build brings a mod whose folder pack-sync did not put there, such as a developer's copy, that folder is moved to `.pack-sync/replaced/` beside `horizon-mods/`, not deleted.

**A build is broken.** If it does not start on the server, pack-sync goes back to the previous engine, marks the build as failed and never tries it again. Players never receive it, because the server never confirmed it. Fix the mod and publish again. Build numbers only grow, so "going back" to old mods means publishing them as a new build.

**Moving to a new Horizon version.** Change `horizon_base` and install and prepare that version in your launcher first. Players' next start installs the new game files and asks them to press Play once more.

**Several servers.** Each pack gets its own repository, `pack-sync.toml`, hub and profile. A player can have several packs: each install lives in its own `%LOCALAPPDATA%\PackSync\<profile>` folder.

## When something goes wrong

| What you see | Why, and what to do |
|---|---|
| `publish.ps1`: *does not match the public key in pack-sync.toml* | The key file is not the one whose public half is in `pack-sync.toml`. Fix one of them |
| `publish.ps1`: *is not prepared on this computer* | Install and prepare `horizon_base` in the launcher first |
| Docker: *failed to connect to the docker API* | Start Docker Desktop |
| Hub deploy: *Reading the repository failed (HTTP 502)* | The token cannot read the repository: check *Only select repositories* and *Contents: Read-only* |
| Hub deploy: *no pack-N release yet* | Fine before your first publish |
| Server log: *no update: … no server is running* | Only on the very first install, before the first build: systemd starts the server again and it picks up the build |
| Server log: *would not start: rolled back* | The build crashes on the server. `journalctl -u horizon-server` shows why |
| Player log: *the site did not accept the access code* | The code was replaced or revoked. Give the player a new one and have them run `setup` again |
| Player log: *this launch runs the previous build* | The new build could not be put into the launch; the game started on the old one. The reason follows in the log, which is `%LOCALAPPDATA%\PackSync\<profile>\pack-sync.log` |
| A player gets in with an old build | Builds with the same mod versions are interchangeable, so the server lets them in. That is expected |
