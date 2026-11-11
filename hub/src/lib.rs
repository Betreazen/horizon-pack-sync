//! Serves a Horizon modpack's signed builds to pack-sync.
//!
//! Three routes, for players' updaters and the game server's:
//! `GET /api/pack/manifest`, `GET /api/pack/file/{sequence}/{asset}` and `POST /api/pack/confirm`.
//! Builds come from the releases of a private GitHub repository; the token never leaves this
//! process. Use it as the `pack-sync-hub` program, or mount [`router`] in a site of your own and
//! give it an [`AccessStore`] backed by that site's accounts.

pub mod api;
pub mod client_ip;
pub mod codes;
pub mod config;
pub mod github;
pub mod limits;
pub mod source;
pub mod store;

pub use api::{PackApi, router};
pub use codes::hash_code;
pub use config::{PackConfig, PackConfigError};
pub use source::{PackSource, SignedManifest, SourceError};
pub use store::{AccessStore, FileStore, StoreError};
