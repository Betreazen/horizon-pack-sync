//! Keeps a player's game and the server on the same signed build of the mod pack.
//! How it fits together: README.md, docs/owner-guide.md and docs/security.md.

mod client;
mod files;
mod launcher;
mod manifest;
mod release;
mod server;
mod setup;
mod site;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ed25519_dalek::VerifyingKey;

use crate::client::{ClientState, Outcome as ClientOutcome, Sync};
use crate::launcher::RealLauncher;
use crate::manifest::parse_public_key;
use crate::server::{HorizonServer, Outcome as ServerOutcome, ServerState, Updater};
use crate::site::HttpSite;

/// The owner's public key from pack-sync.toml; a build without one refuses to update anything.
const PUBLIC_KEY: &str = env!("PACK_PUBLIC_KEY");
const CLIENT_STATE_FILE: &str = "client.json";
const SERVER_STATE_FILE: &str = "server.json";
const LOG_FILE: &str = "pack-sync.log";
const BUILDS_FOLDER: &str = "builds";
const ENGINES_FOLDER: &str = "engines";
/// Set by the Windows launcher for the pre-launch command: the folder of the game copy it starts.
const RUNTIME_SLOT_VARIABLE: &str = "HORIZON_WINDOWS_RUNTIME_SLOT";
const SLOT_GAME: [&str; 2] = ["game", "horizon-client.exe"];
const SERVER_KEY_VARIABLE: &str = "PACK_SERVER_KEY";

#[derive(Parser)]
#[command(about = "Keeps the game and the server on the same signed build of the pack")]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// First install on a player's computer
    Setup {
        /// The personal code from your profile on the site
        #[arg(long)]
        code: Option<String>,
        /// The site's address, when it is not the usual one
        #[arg(long)]
        site: Option<String>,
    },
    /// Update the game; the launcher runs this before each start
    Client,
    /// Watch for new builds and move the dedicated server to them
    Server {
        #[arg(long)]
        site: String,
        #[arg(long)]
        horizon_server: PathBuf,
        #[arg(long)]
        server_dir: PathBuf,
        /// Where this tool keeps its state and the engines
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long, default_value_t = 300)]
        poll_seconds: u64,
        #[arg(long, default_value_t = 120)]
        warning_seconds: u64,
        #[arg(long, default_value_t = 300)]
        start_timeout_seconds: u64,
    },
    /// Create the owner's signing key
    Keygen {
        /// A new file for the private key; keep it out of every repository
        #[arg(long)]
        out: PathBuf,
    },
    /// Check that a private key belongs to the public key built into this program
    CheckKey {
        #[arg(long)]
        key: PathBuf,
    },
    /// Describe and sign a build
    Release {
        #[arg(long)]
        sequence: u64,
        #[arg(long)]
        horizon_base: String,
        #[arg(long)]
        client: PathBuf,
        #[arg(long)]
        server: PathBuf,
        #[arg(long)]
        key: PathBuf,
        /// Where to write the signed manifest
        #[arg(long)]
        out: PathBuf,
    },
    /// Check a signed build as an updater would: the signature and every file it names
    Verify {
        #[arg(long)]
        manifest: PathBuf,
        /// The folder holding the build's files
        #[arg(long)]
        dir: PathBuf,
    },
}

fn verify(manifest: &Path, dir: &Path) -> Result<()> {
    let signed: manifest::Signed = read_json(manifest)?;
    let opened = signed.open(&embedded_key()?)?;
    for (name, artifact) in &opened.artifacts {
        files::check_artifact(&dir.join(&artifact.asset), artifact)
            .with_context(|| format!("{name} does not match"))?;
        println!("{name}: {} matches", artifact.asset);
    }
    println!(
        "build {} on {}: signature valid",
        opened.sequence, opened.horizon_base
    );
    Ok(())
}

fn embedded_key() -> Result<VerifyingKey> {
    parse_public_key(PUBLIC_KEY).context("this pack-sync was built without the pack's public key")
}

fn home_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("cannot find this program")?;
    Ok(exe
        .parent()
        .context("this program has no folder")?
        .to_owned())
}

fn log(dir: &Path, message: &str) {
    eprintln!("{message}");
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs());
    let line = format!("{seconds} {message}\n");
    let written = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LOG_FILE))
        .and_then(|mut file| file.write_all(line.as_bytes()));
    if let Err(error) = written {
        eprintln!("cannot write the log: {error}");
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    serde_json::from_slice(&text).with_context(|| format!("{} is damaged", path.display()))
}

/// The game copy this launch will start, when the launcher staged one.
fn staged_program() -> Option<PathBuf> {
    let slot = std::env::var_os(RUNTIME_SLOT_VARIABLE)?;
    let program = SLOT_GAME
        .iter()
        .fold(PathBuf::from(slot), |path, part| path.join(part));
    program.is_file().then_some(program)
}

/// Exit 0 starts the game, so only a finished update that needs a new launch stops it.
fn run_client() -> Result<ExitCode> {
    let home = home_dir()?;
    let state_path = home.join(CLIENT_STATE_FILE);
    let state: ClientState = read_json(&state_path)?;
    let key = embedded_key()?;
    let site = HttpSite::new(&state.site, &state.code)?;
    let launcher = RealLauncher::new(state.launcher.clone());
    let builds = home.join(BUILDS_FOLDER);
    let staged = staged_program();
    let target = staged.as_deref().unwrap_or(&state.program);
    log(&home, &format!("this launch starts {}", target.display()));
    let sync = Sync {
        site: &site,
        launcher: &launcher,
        key: &key,
        builds_dir: &builds,
        staged: staged.as_deref(),
    };
    let (next, outcome) = sync.run(&state);
    if next != state
        && let Err(error) = files::write_json(&state_path, &next)
    {
        // The worst result is downloading this build once more.
        log(&home, &format!("cannot save the state: {error:#}"));
    }
    match outcome {
        ClientOutcome::UpToDate => log(&home, &format!("build {} is current", next.sequence)),
        ClientOutcome::Updated(sequence) => log(&home, &format!("updated to build {sequence}")),
        ClientOutcome::Kept(reason) => log(&home, &format!("no update: {reason}")),
        ClientOutcome::RestartNeeded(sequence) => {
            log(
                &home,
                &format!("build {sequence} installed: press Play again to use it"),
            );
            return Ok(ExitCode::FAILURE);
        }
    }
    Ok(ExitCode::SUCCESS)
}

struct ServerOptions {
    site: String,
    horizon_server: PathBuf,
    server_dir: PathBuf,
    state_dir: PathBuf,
    poll: Duration,
    warning: Duration,
    start_timeout: Duration,
}

fn run_server(options: &ServerOptions) -> Result<()> {
    let credential = std::env::var(SERVER_KEY_VARIABLE)
        .with_context(|| format!("{SERVER_KEY_VARIABLE} is not set"))?;
    fs::create_dir_all(&options.state_dir).context("cannot create the state folder")?;
    let key = embedded_key()?;
    let site = HttpSite::new(&options.site, &credential)?;
    let game = HorizonServer {
        program: options.horizon_server.clone(),
        dir: options.server_dir.clone(),
    };
    let engines = options.state_dir.join(ENGINES_FOLDER);
    let state_path = options.state_dir.join(SERVER_STATE_FILE);
    let sleep = |duration: Duration| std::thread::sleep(duration);
    let updater = Updater {
        site: &site,
        game: &game,
        key: &key,
        engines: &engines,
        warning: options.warning,
        start_timeout: options.start_timeout,
        sleep: &sleep,
    };
    let mut state: ServerState = if state_path.exists() {
        read_json(&state_path)?
    } else {
        ServerState::default()
    };
    loop {
        let (next, outcome) = updater.run(&state);
        if next != state {
            files::write_json(&state_path, &next)?;
        }
        state = next;
        match outcome {
            ServerOutcome::UpToDate => {}
            ServerOutcome::Updated(sequence) => {
                log(
                    &options.state_dir,
                    &format!("server moved to build {sequence}"),
                );
            }
            ServerOutcome::RolledBack(sequence) => {
                log(
                    &options.state_dir,
                    &format!("build {sequence} would not start: rolled back"),
                );
            }
            ServerOutcome::Stranded(sequence, reason) => {
                log(
                    &options.state_dir,
                    &format!("build {sequence} would not start, fix by hand: {reason}"),
                );
            }
            ServerOutcome::Skipped(reason) => {
                log(&options.state_dir, &format!("no update: {reason}"));
            }
        }
        std::thread::sleep(options.poll);
    }
}

fn dispatch(action: Action) -> Result<ExitCode> {
    match action {
        // A broken updater must not keep the player out of the game.
        Action::Client => {
            return Ok(run_client().unwrap_or_else(|error| {
                match home_dir() {
                    Ok(home) => log(&home, &format!("no update: {error:#}")),
                    Err(_) => eprintln!("no update: {error:#}"),
                }
                ExitCode::SUCCESS
            }));
        }
        Action::Setup { code, site } => setup::run(code, site, &embedded_key()?)?,
        Action::Server {
            site,
            horizon_server,
            server_dir,
            state_dir,
            poll_seconds,
            warning_seconds,
            start_timeout_seconds,
        } => run_server(&ServerOptions {
            site,
            horizon_server,
            server_dir,
            state_dir,
            poll: Duration::from_secs(poll_seconds),
            warning: Duration::from_secs(warning_seconds),
            start_timeout: Duration::from_secs(start_timeout_seconds),
        })?,
        Action::Keygen { out } => release::keygen(&out)?,
        Action::CheckKey { key } => release::check_key(&key, &embedded_key()?)?,
        Action::Verify { manifest, dir } => verify(&manifest, &dir)?,
        Action::Release {
            sequence,
            horizon_base,
            client,
            server,
            key,
            out,
        } => {
            release::check_key(&key, &embedded_key()?)?;
            release::release(&release::Input {
                sequence,
                horizon_base,
                client,
                server,
                key,
                out,
            })?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match dispatch(Cli::parse().command) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("pack-sync: {error:#}");
            ExitCode::FAILURE
        }
    }
}
