# Security policy

pack-sync installs native code on players' computers and on game servers, so security reports are welcome and handled first.

## Reporting a vulnerability

Please report privately through **[GitHub's private vulnerability reporting](https://github.com/Betreazen/horizon-pack-sync/security/advisories/new)**. Do not open a public issue for a vulnerability.

Include what you can:

- the affected part (`pack-sync client`, `server`, `setup`, `release` or `pack-sync-hub`) and the commit;
- the steps or a proof of concept;
- what an attacker gains, for example installing unsigned code, reading a player's code or the GitHub token, or escaping the game folder.

You get a first answer within a few days. Once a fix is ready, it is released and the advisory is published, with credit to you if you want it.

## Supported versions

Only the newest commit on `main` gets security fixes. Pack owners build their own `pack-sync.exe` from it (see the [README](README.md#faq)), so update by pulling `main` and publishing a new build.

## What counts

The model is described in **[docs/security.md](docs/security.md)**: what the signing key protects, and what each piece can and cannot do when an attacker controls it. Anything that breaks a promise made there is in scope.

Out of scope:

- what a mod does once you, the pack owner, have signed it;
- attacks that need your private signing key;
- bugs in Horizon or its launcher, which belong to [their project](https://github.com/horizon-rust/horizon).
