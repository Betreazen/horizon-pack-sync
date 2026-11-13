//! Brings a player's game to the build the server is running, before the game starts.
//!
//! On Windows the launcher copies the game from its version folder into a runtime slot and starts
//! that copy, holding the version folder's file open until the game exits. So pack-sync keeps the
//! current build in its own folder and, before every start, puts it into the slot, whose path the
//! launcher hands to the pre-launch command in `HORIZON_WINDOWS_RUNTIME_SLOT`. Without a slot the
//! game starts from the version folder, and the build replaces the program there.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};

use crate::content;
use crate::files::{check_artifact, gunzip, replace_keeping_previous, sha256_of};
use crate::launcher::Launcher;
use crate::manifest::{CLIENT_ARTIFACT, Manifest};
use crate::site::Site;

const WINDOWS_PROGRAM_MAGIC: &[u8] = b"MZ";
/// Beside the kept program: the build's content mods as they came out of the archive.
const CONTENT_FOLDER: &str = "content";
const DOWNLOAD_SUFFIX: &str = "download";
const UNPACKED_SUFFIX: &str = "new";
const GAME_PROGRAM: &str = "horizon-client.exe";

/// What is installed on this computer and how to reach the site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientState {
    pub site: String,
    pub code: String,
    pub instance: String,
    pub launcher: PathBuf,
    pub program: PathBuf,
    pub version_tag: String,
    pub horizon_base: String,
    pub sequence: u64,
    /// pack-sync's own copy of the current build's program, put into every launch.
    #[serde(default)]
    pub build: Option<PathBuf>,
    #[serde(default)]
    pub build_sha256: Option<String>,
    /// The content mods pack-sync put into the instance's `horizon-mods/`.
    #[serde(default)]
    pub content: Vec<String>,
    /// The checksum of the content archive they came from; `None` when the build had none.
    #[serde(default)]
    pub content_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    UpToDate,
    Updated(u64),
    /// The update needs new game files, which the launch already under way cannot use.
    RestartNeeded(u64),
    /// This launch runs what it has, for this reason.
    Kept(String),
}

/// The pieces a sync works with.
pub struct Sync<'a> {
    pub site: &'a dyn Site,
    pub launcher: &'a dyn Launcher,
    pub key: &'a VerifyingKey,
    /// Where pack-sync keeps the current build, one folder per build number.
    pub builds_dir: &'a Path,
    /// The program this launch will start, when the launcher staged a copy of it.
    pub staged: Option<&'a Path>,
}

/// A build kept in `builds_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBuild {
    pub program: PathBuf,
    pub sha256: String,
}

impl Sync<'_> {
    /// Never fails: a problem here must not keep the player out of the game.
    pub fn run(&self, state: &ClientState) -> (ClientState, Outcome) {
        let (next, outcome) = match self.update(state) {
            Ok(result) => result,
            Err(error) => (state.clone(), Outcome::Kept(format!("{error:#}"))),
        };
        if matches!(outcome, Outcome::RestartNeeded(_)) {
            return (next, outcome);
        }
        // Also when the site is down: the launcher staged the version folder's older program.
        match self.stage(&next) {
            Ok(()) => (next, outcome),
            Err(error) => {
                let reason = format!("this launch runs the previous build: {error:#}");
                (next, Outcome::Kept(reason))
            }
        }
    }

    fn update(&self, state: &ClientState) -> Result<(ClientState, Outcome)> {
        let manifest = self.site.manifest()?.open(self.key)?;
        if manifest.sequence < state.sequence {
            bail!(
                "the site offered build {} but {} is installed",
                manifest.sequence,
                state.sequence
            );
        }
        if manifest.sequence == state.sequence {
            // An install interrupted after the program, or made by a pack-sync from before
            // content mods, may lack this build's content.
            if content::checksum(&manifest) == state.content_sha256 {
                return Ok((state.clone(), Outcome::UpToDate));
            }
            let installed = self.install_content(&manifest, &state.instance, &state.content)?;
            let next = ClientState {
                content: installed.ids,
                content_sha256: installed.sha256,
                ..state.clone()
            };
            return Ok((next, Outcome::UpToDate));
        }
        if manifest.horizon_base != state.horizon_base {
            let installed = self.install_fresh(&manifest, &state.instance, &state.content)?;
            if let Err(error) = self.launcher.remove(&state.version_tag) {
                eprintln!("the old version stays installed: {error:#}");
            }
            let next = ClientState {
                program: installed.program,
                version_tag: installed.tag,
                horizon_base: manifest.horizon_base.clone(),
                sequence: manifest.sequence,
                build: Some(installed.build.program),
                build_sha256: Some(installed.build.sha256),
                content: installed.content.ids,
                content_sha256: installed.content.sha256,
                ..state.clone()
            };
            return Ok((next, Outcome::RestartNeeded(manifest.sequence)));
        }
        // Both downloads are checked before anything changes in the game. The content goes in
        // just before the program: if the program then fails, the next launch retries the build.
        let unpacked = self.fetch_content(&manifest)?;
        let build = self.store_build(&manifest)?;
        let installed = self.put_content(&unpacked, &manifest, &state.instance, &state.content)?;
        if self.staged.is_none() {
            replace_from(&build.program, &state.program)?;
        }
        let next = ClientState {
            sequence: manifest.sequence,
            build: Some(build.program),
            build_sha256: Some(build.sha256),
            content: installed.ids,
            content_sha256: installed.sha256,
            ..state.clone()
        };
        Ok((next, Outcome::Updated(manifest.sequence)))
    }

    /// Fetches the build's content mods and puts them into the instance's game folder, taking out
    /// those of `managed` the build no longer has.
    fn install_content(
        &self,
        manifest: &Manifest,
        instance: &str,
        managed: &[String],
    ) -> Result<InstalledContent> {
        let unpacked = self.fetch_content(manifest)?;
        self.put_content(&unpacked, manifest, instance, managed)
    }

    /// Downloads, checks and unpacks the build's content beside its kept program.
    fn fetch_content(&self, manifest: &Manifest) -> Result<PathBuf> {
        let unpacked = self
            .builds_dir
            .join(manifest.sequence.to_string())
            .join(CONTENT_FOLDER);
        content::fetch(self.site, manifest, &unpacked)?;
        Ok(unpacked)
    }

    fn put_content(
        &self,
        unpacked: &Path,
        manifest: &Manifest,
        instance: &str,
        managed: &[String],
    ) -> Result<InstalledContent> {
        let home = self.launcher.instance_home(instance)?;
        let ids = content::install(unpacked, &home, managed)?;
        Ok(InstalledContent {
            ids,
            sha256: content::checksum(manifest),
        })
    }

    /// Puts the current build where this launch will start it, unless it is there already.
    fn stage(&self, state: &ClientState) -> Result<()> {
        let (Some(staged), Some(build), Some(sha256)) =
            (self.staged, &state.build, &state.build_sha256)
        else {
            return Ok(());
        };
        if sha256_of(staged).ok().as_deref() == Some(sha256.as_str()) {
            return Ok(());
        }
        if sha256_of(build)? != *sha256 {
            bail!("the kept copy of build {} is damaged", state.sequence);
        }
        let temporary = staged.with_extension(UNPACKED_SUFFIX);
        fs::copy(build, &temporary).context("cannot copy the build into the launch")?;
        fs::rename(&temporary, staged).context("cannot put the build into the launch")
    }

    /// Downloads and checks a build into its own folder, dropping the folders of older builds.
    fn store_build(&self, manifest: &Manifest) -> Result<StoredBuild> {
        let folder = self.builds_dir.join(manifest.sequence.to_string());
        fs::create_dir_all(&folder).context("cannot create the builds folder")?;
        let program = folder.join(GAME_PROGRAM);
        let unpacked = program.with_extension(UNPACKED_SUFFIX);
        self.fetch_program(manifest, &unpacked)?;
        fs::rename(&unpacked, &program).context("cannot keep the build")?;
        self.prune_builds(manifest.sequence);
        Ok(StoredBuild {
            sha256: sha256_of(&program)?,
            program,
        })
    }

    fn prune_builds(&self, current: u64) {
        let Ok(entries) = fs::read_dir(self.builds_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let is_older = name
                .to_str()
                .and_then(|name| name.parse::<u64>().ok())
                .is_some_and(|sequence| sequence != current);
            if is_older && let Err(error) = fs::remove_dir_all(entry.path()) {
                eprintln!("cannot remove an old build: {error}");
            }
        }
    }

    /// Downloads the client, checks it against the manifest and unpacks it to `unpacked`.
    fn fetch_program(&self, manifest: &Manifest, unpacked: &Path) -> Result<()> {
        let artifact = manifest.artifact(CLIENT_ARTIFACT)?;
        let archive = unpacked.with_extension(DOWNLOAD_SUFFIX);
        let result = self
            .site
            .download(manifest.sequence, &artifact.asset, &archive)
            .and_then(|()| check_artifact(&archive, artifact))
            .and_then(|()| gunzip(&archive, unpacked))
            .and_then(|()| check_is_program(unpacked));
        let _ = fs::remove_file(&archive);
        if result.is_err() {
            let _ = fs::remove_file(unpacked);
        }
        result
    }

    /// Installs the build as a new version pinned to `instance`, leaving the player's selected
    /// version as it was.
    pub fn install_fresh(
        &self,
        manifest: &Manifest,
        instance: &str,
        managed: &[String],
    ) -> Result<FreshInstall> {
        let before = self.launcher.installed()?;
        if !before.installed.contains(&manifest.horizon_base) {
            self.launcher.install(&manifest.horizon_base)?;
        }
        let unpacked = self.fetch_content(manifest)?;
        let build = self.store_build(manifest)?;
        let content = self.put_content(&unpacked, manifest, instance, managed)?;
        let imported = self
            .launcher
            .import(&build.program, &manifest.horizon_base)?;
        if let Some(selected) = &before.selected {
            self.launcher.select(selected)?;
        }
        if !imported.prepared {
            self.launcher.prepare(&imported.imported)?;
        }
        self.launcher.pin(instance, &imported.imported)?;
        Ok(FreshInstall {
            program: self.launcher.program_path(&imported.imported)?,
            tag: imported.imported,
            build,
            content,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshInstall {
    pub program: PathBuf,
    pub tag: String,
    pub build: StoredBuild,
    pub content: InstalledContent,
}

/// The content mods a sync put into the game folder, and their archive's checksum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledContent {
    pub ids: Vec<String>,
    pub sha256: Option<String>,
}

/// Replaces `target` with a copy of `source`, keeping the old one beside it.
fn replace_from(source: &Path, target: &Path) -> Result<()> {
    let temporary = target.with_extension(UNPACKED_SUFFIX);
    fs::copy(source, &temporary).context("cannot copy the build")?;
    let result = replace_keeping_previous(&temporary, target);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn check_is_program(path: &Path) -> Result<()> {
    let bytes = fs::read(path).context("cannot read the unpacked program")?;
    if !bytes.starts_with(WINDOWS_PROGRAM_MAGIC) {
        bail!("the download is not a Windows program");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Write;

    use ed25519_dalek::SigningKey;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::launcher::tests::FakeLauncher;
    use crate::manifest::tests::signing_key;
    use crate::manifest::{Artifact, Signed};
    use crate::site::tests::FakeSite;

    const NEW_PROGRAM: &[u8] = b"MZ new game";
    const OLD_PROGRAM: &[u8] = b"MZ old game";
    const ASSET: &str = "client.exe.gz";

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(bytes).expect("gz");
        encoder.finish().expect("finish")
    }

    fn manifest_for(sequence: u64, base: &str, file: &[u8]) -> Manifest {
        let artifact = Artifact {
            asset: ASSET.into(),
            sha256: hex::encode(Sha256::digest(file)),
            size: file.len() as u64,
        };
        Manifest::new(
            sequence,
            base.into(),
            BTreeMap::from([(CLIENT_ARTIFACT.into(), artifact)]),
        )
    }

    fn site_serving(signed: Signed, file: Vec<u8>) -> FakeSite {
        let site = FakeSite {
            files: BTreeMap::from([(ASSET.to_owned(), file)]),
            ..FakeSite::default()
        };
        *site.signed.borrow_mut() = Some(signed);
        site
    }

    fn signed_site(world: &World, sequence: u64, base: &str) -> FakeSite {
        let file = gzip(NEW_PROGRAM);
        let manifest = manifest_for(sequence, base, &file);
        site_serving(Signed::create(&manifest, &world.key).expect("sign"), file)
    }

    /// A build whose release also carries content mods.
    fn content_site(world: &World, sequence: u64, mods: &[(&str, &str)]) -> FakeSite {
        let file = gzip(NEW_PROGRAM);
        let mut manifest = manifest_for(sequence, "base-1", &file);
        let (bytes, artifact) = content::tests::archive_of(mods);
        manifest
            .artifacts
            .insert(crate::manifest::CONTENT_ARTIFACT.into(), artifact);
        let site = site_serving(Signed::create(&manifest, &world.key).expect("sign"), file);
        FakeSite {
            files: BTreeMap::from([
                (ASSET.to_owned(), gzip(NEW_PROGRAM)),
                (content::CONTENT_ASSET.to_owned(), bytes),
            ]),
            signed: site.signed,
            ..FakeSite::default()
        }
    }

    /// What the content mod `id` holds in the player's instance, if it is there.
    fn installed_marker(world: &World, id: &str) -> Option<String> {
        let path = world
            .dir
            .path()
            .join("instances")
            .join("pack")
            .join("horizon-mods")
            .join(id)
            .join("data")
            .join("marker.txt");
        fs::read_to_string(path).ok()
    }

    #[test]
    fn a_build_with_content_puts_it_into_the_instance() {
        let world = world(1);
        let (state, outcome) = run_staged(
            &world,
            &world.state,
            &content_site(&world, 2, &[("rustcraft", "v2")]),
        );
        assert_eq!(outcome, Outcome::Updated(2));
        assert_eq!(state.content, ["rustcraft"]);
        assert!(state.content_sha256.is_some());
        assert_eq!(installed_marker(&world, "rustcraft").as_deref(), Some("v2"));
        assert_eq!(read(&world.staged), NEW_PROGRAM);
    }

    #[test]
    fn content_missing_from_the_current_build_is_installed_without_the_program() {
        let world = world(2);
        let site = content_site(&world, 2, &[("rustcraft", "v2")]);
        let (state, outcome) = run(&world, &site);
        assert_eq!(outcome, Outcome::UpToDate);
        assert_eq!(state.content, ["rustcraft"]);
        assert_eq!(site.downloads.get(), 1);
        assert_eq!(read(&world.state.program), OLD_PROGRAM);
        // And the next launch has nothing to do.
        let (_, again) = run_with(&world, &state, &site, None);
        assert_eq!(again, Outcome::UpToDate);
        assert_eq!(site.downloads.get(), 1);
    }

    #[test]
    fn a_swapped_content_archive_keeps_the_old_build() {
        let world = world(1);
        let mut site = content_site(&world, 2, &[("rustcraft", "v2")]);
        let (evil, _) = content::tests::archive_of(&[("rustcraft", "evil")]);
        site.files.insert(content::CONTENT_ASSET.into(), evil);
        let (state, outcome) = run_staged(&world, &world.state, &site);
        assert!(matches!(outcome, Outcome::Kept(_)));
        assert_eq!(state.sequence, 1);
        assert_eq!(read(&world.staged), OLD_PROGRAM);
        assert_eq!(installed_marker(&world, "rustcraft"), None);
    }

    #[test]
    fn a_program_that_fails_its_check_leaves_the_content_out_too() {
        let world = world(1);
        let mut site = content_site(&world, 2, &[("rustcraft", "v2")]);
        site.files.insert(ASSET.into(), gzip(b"MZ evil game"));
        let (state, outcome) = run_staged(&world, &world.state, &site);
        assert!(matches!(outcome, Outcome::Kept(_)));
        assert!(state.content.is_empty());
        assert_eq!(installed_marker(&world, "rustcraft"), None);
    }

    struct World {
        dir: tempfile::TempDir,
        key: SigningKey,
        state: ClientState,
        /// The launcher's runtime copy of the old program, as it stages one before each start.
        staged: PathBuf,
    }

    fn world(sequence: u64) -> World {
        let dir = tempfile::tempdir().expect("dir");
        let program = dir.path().join("old").join(GAME_PROGRAM);
        let staged = dir.path().join("slot").join(GAME_PROGRAM);
        for path in [&program, &staged] {
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(path, OLD_PROGRAM).expect("write");
        }
        let state = ClientState {
            site: "https://example.com".into(),
            code: "code".into(),
            instance: "pack".into(),
            launcher: "launcher.exe".into(),
            program,
            version_tag: "imported-0".into(),
            horizon_base: "base-1".into(),
            sequence,
            build: None,
            build_sha256: None,
            content: Vec::new(),
            content_sha256: None,
        };
        World {
            dir,
            key: signing_key(1),
            state,
            staged,
        }
    }

    fn run_with(
        world: &World,
        state: &ClientState,
        site: &FakeSite,
        staged: Option<&Path>,
    ) -> (ClientState, Outcome) {
        let key = world.key.verifying_key();
        let builds = world.dir.path().join("builds");
        let launcher = launcher(world);
        let sync = Sync {
            site,
            launcher: &launcher,
            key: &key,
            builds_dir: &builds,
            staged,
        };
        sync.run(state)
    }

    /// A launch without a runtime slot: the game starts from the version folder.
    fn run(world: &World, site: &FakeSite) -> (ClientState, Outcome) {
        run_with(world, &world.state, site, None)
    }

    fn run_staged(world: &World, state: &ClientState, site: &FakeSite) -> (ClientState, Outcome) {
        run_with(world, state, site, Some(&world.staged))
    }

    fn launcher(world: &World) -> FakeLauncher {
        FakeLauncher::new(world.dir.path(), &["base-1"], Some("player-choice"))
    }

    fn read(path: &Path) -> Vec<u8> {
        fs::read(path).expect("read")
    }

    #[test]
    fn a_newer_build_replaces_the_program_in_place_without_a_slot() {
        let world = world(1);
        let (state, outcome) = run(&world, &signed_site(&world, 2, "base-1"));
        assert_eq!(outcome, Outcome::Updated(2));
        assert_eq!(state.sequence, 2);
        assert_eq!(read(&world.state.program), NEW_PROGRAM);
    }

    #[test]
    fn a_newer_build_goes_into_the_staged_copy_this_launch_starts() {
        let world = world(1);
        let (state, outcome) = run_staged(&world, &world.state, &signed_site(&world, 2, "base-1"));
        assert_eq!(outcome, Outcome::Updated(2));
        assert_eq!(read(&world.staged), NEW_PROGRAM);
        // The version folder's file is held by the launcher during a launch: left alone.
        assert_eq!(read(&world.state.program), OLD_PROGRAM);
        assert_eq!(read(state.build.as_deref().expect("build")), NEW_PROGRAM);
    }

    #[test]
    fn every_later_launch_gets_the_build_without_downloading_it() {
        let world = world(1);
        let site = signed_site(&world, 2, "base-1");
        let (state, _) = run_staged(&world, &world.state, &site);
        // The next launch: the launcher stages the version folder's old program again.
        fs::write(&world.staged, OLD_PROGRAM).expect("write");
        let (_, outcome) = run_staged(&world, &state, &site);
        assert_eq!(outcome, Outcome::UpToDate);
        assert_eq!(read(&world.staged), NEW_PROGRAM);
        assert_eq!(site.downloads.get(), 1);
    }

    #[test]
    fn the_build_is_staged_even_when_the_site_is_down() {
        let world = world(1);
        let (state, _) = run_staged(&world, &world.state, &signed_site(&world, 2, "base-1"));
        fs::write(&world.staged, OLD_PROGRAM).expect("write");
        let down = FakeSite::default();
        down.is_down.set(true);
        let (_, outcome) = run_staged(&world, &state, &down);
        assert!(matches!(outcome, Outcome::Kept(_)));
        assert_eq!(read(&world.staged), NEW_PROGRAM);
    }

    #[test]
    fn a_damaged_kept_build_is_not_staged() {
        let world = world(1);
        let (state, _) = run_staged(&world, &world.state, &signed_site(&world, 2, "base-1"));
        fs::write(state.build.as_deref().expect("build"), b"MZ tampered").expect("write");
        fs::write(&world.staged, OLD_PROGRAM).expect("write");
        let (_, outcome) = run_staged(&world, &state, &signed_site(&world, 2, "base-1"));
        assert!(matches!(outcome, Outcome::Kept(_)));
        assert_eq!(read(&world.staged), OLD_PROGRAM);
    }

    #[test]
    fn only_the_current_build_is_kept() {
        let world = world(1);
        let (state, _) = run_staged(&world, &world.state, &signed_site(&world, 2, "base-1"));
        run_staged(&world, &state, &signed_site(&world, 3, "base-1"));
        let builds: Vec<String> = fs::read_dir(world.dir.path().join("builds"))
            .expect("list")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(builds, ["3"]);
    }

    #[test]
    fn the_same_build_downloads_nothing() {
        let world = world(2);
        let site = signed_site(&world, 2, "base-1");
        assert_eq!(run(&world, &site).1, Outcome::UpToDate);
        assert_eq!(site.downloads.get(), 0);
    }

    #[test]
    fn an_older_build_is_refused() {
        let world = world(5);
        let (state, outcome) = run(&world, &signed_site(&world, 4, "base-1"));
        assert!(matches!(outcome, Outcome::Kept(_)));
        assert_eq!(state.sequence, 5);
        assert_eq!(read(&world.state.program), OLD_PROGRAM);
    }

    #[test]
    fn a_build_signed_by_someone_else_is_refused() {
        let world = world(1);
        let file = gzip(NEW_PROGRAM);
        let signed =
            Signed::create(&manifest_for(2, "base-1", &file), &signing_key(9)).expect("sign");
        let site = site_serving(signed, file);
        assert!(matches!(
            run_staged(&world, &world.state, &site).1,
            Outcome::Kept(_)
        ));
        assert_eq!(site.downloads.get(), 0);
        assert_eq!(read(&world.staged), OLD_PROGRAM);
    }

    #[test]
    fn a_swapped_file_under_a_genuine_manifest_is_refused() {
        let world = world(1);
        let file = gzip(NEW_PROGRAM);
        let signed = Signed::create(&manifest_for(2, "base-1", &file), &world.key).expect("sign");
        let site = site_serving(signed, gzip(b"MZ evil game"));
        assert!(matches!(
            run_staged(&world, &world.state, &site).1,
            Outcome::Kept(_)
        ));
        assert_eq!(read(&world.staged), OLD_PROGRAM);
        assert_eq!(read(&world.state.program), OLD_PROGRAM);
    }

    #[test]
    fn the_game_still_starts_when_the_site_is_down() {
        let world = world(1);
        let site = FakeSite::default();
        site.is_down.set(true);
        let (state, outcome) = run(&world, &site);
        assert!(matches!(outcome, Outcome::Kept(_)));
        assert_eq!(state, world.state);
    }

    #[test]
    fn no_temporary_file_is_left_after_a_refused_download() {
        let world = world(1);
        let file = gzip(NEW_PROGRAM);
        let signed = Signed::create(&manifest_for(2, "base-1", &file), &world.key).expect("sign");
        run(&world, &site_serving(signed, gzip(b"bad")));
        let folder = world.state.program.parent().expect("parent");
        assert_eq!(fs::read_dir(folder).expect("list").count(), 1);
        let kept = world.dir.path().join("builds").join("2");
        assert_eq!(fs::read_dir(kept).expect("list").count(), 0);
    }

    #[test]
    fn a_new_horizon_base_installs_and_asks_for_another_launch() {
        let world = world(1);
        let key = world.key.verifying_key();
        let builds = world.dir.path().join("builds");
        let launcher = launcher(&world);
        let sync = Sync {
            site: &signed_site(&world, 2, "base-2"),
            launcher: &launcher,
            key: &key,
            builds_dir: &builds,
            staged: Some(&world.staged),
        };
        let (state, outcome) = sync.run(&world.state);
        assert_eq!(outcome, Outcome::RestartNeeded(2));
        assert_eq!(state.horizon_base, "base-2");
        assert_eq!(state.version_tag, "imported-1");
        assert!(state.build.is_some());
        let calls = launcher.calls.borrow();
        assert!(calls.contains(&"install base-2".to_owned()));
        assert!(calls.contains(&"pin pack imported-1".to_owned()));
        assert!(calls.contains(&"remove imported-0".to_owned()));
        assert_eq!(
            launcher.state.borrow().selected.as_deref(),
            Some("player-choice")
        );
    }

    #[test]
    fn an_old_state_file_without_a_kept_build_still_reads() {
        let world = world(1);
        let mut json = serde_json::to_value(&world.state).expect("json");
        let object = json.as_object_mut().expect("object");
        object.remove("build");
        object.remove("build_sha256");
        let state: ClientState = serde_json::from_value(json).expect("reads");
        assert_eq!(state.build, None);
    }
}
