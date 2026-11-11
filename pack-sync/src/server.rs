//! Moves the dedicated server to the newest build, and back again if that build will not run.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};

use crate::files::{check_artifact, untar_gz};
use crate::manifest::{Manifest, SERVER_ARTIFACT};
use crate::site::Site;

const ENGINE_PROGRAM: &str = if cfg!(windows) {
    "horizon-engine.exe"
} else {
    "horizon-engine"
};
const ENGINES_KEPT: usize = 3;
/// Where an export keeps its game files, one folder per Minecraft version.
const EXPORTED_RESOURCES: &str = ".local/horizon-client";
const RESOURCES_MANIFEST: &str = "horizon-resources.json";
/// Left beside a fully unpacked engine, so a later round does not download it again.
const UNPACKED_MARKER: &str = ".pack-sync-unpacked";
const NOT_RUNNING: &str = "no server is running";
const POLL_WHILE_STARTING: Duration = Duration::from_secs(5);
/// Up this long without the supervisor restarting it: the build really runs.
const STABLE_UPTIME_SECONDS: u64 = 30;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ServerState {
    pub sequence: u64,
    /// Whether the site knows `sequence` is what runs here.
    pub reported: bool,
    /// Builds that failed to start and must not be tried again.
    pub failed: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    UpToDate,
    Updated(u64),
    RolledBack(u64),
    /// The build would not start and the old engine could not be restored: needs a person.
    Stranded(u64, String),
    Skipped(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GameStatus {
    pub is_stable: bool,
    pub players: usize,
}

pub trait Game {
    fn status(&self) -> Result<GameStatus>;
    fn say(&self, message: &str) -> Result<()>;
    fn backup(&self, name: &str) -> Result<()>;
    fn engine(&self) -> Result<Option<PathBuf>>;
    fn set_engine(&self, path: &Path) -> Result<()>;
    fn restart(&self) -> Result<()>;
}

pub struct Updater<'a> {
    pub site: &'a dyn Site,
    pub game: &'a dyn Game,
    pub key: &'a VerifyingKey,
    pub engines: &'a Path,
    pub warning: Duration,
    pub start_timeout: Duration,
    pub sleep: &'a dyn Fn(Duration),
}

impl Updater<'_> {
    pub fn run(&self, state: &ServerState) -> (ServerState, Outcome) {
        match self.try_run(state) {
            Ok(result) => result,
            Err(error) => (state.clone(), Outcome::Skipped(format!("{error:#}"))),
        }
    }

    fn try_run(&self, state: &ServerState) -> Result<(ServerState, Outcome)> {
        let state = self.report(state);
        let manifest = self.site.manifest()?.open(self.key)?;
        if manifest.sequence <= state.sequence {
            return Ok((state, Outcome::UpToDate));
        }
        if state.failed.contains(&manifest.sequence) {
            let reason = format!("build {} already failed to start", manifest.sequence);
            return Ok((state, Outcome::Skipped(reason)));
        }
        let engine = self.unpack(&manifest)?;
        let previous = self.game.engine()?;
        self.warn_players()?;
        self.game
            .backup(&format!("before-pack-{}", manifest.sequence))?;
        self.game.set_engine(&engine)?;
        self.game.restart()?;
        if self.wait_until_stable() {
            let done = ServerState {
                sequence: manifest.sequence,
                reported: false,
                failed: state.failed.clone(),
            };
            self.prune(manifest.sequence);
            return Ok((self.report(&done), Outcome::Updated(manifest.sequence)));
        }
        // Recorded whatever the rollback does: retrying would restart the server every round.
        let mut failed = state.failed.clone();
        failed.push(manifest.sequence);
        let next = ServerState { failed, ..state };
        let outcome = match self.roll_back(previous.as_deref()) {
            Ok(()) => Outcome::RolledBack(manifest.sequence),
            Err(error) => Outcome::Stranded(manifest.sequence, format!("{error:#}")),
        };
        Ok((next, outcome))
    }

    /// Tells the site what runs here; a site that is down just hears about it next round.
    fn report(&self, state: &ServerState) -> ServerState {
        if state.reported || state.sequence == 0 {
            return state.clone();
        }
        match self.site.confirm(state.sequence) {
            Ok(()) => ServerState {
                reported: true,
                ..state.clone()
            },
            Err(error) => {
                eprintln!("cannot report build {}: {error:#}", state.sequence);
                state.clone()
            }
        }
    }

    fn unpack(&self, manifest: &Manifest) -> Result<PathBuf> {
        let artifact = manifest.artifact(SERVER_ARTIFACT)?;
        fs::create_dir_all(self.engines).context("cannot create the engines folder")?;
        let archive = self.engines.join(format!("{}.download", manifest.sequence));
        let folder = self.engines.join(manifest.sequence.to_string());
        let marker = folder.join(UNPACKED_MARKER);
        if !marker.exists() {
            let result = self
                .site
                .download(manifest.sequence, &artifact.asset, &archive)
                .and_then(|()| check_artifact(&archive, artifact))
                .and_then(|()| untar_gz(&archive, &folder))
                .and_then(|()| fs::write(&marker, b"").context("cannot mark the engine unpacked"));
            let _ = fs::remove_file(&archive);
            result?;
        }
        let engine = folder.join(ENGINE_PROGRAM);
        if !engine.exists() {
            bail!("the build has no {ENGINE_PROGRAM}");
        }
        make_executable(&engine)?;
        Ok(engine)
    }

    fn warn_players(&self) -> Result<()> {
        if self.game.status()?.players == 0 {
            return Ok(());
        }
        let minutes = self.warning.as_secs().div_ceil(60);
        self.game.say(&format!(
            "Вышла новая версия сборки, сервер перезапустится через {minutes} мин. \
             После этого перезапустите игру."
        ))?;
        (self.sleep)(self.warning);
        Ok(())
    }

    fn wait_until_stable(&self) -> bool {
        let started = Instant::now();
        while started.elapsed() < self.start_timeout {
            if self.game.status().is_ok_and(|status| status.is_stable) {
                return true;
            }
            (self.sleep)(POLL_WHILE_STARTING);
        }
        false
    }

    fn roll_back(&self, previous: Option<&Path>) -> Result<()> {
        let path = previous.context("the new build failed and there is no earlier engine")?;
        self.game.set_engine(path)?;
        self.game.restart()
    }

    /// Keeps the newest few engines for manual rollback; a leftover costs only disk.
    fn prune(&self, current: u64) {
        let Ok(entries) = fs::read_dir(self.engines) else {
            return;
        };
        let mut numbered: Vec<u64> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
            .filter(|sequence| *sequence != current)
            .collect();
        numbered.sort_unstable_by(|a, b| b.cmp(a));
        for old in numbered.into_iter().skip(ENGINES_KEPT - 1) {
            if let Err(error) = fs::remove_dir_all(self.engines.join(old.to_string())) {
                eprintln!("cannot remove engine {old}: {error}");
            }
        }
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .context("cannot mark the engine executable")
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// The real server, driven through `horizon-server`.
pub struct HorizonServer {
    pub program: PathBuf,
    pub dir: PathBuf,
}

impl HorizonServer {
    fn run(&self, arguments: &[&str]) -> Result<Output> {
        let output = Command::new(&self.program)
            .arg("--dir")
            .arg(&self.dir)
            .args(arguments)
            .output()
            .context("cannot start horizon-server")?;
        if !output.status.success() {
            bail!(
                "horizon-server {}: {}",
                arguments.first().copied().unwrap_or_default(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output)
    }
}

impl Game for HorizonServer {
    fn status(&self) -> Result<GameStatus> {
        parse_status(&self.run(&["status", "--json"])?.stdout)
    }

    fn say(&self, message: &str) -> Result<()> {
        self.run(&["cmd", "say", message]).map(drop)
    }

    fn backup(&self, name: &str) -> Result<()> {
        self.run(&["backup", name]).map(drop)
    }

    fn engine(&self) -> Result<Option<PathBuf>> {
        parse_engine(&self.run(&["config", "--json"])?.stdout)
    }

    /// Points the server at the engine and at the game files exported with it: the supervisor
    /// looks for them beside its own install, not beside the engine.
    fn set_engine(&self, path: &Path) -> Result<()> {
        let engine = path.to_str().context("the engine path is not valid text")?;
        let resources = resources_beside(path);
        let resources = resources
            .as_deref()
            .map_or(Some(""), Path::to_str)
            .context("the resources path is not valid text")?;
        self.run(&[
            "config",
            "set",
            &format!("engine.path={engine}"),
            &format!("engine.resources={resources}"),
        ])
        .map(drop)
    }

    /// A server that is down cannot be restarted, and needs no restart: systemd starts it again
    /// and it reads the new engine then.
    fn restart(&self) -> Result<()> {
        match self.run(&["restart"]) {
            Err(error) if is_not_running(&format!("{error:#}")) => Ok(()),
            other => other.map(drop),
        }
    }
}

fn is_not_running(message: &str) -> bool {
    message.contains(NOT_RUNNING)
}

/// The prepared game files a server export carries, if any.
fn resources_beside(engine: &Path) -> Option<PathBuf> {
    let versions = engine.parent()?.join(EXPORTED_RESOURCES);
    fs::read_dir(versions)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|dir| dir.join(RESOURCES_MANIFEST).is_file())
}

fn parse_status(json: &[u8]) -> Result<GameStatus> {
    let value: serde_json::Value = serde_json::from_slice(json).context("unexpected status")?;
    let game = &value["sample"]["game"];
    let is_running = value["phase"] == "running" && game.is_object();
    let uptime = value["engine_uptime_seconds"].as_u64().unwrap_or(0);
    Ok(GameStatus {
        is_stable: is_running && uptime >= STABLE_UPTIME_SECONDS,
        players: game["players"].as_array().map_or(0, Vec::len),
    })
}

fn parse_engine(json: &[u8]) -> Result<Option<PathBuf>> {
    let value: serde_json::Value = serde_json::from_slice(json).context("unexpected config")?;
    Ok(value["engine"]["path"]
        .as_str()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::manifest::tests::signing_key;
    use crate::manifest::{Artifact, Signed};
    use crate::site::tests::FakeSite;

    const ASSET: &str = "server.tar.gz";

    fn engine_archive() -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, ENGINE_PROGRAM, &b"x"[..])
            .expect("tar");
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder
            .write_all(&builder.into_inner().expect("tar"))
            .expect("gz");
        encoder.finish().expect("finish")
    }

    fn site_with_build(sequence: u64) -> FakeSite {
        let file = engine_archive();
        let artifact = Artifact {
            asset: ASSET.into(),
            sha256: hex::encode(Sha256::digest(&file)),
            size: file.len() as u64,
        };
        let manifest = Manifest::new(
            sequence,
            "base".into(),
            BTreeMap::from([(SERVER_ARTIFACT.into(), artifact)]),
        );
        let site = FakeSite {
            files: BTreeMap::from([(ASSET.to_owned(), file)]),
            ..FakeSite::default()
        };
        *site.signed.borrow_mut() = Some(Signed::create(&manifest, &signing_key(1)).expect("sign"));
        site
    }

    #[derive(Default)]
    struct FakeGame {
        comes_up: Cell<bool>,
        players: Cell<usize>,
        calls: RefCell<Vec<String>>,
        engine: RefCell<Option<PathBuf>>,
    }

    impl FakeGame {
        fn calls_named(&self, name: &str) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|call| call.starts_with(name))
                .count()
        }
    }

    impl Game for FakeGame {
        fn status(&self) -> Result<GameStatus> {
            Ok(GameStatus {
                is_stable: self.comes_up.get(),
                players: self.players.get(),
            })
        }

        fn say(&self, _message: &str) -> Result<()> {
            self.calls.borrow_mut().push("say".into());
            Ok(())
        }

        fn backup(&self, name: &str) -> Result<()> {
            self.calls.borrow_mut().push(format!("backup {name}"));
            Ok(())
        }

        fn engine(&self) -> Result<Option<PathBuf>> {
            Ok(self.engine.borrow().clone())
        }

        fn set_engine(&self, path: &Path) -> Result<()> {
            self.calls.borrow_mut().push("set_engine".into());
            *self.engine.borrow_mut() = Some(path.to_owned());
            Ok(())
        }

        fn restart(&self) -> Result<()> {
            self.calls.borrow_mut().push("restart".into());
            Ok(())
        }
    }

    fn run(
        site: &FakeSite,
        game: &FakeGame,
        dir: &Path,
        state: &ServerState,
    ) -> (ServerState, Outcome) {
        let key = signing_key(1).verifying_key();
        let sleep = |_: Duration| {};
        let updater = Updater {
            site,
            game,
            key: &key,
            engines: dir,
            warning: Duration::from_secs(120),
            start_timeout: Duration::from_millis(5),
            sleep: &sleep,
        };
        updater.run(state)
    }

    fn state_at(sequence: u64) -> ServerState {
        ServerState {
            sequence,
            reported: true,
            failed: Vec::new(),
        }
    }

    #[test]
    fn a_new_build_is_installed_started_and_confirmed() {
        let dir = tempfile::tempdir().expect("dir");
        let site = site_with_build(2);
        let game = FakeGame::default();
        game.comes_up.set(true);
        let (state, outcome) = run(&site, &game, dir.path(), &state_at(1));
        assert_eq!(outcome, Outcome::Updated(2));
        assert_eq!(
            state,
            ServerState {
                sequence: 2,
                reported: true,
                failed: Vec::new()
            }
        );
        assert_eq!(site.confirmed.get(), Some(2));
        assert!(
            game.engine
                .borrow()
                .as_ref()
                .expect("engine")
                .ends_with(ENGINE_PROGRAM)
        );
        assert_eq!(game.calls_named("backup before-pack-2"), 1);
    }

    #[test]
    fn a_build_that_will_not_start_is_rolled_back_and_not_confirmed() {
        let dir = tempfile::tempdir().expect("dir");
        let site = site_with_build(2);
        let game = FakeGame::default();
        *game.engine.borrow_mut() = Some(PathBuf::from("old-engine"));
        let (state, outcome) = run(&site, &game, dir.path(), &state_at(1));
        assert_eq!(outcome, Outcome::RolledBack(2));
        assert_eq!(state.sequence, 1);
        assert_eq!(state.failed, vec![2]);
        assert_eq!(
            game.engine.borrow().as_deref(),
            Some(Path::new("old-engine"))
        );
        assert_eq!(site.confirmed.get(), None);
        assert_eq!(game.calls_named("restart"), 2);
    }

    #[test]
    fn a_first_build_that_will_not_start_is_never_retried() {
        let dir = tempfile::tempdir().expect("dir");
        let site = site_with_build(2);
        let game = FakeGame::default();
        let (state, outcome) = run(&site, &game, dir.path(), &state_at(1));
        assert!(matches!(outcome, Outcome::Stranded(2, _)));
        assert_eq!(state.failed, vec![2]);
        assert_eq!(game.calls_named("restart"), 1);

        let (_, again) = run(&site, &game, dir.path(), &state);
        assert!(matches!(again, Outcome::Skipped(_)));
        assert_eq!(game.calls_named("restart"), 1);
    }

    #[test]
    fn an_unpacked_build_is_not_downloaded_again() {
        let dir = tempfile::tempdir().expect("dir");
        let site = site_with_build(2);
        let game = FakeGame::default();
        game.comes_up.set(true);
        let updater_state = state_at(1);
        // A round that stops after unpacking, as when the site is unreachable for the report.
        let key = signing_key(1).verifying_key();
        let sleep = |_: Duration| {};
        let updater = Updater {
            site: &site,
            game: &game,
            key: &key,
            engines: dir.path(),
            warning: Duration::ZERO,
            start_timeout: Duration::from_millis(5),
            sleep: &sleep,
        };
        let manifest = site.manifest().expect("manifest").open(&key).expect("open");
        updater.unpack(&manifest).expect("first unpack");
        updater.unpack(&manifest).expect("second unpack");
        assert_eq!(site.downloads.get(), 1);
        assert_eq!(
            run(&site, &game, dir.path(), &updater_state).1,
            Outcome::Updated(2)
        );
        assert_eq!(site.downloads.get(), 1);
    }

    #[test]
    fn restarting_a_stopped_server_is_not_an_error() {
        assert!(is_not_running(
            "horizon-server restart: horizon-server: no server is running in /srv/minecraft"
        ));
        assert!(!is_not_running("horizon-server restart: permission denied"));
    }

    #[test]
    fn the_game_files_beside_an_engine_are_found() {
        let dir = tempfile::tempdir().expect("dir");
        let engine = dir.path().join(ENGINE_PROGRAM);
        assert_eq!(resources_beside(&engine), None);
        let files = dir.path().join(EXPORTED_RESOURCES).join("26.3");
        fs::create_dir_all(&files).expect("mkdir");
        fs::create_dir_all(dir.path().join(EXPORTED_RESOURCES).join("empty")).expect("mkdir");
        fs::write(files.join(RESOURCES_MANIFEST), b"{}").expect("write");
        assert_eq!(resources_beside(&engine), Some(files));
    }

    #[test]
    fn a_failed_build_is_not_retried() {
        let dir = tempfile::tempdir().expect("dir");
        let site = site_with_build(2);
        let game = FakeGame::default();
        let state = ServerState {
            sequence: 1,
            reported: true,
            failed: vec![2],
        };
        assert!(matches!(
            run(&site, &game, dir.path(), &state).1,
            Outcome::Skipped(_)
        ));
        assert_eq!(game.calls_named("restart"), 0);
    }

    #[test]
    fn players_are_warned_only_when_someone_is_online() {
        let dir = tempfile::tempdir().expect("dir");
        let site = site_with_build(2);
        let game = FakeGame::default();
        game.comes_up.set(true);
        run(&site, &game, dir.path(), &state_at(1));
        assert_eq!(game.calls_named("say"), 0);

        let site = site_with_build(3);
        game.players.set(1);
        run(&site, &game, dir.path(), &state_at(2));
        assert_eq!(game.calls_named("say"), 1);
    }

    #[test]
    fn a_forged_build_touches_nothing() {
        let dir = tempfile::tempdir().expect("dir");
        let site = site_with_build(2);
        let forged = site.signed.borrow().clone().expect("signed");
        let other = forged.manifest.replace("\"sequence\":2", "\"sequence\":3");
        *site.signed.borrow_mut() = Some(Signed {
            manifest: other,
            ..forged
        });
        let game = FakeGame::default();
        assert!(matches!(
            run(&site, &game, dir.path(), &state_at(1)).1,
            Outcome::Skipped(_)
        ));
        assert_eq!(site.downloads.get(), 0);
        assert_eq!(game.calls.borrow().len(), 0);
    }

    #[test]
    fn an_unreported_build_is_reported_on_the_next_round() {
        let dir = tempfile::tempdir().expect("dir");
        let site = site_with_build(2);
        let game = FakeGame::default();
        let state = ServerState {
            sequence: 2,
            reported: false,
            failed: Vec::new(),
        };
        let (state, outcome) = run(&site, &game, dir.path(), &state);
        assert_eq!(outcome, Outcome::UpToDate);
        assert!(state.reported);
        assert_eq!(site.confirmed.get(), Some(2));
    }

    #[test]
    fn only_the_newest_engines_are_kept() {
        let dir = tempfile::tempdir().expect("dir");
        for old in 1..=4 {
            fs::create_dir_all(dir.path().join(old.to_string())).expect("mkdir");
        }
        let site = site_with_build(5);
        let game = FakeGame::default();
        game.comes_up.set(true);
        run(&site, &game, dir.path(), &state_at(4));
        let mut left: Vec<String> = fs::read_dir(dir.path())
            .expect("list")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["3", "4", "5"]);
    }

    #[test]
    fn status_needs_a_running_game_up_long_enough() {
        let json = |phase: &str, uptime: u64| {
            format!(
                r#"{{"phase":"{phase}","engine_uptime_seconds":{uptime},
                    "sample":{{"game":{{"players":[{{"name":"a"}}]}}}}}}"#
            )
        };
        let parsed = parse_status(json("running", 60).as_bytes()).expect("parse");
        assert_eq!(
            parsed,
            GameStatus {
                is_stable: true,
                players: 1
            }
        );
        assert!(
            !parse_status(json("running", 3).as_bytes())
                .expect("parse")
                .is_stable
        );
        assert!(
            !parse_status(json("starting", 60).as_bytes())
                .expect("parse")
                .is_stable
        );
    }

    #[test]
    fn status_without_a_game_sample_is_not_stable() {
        let json = br#"{"phase":"running","engine_uptime_seconds":99,"sample":null}"#;
        assert!(!parse_status(json).expect("parse").is_stable);
    }

    #[test]
    fn the_engine_path_is_read_from_the_config() {
        let parsed =
            parse_engine(br#"{"engine":{"path":"/srv/e/horizon-engine"}}"#).expect("parse");
        assert_eq!(parsed, Some(PathBuf::from("/srv/e/horizon-engine")));
        assert_eq!(
            parse_engine(br#"{"engine":{"path":""}}"#).expect("parse"),
            None
        );
        assert_eq!(parse_engine(br#"{"engine":{}}"#).expect("parse"), None);
    }
}
