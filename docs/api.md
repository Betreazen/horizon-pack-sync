# API

`pack-sync` talks to three routes. `pack-sync-hub` serves them, and so can any site that mounts the hub as a library.

Every request carries `Authorization: Bearer <credential>`. The credential is either a **player's code** or the **game server's key**. The hub compares `SHA-256(credential.trim())`, in lowercase hex, with the server key's hash and with the stored codes.

| Status | When |
|---|---|
| `401` | No credential, or an unknown one |
| `403` | A player tried a server-only action |
| `404` | No such build, or not visible to this caller |
| `429` | More than 60 requests a minute from one address |
| `502` | GitHub could not be read |

## `GET /api/pack/manifest`

The signed manifest, passed through exactly as published:

```json
{ "manifest": "{\"format\":1,\"sequence\":3,…}", "signature": "<128 hex digits>" }
```

- **Game server:** the newest release `pack-N` of the repository.
- **Player:** the build the game server last confirmed, or `404` if it confirmed none.

The `manifest` string, once its signature checks out, reads:

```json
{
  "format": 1,
  "sequence": 3,
  "horizon_base": "nightly-2026-11-08-fdc87ae",
  "artifacts": {
    "client-windows-x64": { "asset": "client-windows-x64.exe.gz", "sha256": "…", "size": 37125659 },
    "server-linux-x64":   { "asset": "server-linux-x64.tar.gz",   "sha256": "…", "size": 766468145 }
  }
}
```

## `GET /api/pack/file/{sequence}/{asset}`

`307 Temporary Redirect` to a short-lived GitHub download link for one file of a build. Fetch the link **without** the `Authorization` header.

- Only file names listed in that release are accepted.
- A player gets only files whose names start with `client-`, and only of the confirmed build.

## `POST /api/pack/confirm`

Game server only. Body: `{ "sequence": 3 }`. Answers `204` once it records that the server runs this build, or `404` if the build does not exist. From then on players receive this build.

## Using the hub as a library

Mount the router in your own axum site and keep codes with your accounts. The author's site issues codes from each player's profile this way.

```toml
[dependencies]
pack-sync-hub = { git = "https://github.com/Betreazen/horizon-pack-sync", rev = "<commit>" }
```

```rust
use std::sync::Arc;

use async_trait::async_trait;
use pack_sync_hub::{AccessStore, PackApi, PackConfig, StoreError, router};

struct MyAccounts { /* your database pool */ }

#[async_trait]
impl AccessStore for MyAccounts {
    async fn is_player(&self, code_hash: &str) -> Result<bool, StoreError> {
        // SELECT 1 FROM pack_codes WHERE code_hash = ?
        todo!()
    }
    async fn confirmed(&self) -> Result<Option<i64>, StoreError> {
        todo!()
    }
    async fn set_confirmed(&self, sequence: i64) -> Result<(), StoreError> {
        todo!()
    }
}

fn pack_routes(accounts: MyAccounts) -> axum::Router {
    // PACK_GITHUB_REPO, PACK_GITHUB_TOKEN, PACK_SERVER_KEY; None when none is set.
    let config = PackConfig::from_env().expect("valid").expect("configured");
    let api = PackApi::from_config(&config, Arc::new(accounts)).expect("http client");
    router(api)
}
```

- Serve the app with `into_make_service_with_connect_info::<SocketAddr>()`, so the rate limit sees real addresses.
- Store codes as `pack_sync_hub::hash_code(code)`. `pack_sync_hub::codes::new_code()` makes a code: 32 letters and digits.
