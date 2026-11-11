//! File helpers: checking downloads, unpacking them and writing state safely.

use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::manifest::Artifact;

const HASH_BUFFER_BYTES: usize = 64 * 1024;

pub fn sha256_of(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    loop {
        let count = file
            .read(&mut buffer)
            .context("cannot read the file to hash it")?;
        if count == 0 {
            return Ok(hex::encode(hasher.finalize()));
        }
        hasher.update(&buffer[..count]);
    }
}

/// Fails unless `path` is exactly the file the signed manifest describes.
pub fn check_artifact(path: &Path, artifact: &Artifact) -> Result<()> {
    let size = fs::metadata(path).context("the download is missing")?.len();
    if size != artifact.size {
        bail!(
            "the download is {size} bytes, the manifest says {}",
            artifact.size
        );
    }
    let actual = sha256_of(path)?;
    if !actual.eq_ignore_ascii_case(&artifact.sha256) {
        bail!("the download's checksum does not match the manifest");
    }
    Ok(())
}

pub fn gunzip(source: &Path, target: &Path) -> Result<()> {
    let mut decoder = GzDecoder::new(BufReader::new(File::open(source)?));
    let mut output = File::create(target).context("cannot create the unpacked file")?;
    io::copy(&mut decoder, &mut output).context("the archive is damaged")?;
    output.sync_all().context("cannot flush the unpacked file")
}

/// Unpacks into a fresh folder; `tar` refuses entries that climb out of it.
pub fn untar_gz(source: &Path, target: &Path) -> Result<()> {
    if target.exists() {
        fs::remove_dir_all(target).context("cannot clear an earlier unpack")?;
    }
    fs::create_dir_all(target).context("cannot create the unpack folder")?;
    let decoder = GzDecoder::new(BufReader::new(File::open(source)?));
    tar::Archive::new(decoder)
        .unpack(target)
        .context("the archive is damaged")
}

/// Swaps in `new` for `target`, keeping a copy of the old one beside it. `target` is replaced in
/// one step and never moved away, so a crash at any moment leaves a game that starts.
pub fn replace_keeping_previous(new: &Path, target: &Path) -> Result<()> {
    fs::copy(target, previous_of(target)).context("cannot keep a copy of the old game file")?;
    fs::rename(new, target).context("the game file is in use")
}

pub fn previous_of(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_owned();
    name.push(".previous");
    PathBuf::from(name)
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = serde_json::to_vec_pretty(value).context("cannot write the state")?;
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary)
        .with_context(|| format!("cannot write {}", temporary.display()))?;
    file.write_all(&text).context("cannot write the state")?;
    // Flushed first: a power cut must not leave an empty file under the real name.
    file.sync_all().context("cannot flush the state")?;
    drop(file);
    fs::rename(&temporary, path).with_context(|| format!("cannot replace {}", path.display()))
}

#[cfg(test)]
mod tests {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::*;

    pub fn artifact_for(path: &Path) -> Artifact {
        Artifact {
            asset: "a".into(),
            sha256: sha256_of(path).expect("hash"),
            size: fs::metadata(path).expect("meta").len(),
        }
    }

    #[test]
    fn the_described_file_passes_and_any_other_does_not() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("f");
        fs::write(&path, b"one").expect("write");
        let artifact = artifact_for(&path);
        assert!(check_artifact(&path, &artifact).is_ok());
        fs::write(&path, b"two").expect("write");
        assert!(check_artifact(&path, &artifact).is_err());
    }

    #[test]
    fn a_file_of_another_size_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("f");
        fs::write(&path, b"one").expect("write");
        let mut artifact = artifact_for(&path);
        artifact.size += 1;
        assert!(check_artifact(&path, &artifact).is_err());
    }

    #[test]
    fn replacing_keeps_the_old_file_beside_the_new_one() {
        let dir = tempfile::tempdir().expect("dir");
        let target = dir.path().join("game");
        let new = dir.path().join("game.new");
        fs::write(&target, b"old").expect("write");
        fs::write(&new, b"new").expect("write");
        replace_keeping_previous(&new, &target).expect("replaced");
        assert_eq!(fs::read(&target).expect("read"), b"new");
        assert_eq!(fs::read(previous_of(&target)).expect("read"), b"old");
    }

    #[test]
    fn a_failed_replace_leaves_the_game_in_place() {
        let dir = tempfile::tempdir().expect("dir");
        let target = dir.path().join("game");
        fs::write(&target, b"old").expect("write");
        assert!(replace_keeping_previous(&dir.path().join("missing"), &target).is_err());
        assert_eq!(fs::read(&target).expect("read"), b"old");
    }

    #[test]
    fn replacing_twice_keeps_only_the_latest_previous() {
        let dir = tempfile::tempdir().expect("dir");
        let target = dir.path().join("game");
        let new = dir.path().join("game.new");
        fs::write(&target, b"1").expect("write");
        for version in [b"2", b"3"] {
            fs::write(&new, version).expect("write");
            replace_keeping_previous(&new, &target).expect("replaced");
        }
        assert_eq!(fs::read(previous_of(&target)).expect("read"), b"2");
    }

    #[test]
    fn a_tar_entry_cannot_escape_the_unpack_folder() {
        let dir = tempfile::tempdir().expect("dir");
        let archive = dir.path().join("a.tar.gz");
        let mut raw = vec![0_u8; 512];
        raw[..9].copy_from_slice(b"../escape");
        raw[100..107].copy_from_slice(b"0000644");
        raw[108..115].copy_from_slice(b"0000000");
        raw[116..123].copy_from_slice(b"0000000");
        raw[124..135].copy_from_slice(b"00000000001");
        raw[136..147].copy_from_slice(b"00000000000");
        raw[156] = b'0';
        raw[148..156].copy_from_slice(b"        ");
        let sum: u32 = raw.iter().map(|byte| u32::from(*byte)).sum();
        raw[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        raw.extend_from_slice(b"x");
        raw.resize(1024, 0);
        raw.resize(2048, 0);
        let mut encoder =
            GzEncoder::new(File::create(&archive).expect("file"), Compression::fast());
        encoder.write_all(&raw).expect("gz");
        encoder.finish().expect("finish");

        let target = dir.path().join("out");
        let _ = untar_gz(&archive, &target);
        assert!(!dir.path().join("escape").exists());
    }

    #[test]
    fn gunzip_restores_the_original_bytes() {
        let dir = tempfile::tempdir().expect("dir");
        let archive = dir.path().join("a.gz");
        let mut encoder =
            GzEncoder::new(File::create(&archive).expect("file"), Compression::fast());
        encoder.write_all(b"hello").expect("gz");
        encoder.finish().expect("finish");
        let out = dir.path().join("out");
        gunzip(&archive, &out).expect("gunzip");
        assert_eq!(fs::read(out).expect("read"), b"hello");
    }
}
