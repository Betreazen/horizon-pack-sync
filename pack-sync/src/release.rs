//! The owner's side: the signing key and the signed description of a build.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ed25519_dalek::{SigningKey, VerifyingKey};

use crate::files::{sha256_of, write_json};
use crate::manifest::{
    Artifact, CLIENT_ARTIFACT, Manifest, SERVER_ARTIFACT, Signed, parse_signing_key,
};

const KEY_BYTES: usize = 32;

/// Writes a new private key and prints the public one; never overwrites an existing key.
pub fn keygen(path: &Path) -> Result<()> {
    let mut seed = [0_u8; KEY_BYTES];
    getrandom::fill(&mut seed).map_err(|error| anyhow::anyhow!("no system randomness: {error}"))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("cannot create {} (does it already exist?)", path.display()))?;
    file.write_all(hex::encode(seed).as_bytes())
        .context("cannot write the key")?;
    println!(
        "{}",
        hex::encode(SigningKey::from_bytes(&seed).verifying_key().to_bytes())
    );
    Ok(())
}

pub struct Input {
    pub sequence: u64,
    pub horizon_base: String,
    pub client: PathBuf,
    pub server: PathBuf,
    pub key: PathBuf,
    pub out: PathBuf,
}

fn describe(path: &Path) -> Result<Artifact> {
    let asset = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("the artifact has no usable name")?
        .to_owned();
    let size = fs::metadata(path)
        .with_context(|| format!("cannot read {}", path.display()))?
        .len();
    Ok(Artifact {
        asset,
        sha256: sha256_of(path)?,
        size,
    })
}

/// Fails unless the private key belongs to the public key built into this program. A mismatch
/// would publish builds that every updater refuses.
pub fn check_key(path: &Path, embedded: &VerifyingKey) -> Result<()> {
    let key = parse_signing_key(&fs::read_to_string(path).context("cannot read the key")?)?;
    if key.verifying_key() != *embedded {
        bail!(
            "{} does not match the public key in pack-sync.toml",
            path.display()
        );
    }
    Ok(())
}

pub fn release(input: &Input) -> Result<()> {
    let key = parse_signing_key(&fs::read_to_string(&input.key).context("cannot read the key")?)?;
    let artifacts = BTreeMap::from([
        (CLIENT_ARTIFACT.to_owned(), describe(&input.client)?),
        (SERVER_ARTIFACT.to_owned(), describe(&input.server)?),
    ]);
    let manifest = Manifest::new(input.sequence, input.horizon_base.clone(), artifacts);
    write_json(&input.out, &Signed::create(&manifest, &key)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::parse_public_key;

    #[test]
    fn keygen_refuses_to_overwrite_a_key() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("key");
        fs::write(&path, "precious").expect("write");
        assert!(keygen(&path).is_err());
        assert_eq!(fs::read_to_string(&path).expect("read"), "precious");
    }

    #[test]
    fn only_the_matching_private_key_passes_the_check() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("key");
        fs::write(&path, hex::encode([7_u8; KEY_BYTES])).expect("write");
        let matching = SigningKey::from_bytes(&[7; KEY_BYTES]).verifying_key();
        let other = SigningKey::from_bytes(&[8; KEY_BYTES]).verifying_key();
        assert!(check_key(&path, &matching).is_ok());
        assert!(check_key(&path, &other).is_err());
    }

    #[test]
    fn a_release_opens_with_the_public_half_of_its_key() {
        let dir = tempfile::tempdir().expect("dir");
        let seed = hex::encode([7_u8; KEY_BYTES]);
        let key_file = dir.path().join("key");
        fs::write(&key_file, &seed).expect("write");
        let client = dir.path().join("client.exe.gz");
        let server = dir.path().join("server.tar.gz");
        fs::write(&client, b"client").expect("write");
        fs::write(&server, b"server").expect("write");
        let out = dir.path().join("manifest.signed.json");
        release(&Input {
            sequence: 3,
            horizon_base: "base".into(),
            client,
            server,
            key: key_file,
            out: out.clone(),
        })
        .expect("release");

        let public = hex::encode(
            SigningKey::from_bytes(&[7; KEY_BYTES])
                .verifying_key()
                .to_bytes(),
        );
        let signed: Signed = serde_json::from_slice(&fs::read(out).expect("read")).expect("json");
        let manifest = signed
            .open(&parse_public_key(&public).expect("key"))
            .expect("opens");
        assert_eq!(manifest.sequence, 3);
        assert_eq!(
            manifest.artifact(CLIENT_ARTIFACT).expect("client").asset,
            "client.exe.gz"
        );
    }
}
