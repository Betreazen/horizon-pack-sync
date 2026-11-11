//! Where published builds come from.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Passed through untouched: only the updater, with the owner's public key, judges it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedManifest {
    pub manifest: String,
    pub signature: String,
}

#[derive(thiserror::Error, Debug)]
pub enum SourceError {
    #[error("request to GitHub failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("GitHub answered {0}")]
    Status(reqwest::StatusCode),
    #[error("GitHub sent no download link")]
    NoLink,
    #[error("release {0} has no signed manifest")]
    NoManifest(String),
}

#[async_trait]
pub trait PackSource: Send + Sync {
    /// The newest published build.
    async fn latest(&self) -> Result<Option<SignedManifest>, SourceError>;
    async fn release(&self, sequence: i64) -> Result<Option<SignedManifest>, SourceError>;
    /// A short-lived link to a file of that build.
    async fn asset_link(&self, sequence: i64, asset: &str) -> Result<Option<String>, SourceError>;
}
