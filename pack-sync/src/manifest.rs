//! The signed description of one published build of the pack.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

pub const CLIENT_ARTIFACT: &str = "client-windows-x64";
pub const SERVER_ARTIFACT: &str = "server-linux-x64";
const FORMAT: u32 = 1;
const KEY_LENGTH: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub asset: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    /// Only ever grows: a lower number is a replayed old build, never a wanted one.
    pub sequence: u64,
    /// The Horizon release or Nightly whose game files this build plays with.
    pub horizon_base: String,
    pub artifacts: BTreeMap<String, Artifact>,
}

impl Manifest {
    pub fn new(sequence: u64, horizon_base: String, artifacts: BTreeMap<String, Artifact>) -> Self {
        Self {
            format: FORMAT,
            sequence,
            horizon_base,
            artifacts,
        }
    }

    pub fn artifact(&self, name: &str) -> Result<&Artifact> {
        self.artifacts
            .get(name)
            .with_context(|| format!("build has no {name}"))
    }
}

/// A manifest as it travels: the exact signed text and its signature in hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signed {
    pub manifest: String,
    pub signature: String,
}

impl Signed {
    pub fn create(manifest: &Manifest, key: &SigningKey) -> Result<Self> {
        let text = serde_json::to_string(manifest).context("cannot write the manifest")?;
        let signature = hex::encode(key.sign(text.as_bytes()).to_bytes());
        Ok(Self {
            manifest: text,
            signature,
        })
    }

    /// The manifest, only if the owner's key signed exactly this text.
    pub fn open(&self, key: &VerifyingKey) -> Result<Manifest> {
        let bytes: [u8; 64] = hex::decode(&self.signature)
            .context("the signature is not hex")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("the signature has the wrong length"))?;
        key.verify_strict(self.manifest.as_bytes(), &Signature::from_bytes(&bytes))
            .context("the signature does not match the pack's key")?;
        let manifest: Manifest =
            serde_json::from_str(&self.manifest).context("the manifest is malformed")?;
        if manifest.format != FORMAT {
            bail!(
                "manifest format {} is not supported, update pack-sync",
                manifest.format
            );
        }
        Ok(manifest)
    }
}

pub fn parse_public_key(text: &str) -> Result<VerifyingKey> {
    let bytes = decode_key(text).context("the public key is not 64 hex digits")?;
    VerifyingKey::from_bytes(&bytes).context("the public key is invalid")
}

pub fn parse_signing_key(text: &str) -> Result<SigningKey> {
    let bytes = decode_key(text).context("the signing key is not 64 hex digits")?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn decode_key(text: &str) -> Result<[u8; KEY_LENGTH]> {
    hex::decode(text.trim())?
        .try_into()
        .map_err(|_| anyhow::anyhow!("wrong key length"))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; KEY_LENGTH])
    }

    pub fn manifest(sequence: u64) -> Manifest {
        let artifact = Artifact {
            asset: "file.gz".into(),
            sha256: "00".into(),
            size: 1,
        };
        Manifest::new(
            sequence,
            "base-1".into(),
            BTreeMap::from([("a".to_owned(), artifact)]),
        )
    }

    #[test]
    fn a_signed_manifest_opens_with_the_matching_key() {
        let key = signing_key(1);
        let signed = Signed::create(&manifest(7), &key).expect("signed");
        assert_eq!(
            signed.open(&key.verifying_key()).expect("opens").sequence,
            7
        );
    }

    #[test]
    fn a_manifest_signed_by_another_key_is_refused() {
        let signed = Signed::create(&manifest(7), &signing_key(2)).expect("signed");
        assert!(signed.open(&signing_key(1).verifying_key()).is_err());
    }

    #[test]
    fn a_changed_manifest_is_refused() {
        let key = signing_key(1);
        let mut signed = Signed::create(&manifest(7), &key).expect("signed");
        signed.manifest = signed.manifest.replace("\"sequence\":7", "\"sequence\":8");
        assert!(signed.open(&key.verifying_key()).is_err());
    }

    #[test]
    fn a_future_format_is_refused_even_when_signed() {
        let key = signing_key(1);
        let mut newer = manifest(7);
        newer.format = FORMAT + 1;
        let signed = Signed::create(&newer, &key).expect("signed");
        assert!(signed.open(&key.verifying_key()).is_err());
    }

    #[test]
    fn a_short_signature_is_an_error_not_a_panic() {
        let key = signing_key(1);
        let mut signed = Signed::create(&manifest(7), &key).expect("signed");
        signed.signature = "abcd".into();
        assert!(signed.open(&key.verifying_key()).is_err());
    }
}
