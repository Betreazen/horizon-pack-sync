//! Content mods: a mod's `horizon-mod.toml` with its `assets/` and `data/`. Nothing in them
//! compiles, so they travel beside the engine as one signed archive, one folder per mod id, and
//! go into the game folder's `horizon-mods/`, switched on in `horizon-mods.txt`. Both the players
//! and the server get the same archive: the server compares each joining client's mod list.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::write::GzEncoder;

use crate::files::{check_artifact, untar_gz};
use crate::manifest::{Artifact, CONTENT_ARTIFACT, Manifest};
use crate::site::Site;

/// Named for the hub: players may fetch only files whose names start with `client-`.
pub const CONTENT_ASSET: &str = "client-content.tar.gz";
const MANIFEST: &str = "horizon-mod.toml";
const MODS_FOLDER: &str = "horizon-mods";
const SWITCHES: &str = "horizon-mods.txt";
const NEW_SUFFIX: &str = ".pack-sync-new";
/// Beside `horizon-mods/`, never inside it, so the game never reads a half-done copy.
const STAGING: &str = ".pack-sync";
const STAGING_NEW: &str = "new";
const STAGING_OLD: &str = "old";
/// A mod folder pack-sync did not put there (a developer's copy, say) is moved here, not lost.
const STAGING_REPLACED: &str = "replaced";
/// Names Windows will not give a folder, with or without an extension.
const RESERVED: [&str; 4] = ["con", "prn", "aux", "nul"];

/// A mod id that is safe as a folder name on every system and a `horizon-mods.txt` key.
fn check_id(id: &str) -> Result<()> {
    let stem = id.split('.').next().unwrap_or_default();
    let is_device = RESERVED.contains(&stem)
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.as_bytes()[3].is_ascii_digit());
    let is_safe = !id.is_empty()
        && !id.starts_with('.')
        && !id.ends_with('.')
        && !is_device
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-.".contains(&byte)
        });
    if !is_safe {
        bail!("{id:?} is not a usable mod id");
    }
    Ok(())
}

/// The mod ids in an unpacked content folder, each checked.
fn ids_in(folder: &Path) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    let entries =
        fs::read_dir(folder).with_context(|| format!("cannot read {}", folder.display()))?;
    for entry in entries {
        let entry = entry.context("cannot read the content folder")?;
        let id = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("a content folder name is not valid text"))?;
        check_id(&id)?;
        if !entry.path().join(MANIFEST).is_file() {
            bail!("content mod {id} has no {MANIFEST}");
        }
        ids.insert(id);
    }
    Ok(ids)
}

/// The id a content mod's manifest declares.
fn declared_id(manifest: &Path) -> Result<String> {
    let text = fs::read_to_string(manifest)
        .with_context(|| format!("cannot read {}", manifest.display()))?;
    let table: toml::Table = toml::from_str(&text)
        .with_context(|| format!("{} is not valid TOML", manifest.display()))?;
    table
        .get("id")
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("{} has no id", manifest.display()))
}

/// The owner's side: packs a folder of content mods (`<id>/horizon-mod.toml`, `assets/`, `data/`)
/// into one archive, after checking that each folder is named after its mod's id.
pub fn pack(folder: &Path, archive: &Path) -> Result<()> {
    let ids = ids_in(folder)?;
    if ids.is_empty() {
        bail!("{} holds no content mod", folder.display());
    }
    for id in &ids {
        let declared = declared_id(&folder.join(id).join(MANIFEST))?;
        if declared != *id {
            bail!("the folder {id} holds the content mod {declared}: name it after its id");
        }
    }
    let file =
        File::create(archive).with_context(|| format!("cannot create {}", archive.display()))?;
    let mut builder = tar::Builder::new(GzEncoder::new(BufWriter::new(file), Compression::best()));
    builder.follow_symlinks(false);
    for id in &ids {
        builder
            .append_dir_all(id, folder.join(id))
            .with_context(|| format!("cannot pack {id}"))?;
    }
    let mut writer = builder
        .into_inner()
        .context("cannot finish the archive")?
        .finish()
        .context("cannot finish the archive")?;
    writer.flush().context("cannot write the archive")
}

/// Downloads the build's content, checks it against the manifest and unpacks it into `folder`.
/// A build without content leaves no folder, which `install` reads as no content mods.
pub fn fetch(site: &dyn Site, manifest: &Manifest, folder: &Path) -> Result<()> {
    let Some(artifact) = manifest.artifacts.get(CONTENT_ARTIFACT) else {
        if folder.exists() {
            fs::remove_dir_all(folder).context("cannot clear an earlier unpack")?;
        }
        return Ok(());
    };
    if let Some(parent) = folder.parent() {
        fs::create_dir_all(parent).context("cannot create the content folder")?;
    }
    let archive = folder.with_extension("download");
    let result = site
        .download(manifest.sequence, &artifact.asset, &archive)
        .and_then(|()| check_artifact(&archive, artifact))
        .and_then(|()| untar_gz(&archive, folder))
        .and_then(|()| ids_in(folder).map(drop));
    let _ = fs::remove_file(&archive);
    if result.is_err() {
        let _ = fs::remove_dir_all(folder);
    }
    result
}

/// The checksum that names a build's content, or `None` for a build without any.
pub fn checksum(manifest: &Manifest) -> Option<String> {
    manifest
        .artifacts
        .get(CONTENT_ARTIFACT)
        .map(|artifact: &Artifact| artifact.sha256.clone())
}

/// Puts the unpacked content into `game_dir`. `managed` are the ids pack-sync put there before:
/// those the new content no longer has are taken out. Other mods stay; one the build replaces
/// is kept in `.pack-sync/replaced/`. Returns the ids pack-sync now manages.
pub fn install(unpacked: &Path, game_dir: &Path, managed: &[String]) -> Result<Vec<String>> {
    // A missing folder is a build without content, such as an engine kept from before them.
    let ids = if unpacked.exists() {
        ids_in(unpacked)?
    } else {
        BTreeSet::new()
    };
    for id in managed {
        check_id(id)?;
    }
    let mods = game_dir.join(MODS_FOLDER);
    if !ids.is_empty() {
        fs::create_dir_all(&mods).with_context(|| format!("cannot create {}", mods.display()))?;
    }
    let staging = game_dir.join(STAGING);
    for id in &ids {
        let is_ours = managed.contains(id);
        replace_folder(&unpacked.join(id), &mods.join(id), &staging, id, is_ours)?;
    }
    let dropped: Vec<&String> = managed.iter().filter(|id| !ids.contains(*id)).collect();
    for id in &dropped {
        let folder = mods.join(id);
        if folder.exists() {
            fs::remove_dir_all(&folder)
                .with_context(|| format!("cannot remove {}", folder.display()))?;
        }
    }
    set_switches(&game_dir.join(SWITCHES), &ids, &dropped)?;
    Ok(ids.into_iter().collect())
}

/// Copies `source` over `target`, staging in `.pack-sync/` on the same disk: the old folder moves
/// out only once the new one is complete. A folder pack-sync did not put there is kept.
fn replace_folder(
    source: &Path,
    target: &Path,
    staging: &Path,
    id: &str,
    is_ours: bool,
) -> Result<()> {
    let fresh = staging.join(STAGING_NEW).join(id);
    let aside = staging
        .join(if is_ours {
            STAGING_OLD
        } else {
            STAGING_REPLACED
        })
        .join(id);
    for leftover in [&fresh, &aside] {
        if leftover.exists() {
            fs::remove_dir_all(leftover).context("cannot clear an interrupted install")?;
        }
    }
    copy_tree(source, &fresh)?;
    if target.exists() {
        if let Some(parent) = aside.parent() {
            fs::create_dir_all(parent).context("cannot create the staging folder")?;
        }
        fs::rename(target, &aside).with_context(|| format!("{} is in use", target.display()))?;
        if !is_ours {
            eprintln!(
                "the build brings its own {id}: the copy that was there is kept in {}",
                aside.display()
            );
        }
    }
    fs::rename(&fresh, target).with_context(|| format!("cannot install {}", target.display()))?;
    // The new copy is in place: an old one that cannot go yet (a file held open) only costs disk.
    if is_ours && let Err(error) = fs::remove_dir_all(&aside) {
        eprintln!("cannot remove {} yet: {error}", aside.display());
    }
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target).with_context(|| format!("cannot create {}", target.display()))?;
    for entry in
        fs::read_dir(source).with_context(|| format!("cannot read {}", source.display()))?
    {
        let entry = entry.context("cannot read the content")?;
        let kind = entry.file_type().context("cannot read the content")?;
        let to = target.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &to)
                .with_context(|| format!("cannot copy to {}", to.display()))?;
        }
    }
    Ok(())
}

/// Switches the content mods on in `horizon-mods.txt` and drops the lines of removed ones,
/// keeping every other line as it was.
fn set_switches(path: &Path, on: &BTreeSet<String>, dropped: &[&String]) -> Result<()> {
    let old = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    let ours = |line: &str| {
        let key = line.trim().split(':').next().unwrap_or_default().trim();
        key.strip_prefix("mod.").is_some_and(|id| {
            on.contains(id) || dropped.iter().any(|dropped| dropped.as_str() == id)
        })
    };
    let mut lines: Vec<String> = old
        .lines()
        .filter(|line| !ours(line))
        .map(str::to_owned)
        .collect();
    lines.extend(on.iter().map(|id| format!("mod.{id}:true")));
    let mut text = lines.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    if text == old {
        return Ok(());
    }
    let temporary = with_suffix(path, NEW_SUFFIX);
    fs::write(&temporary, text).with_context(|| format!("cannot write {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("cannot replace {}", path.display()))
}

#[cfg(test)]
pub mod tests {
    use std::collections::BTreeMap;

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::site::tests::FakeSite;

    /// Writes a content mod `id` with one data file holding `marker`.
    pub fn write_mod(folder: &Path, id: &str, marker: &str) {
        let root = folder.join(id);
        fs::create_dir_all(root.join("data")).expect("mkdir");
        fs::write(
            root.join(MANIFEST),
            format!("id = \"{id}\"\nversion = \"1\"\n"),
        )
        .expect("write");
        fs::write(root.join("data").join("marker.txt"), marker).expect("write");
    }

    /// A packed archive of the given mods and the manifest artifact describing it.
    pub fn archive_of(mods: &[(&str, &str)]) -> (Vec<u8>, Artifact) {
        let dir = tempfile::tempdir().expect("dir");
        let source = dir.path().join("content");
        for (id, marker) in mods {
            write_mod(&source, id, marker);
        }
        let archive = dir.path().join(CONTENT_ASSET);
        pack(&source, &archive).expect("pack");
        let bytes = fs::read(&archive).expect("read");
        let artifact = Artifact {
            asset: CONTENT_ASSET.into(),
            sha256: hex::encode(Sha256::digest(&bytes)),
            size: bytes.len() as u64,
        };
        (bytes, artifact)
    }

    fn unpacked(dir: &Path, mods: &[(&str, &str)]) -> PathBuf {
        let folder = dir.join(format!("unpacked-{}", mods.len()));
        for (id, marker) in mods {
            write_mod(&folder, id, marker);
        }
        fs::create_dir_all(&folder).expect("mkdir");
        folder
    }

    fn marker(game: &Path, id: &str) -> String {
        fs::read_to_string(
            game.join(MODS_FOLDER)
                .join(id)
                .join("data")
                .join("marker.txt"),
        )
        .expect("marker")
    }

    #[test]
    fn content_is_installed_and_switched_on() {
        let dir = tempfile::tempdir().expect("dir");
        let game = dir.path().join("game");
        let managed =
            install(&unpacked(dir.path(), &[("rustcraft", "v1")]), &game, &[]).expect("install");
        assert_eq!(managed, ["rustcraft"]);
        assert_eq!(marker(&game, "rustcraft"), "v1");
        assert_eq!(
            fs::read_to_string(game.join(SWITCHES)).expect("switches"),
            "mod.rustcraft:true\n"
        );
    }

    #[test]
    fn the_players_own_mods_and_switches_stay() {
        let dir = tempfile::tempdir().expect("dir");
        let game = dir.path().join("game");
        write_mod(&game.join(MODS_FOLDER), "mine", "keep");
        fs::write(game.join(SWITCHES), "mod.mine:true\nmod.rustcraft:false\n").expect("write");
        install(&unpacked(dir.path(), &[("rustcraft", "v1")]), &game, &[]).expect("install");
        assert_eq!(marker(&game, "mine"), "keep");
        assert_eq!(
            fs::read_to_string(game.join(SWITCHES)).expect("switches"),
            "mod.mine:true\nmod.rustcraft:true\n"
        );
    }

    #[test]
    fn a_newer_build_replaces_the_content_and_drops_what_it_no_longer_has() {
        let dir = tempfile::tempdir().expect("dir");
        let game = dir.path().join("game");
        let first = unpacked(dir.path(), &[("a", "v1"), ("b", "v1")]);
        let managed = install(&first, &game, &[]).expect("first");
        let second = dir.path().join("second");
        write_mod(&second, "a", "v2");
        let managed = install(&second, &game, &managed).expect("second");
        assert_eq!(managed, ["a"]);
        assert_eq!(marker(&game, "a"), "v2");
        assert!(!game.join(MODS_FOLDER).join("b").exists());
        assert_eq!(
            fs::read_to_string(game.join(SWITCHES)).expect("switches"),
            "mod.a:true\n"
        );
    }

    #[test]
    fn a_build_without_content_takes_out_only_what_pack_sync_put_in() {
        let dir = tempfile::tempdir().expect("dir");
        let game = dir.path().join("game");
        write_mod(&game.join(MODS_FOLDER), "mine", "keep");
        let managed = install(&unpacked(dir.path(), &[("a", "v1")]), &game, &[]).expect("first");
        let empty = dir.path().join("empty");
        fs::create_dir_all(&empty).expect("mkdir");
        assert!(install(&empty, &game, &managed).expect("second").is_empty());
        assert!(!game.join(MODS_FOLDER).join("a").exists());
        assert_eq!(marker(&game, "mine"), "keep");
    }

    #[test]
    fn a_copy_pack_sync_did_not_install_is_kept_aside_not_lost() {
        let dir = tempfile::tempdir().expect("dir");
        let game = dir.path().join("game");
        write_mod(&game.join(MODS_FOLDER), "rustcraft", "dev build");
        install(&unpacked(dir.path(), &[("rustcraft", "v1")]), &game, &[]).expect("install");
        assert_eq!(marker(&game, "rustcraft"), "v1");
        let kept = game
            .join(STAGING)
            .join(STAGING_REPLACED)
            .join("rustcraft")
            .join("data")
            .join("marker.txt");
        assert_eq!(fs::read_to_string(kept).expect("kept"), "dev build");
    }

    #[test]
    fn horizon_mods_holds_only_mod_folders_after_an_update() {
        let dir = tempfile::tempdir().expect("dir");
        let game = dir.path().join("game");
        let managed = install(&unpacked(dir.path(), &[("a", "v1")]), &game, &[]).expect("first");
        let second = dir.path().join("second");
        write_mod(&second, "a", "v2");
        install(&second, &game, &managed).expect("second");
        let names: Vec<String> = fs::read_dir(game.join(MODS_FOLDER))
            .expect("list")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a"]);
        assert!(!game.join(STAGING).join(STAGING_OLD).join("a").exists());
    }

    #[test]
    fn a_folder_named_unlike_its_mod_is_not_packed() {
        let dir = tempfile::tempdir().expect("dir");
        let source = dir.path().join("content");
        write_mod(&source, "a", "v1");
        fs::rename(source.join("a"), source.join("b")).expect("rename");
        assert!(pack(&source, &dir.path().join("out.tar.gz")).is_err());
    }

    #[test]
    fn unsafe_ids_are_refused() {
        for id in [
            "", "..", ".hidden", "a/b", "A", "a b", "a.", "con", "nul.txt", "com1", "lpt9",
        ] {
            assert!(check_id(id).is_err(), "{id:?}");
        }
        for id in ["rustcraft", "my_mod", "a-b.c", "x2"] {
            assert!(check_id(id).is_ok(), "{id:?}");
        }
    }

    #[test]
    fn a_fetched_archive_is_checked_and_unpacked() {
        let dir = tempfile::tempdir().expect("dir");
        let (bytes, artifact) = archive_of(&[("rustcraft", "v1")]);
        let manifest = Manifest::new(
            2,
            "base".into(),
            BTreeMap::from([(CONTENT_ARTIFACT.to_owned(), artifact)]),
        );
        let site = FakeSite {
            files: BTreeMap::from([(CONTENT_ASSET.to_owned(), bytes)]),
            ..FakeSite::default()
        };
        let folder = dir.path().join("content");
        fetch(&site, &manifest, &folder).expect("fetch");
        assert!(folder.join("rustcraft").join(MANIFEST).is_file());
    }

    #[test]
    fn a_swapped_archive_is_refused_and_leaves_nothing() {
        let dir = tempfile::tempdir().expect("dir");
        let (_, artifact) = archive_of(&[("rustcraft", "v1")]);
        let (other, _) = archive_of(&[("rustcraft", "evil")]);
        let manifest = Manifest::new(
            2,
            "base".into(),
            BTreeMap::from([(CONTENT_ARTIFACT.to_owned(), artifact)]),
        );
        let site = FakeSite {
            files: BTreeMap::from([(CONTENT_ASSET.to_owned(), other)]),
            ..FakeSite::default()
        };
        let folder = dir.path().join("content");
        assert!(fetch(&site, &manifest, &folder).is_err());
        assert!(!folder.exists());
    }
}
