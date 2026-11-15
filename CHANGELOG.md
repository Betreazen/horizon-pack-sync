# Changelog

All notable changes to pack-sync and pack-sync-hub. The project follows [Semantic Versioning](https://semver.org); both crates share one version.

## [Unreleased]

## [0.2.0] - 2026-11-15

### Added

- **Content mods.** Mods with `horizon-mod.toml`, `assets/` and `data/` ship as a separate signed file, `client-content.tar.gz`. pack-sync installs them into `horizon-mods/<id>` and switches them on, both for players and on the game server. A server rollback also restores the earlier content. `pack-sync pack-content` and `release --content` build that file; `publish.ps1` collects content mods from the pack's repositories.
- Mod folders that pack-sync did not install are never deleted. A clashing folder is moved aside to `.pack-sync/replaced/`.

### Changed

- The server build ships only what a dedicated server runs, so it is much smaller.

### Compatibility

- Older pack-sync binaries ignore the content file. Update the game server's pack-sync and have players run `setup` again before your first build with content mods.

## [0.1.0] - 2026-11-11

### Added

- The first release:
  - signed ed25519 manifests;
  - the player client hooked into the Horizon launcher;
  - the game-server service with backups and automatic rollback;
  - the installer;
  - `publish.ps1`;
  - `pack-sync-hub` as a program and as a library;
  - one-command deploy scripts for the hub and the game server.

[Unreleased]: https://github.com/Betreazen/horizon-pack-sync/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Betreazen/horizon-pack-sync/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Betreazen/horizon-pack-sync/releases/tag/v0.1.0
