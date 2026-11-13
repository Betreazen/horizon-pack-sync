//! Talks to the Horizon launcher through its command line.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const VERSIONS_FOLDER: &str = "versions";
const LAUNCHER_FOLDER: &str = "horizon-launcher";
const GAME_PROGRAM: &str = "horizon-client.exe";

pub trait Launcher {
    fn installed(&self) -> Result<Installed>;
    fn install(&self, version: &str) -> Result<()>;
    /// Imports the program on `base`'s game files; the imported tag and whether it is prepared.
    fn import(&self, program: &Path, base: &str) -> Result<Imported>;
    fn select(&self, version: &str) -> Result<()>;
    fn pin(&self, instance: &str, version: &str) -> Result<()>;
    fn prepare(&self, version: &str) -> Result<()>;
    fn remove(&self, version: &str) -> Result<()>;
    fn program_path(&self, version: &str) -> Result<PathBuf>;
    /// The instance's game folder, the one holding `options.txt` and `horizon-mods/`.
    fn instance_home(&self, instance: &str) -> Result<PathBuf>;
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct Installed {
    pub installed: Vec<String>,
    pub selected: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Imported {
    pub imported: String,
    pub prepared: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Instance {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub home: Option<PathBuf>,
}

pub struct RealLauncher {
    program: PathBuf,
}

impl RealLauncher {
    pub fn new(program: PathBuf) -> Self {
        Self { program }
    }

    /// The launcher in its default place, if this computer has one.
    pub fn default_path() -> Result<PathBuf> {
        let local = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
        let path = Path::new(&local)
            .join("HorizonLauncher")
            .join("horizon-launcher.exe");
        if !path.exists() {
            bail!("the Horizon launcher is not installed; install it first");
        }
        Ok(path)
    }

    fn run(&self, arguments: &[&str]) -> Result<Output> {
        let output = Command::new(&self.program)
            .args(arguments)
            .output()
            .context("cannot start the Horizon launcher")?;
        if !output.status.success() {
            bail!(
                "launcher {}: {}",
                arguments.first().copied().unwrap_or_default(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output)
    }

    pub fn instances(&self) -> Result<Vec<Instance>> {
        let output = self.run(&["--json", "instance", "list"])?;
        serde_json::from_slice(&output.stdout).context("unexpected instance list")
    }

    pub fn create_instance(&self, name: &str) -> Result<()> {
        self.run(&["instance", "create", name]).map(drop)
    }

    pub fn set_pre_launch(&self, instance: &str, command: &str) -> Result<()> {
        self.run(&["instance", "launch", instance, "--pre-launch", command])
            .map(drop)
    }
}

impl Launcher for RealLauncher {
    fn installed(&self) -> Result<Installed> {
        let output = self.run(&["--json", "versions", "--installed"])?;
        serde_json::from_slice(&output.stdout).context("unexpected version list")
    }

    fn install(&self, version: &str) -> Result<()> {
        self.run(&["install", version]).map(drop)
    }

    fn import(&self, program: &Path, base: &str) -> Result<Imported> {
        let path = program.to_str().context("the path is not valid text")?;
        let output = self.run(&[
            "--json",
            "import-build",
            path,
            "--version",
            base,
            "--trust-unsigned",
        ])?;
        serde_json::from_slice(&output.stdout).context("unexpected import result")
    }

    fn select(&self, version: &str) -> Result<()> {
        self.run(&["settings", "selected-version", version])
            .map(drop)
    }

    fn pin(&self, instance: &str, version: &str) -> Result<()> {
        self.run(&["instance", "version", instance, version])
            .map(drop)
    }

    fn prepare(&self, version: &str) -> Result<()> {
        self.run(&["prepare", "--version", version, "--download"])
            .map(drop)
    }

    fn remove(&self, version: &str) -> Result<()> {
        self.run(&["remove", version]).map(drop)
    }

    fn program_path(&self, version: &str) -> Result<PathBuf> {
        let data = match std::env::var_os("HORIZON_LAUNCHER_HOME").filter(|home| !home.is_empty()) {
            Some(home) => PathBuf::from(home),
            None => Path::new(&std::env::var_os("APPDATA").context("APPDATA is not set")?)
                .join(LAUNCHER_FOLDER),
        };
        let path = data.join(VERSIONS_FOLDER).join(version).join(GAME_PROGRAM);
        if !path.exists() {
            bail!("cannot find the game at {}", path.display());
        }
        Ok(path)
    }

    fn instance_home(&self, instance: &str) -> Result<PathBuf> {
        self.instances()?
            .into_iter()
            .find(|known| known.id == instance)
            .with_context(|| format!("the launcher has no instance {instance}"))?
            .home
            .with_context(|| format!("the launcher names no folder for instance {instance}"))
    }
}

#[cfg(test)]
pub mod tests {
    use std::cell::RefCell;

    use super::*;

    /// A launcher in memory that records every change asked of it.
    pub struct FakeLauncher {
        pub folder: PathBuf,
        pub state: RefCell<Installed>,
        pub calls: RefCell<Vec<String>>,
        pub next_tag: String,
    }

    impl FakeLauncher {
        pub fn new(folder: &Path, installed: &[&str], selected: Option<&str>) -> Self {
            Self {
                folder: folder.to_owned(),
                state: RefCell::new(Installed {
                    installed: installed.iter().map(ToString::to_string).collect(),
                    selected: selected.map(str::to_owned),
                }),
                calls: RefCell::new(Vec::new()),
                next_tag: "imported-1".into(),
            }
        }

        fn record(&self, call: String) {
            self.calls.borrow_mut().push(call);
        }
    }

    impl Launcher for FakeLauncher {
        fn installed(&self) -> Result<Installed> {
            Ok(self.state.borrow().clone())
        }

        fn install(&self, version: &str) -> Result<()> {
            self.record(format!("install {version}"));
            self.state.borrow_mut().installed.push(version.to_owned());
            Ok(())
        }

        fn import(&self, program: &Path, base: &str) -> Result<Imported> {
            self.record(format!("import {base}"));
            let folder = self.folder.join(&self.next_tag);
            std::fs::create_dir_all(&folder)?;
            std::fs::copy(program, folder.join(GAME_PROGRAM))?;
            self.state.borrow_mut().selected = Some(self.next_tag.clone());
            Ok(Imported {
                imported: self.next_tag.clone(),
                prepared: false,
            })
        }

        fn select(&self, version: &str) -> Result<()> {
            self.record(format!("select {version}"));
            self.state.borrow_mut().selected = Some(version.to_owned());
            Ok(())
        }

        fn pin(&self, instance: &str, version: &str) -> Result<()> {
            self.record(format!("pin {instance} {version}"));
            Ok(())
        }

        fn prepare(&self, version: &str) -> Result<()> {
            self.record(format!("prepare {version}"));
            Ok(())
        }

        fn remove(&self, version: &str) -> Result<()> {
            self.record(format!("remove {version}"));
            Ok(())
        }

        fn program_path(&self, version: &str) -> Result<PathBuf> {
            Ok(self.folder.join(version).join(GAME_PROGRAM))
        }

        fn instance_home(&self, instance: &str) -> Result<PathBuf> {
            Ok(self.folder.join("instances").join(instance))
        }
    }

    #[test]
    fn an_instance_lists_its_game_folder() {
        let json = r#"[{"id":"pack","name":"My Server","home":"C:\\games\\pack","icon":null}]"#;
        let instances: Vec<Instance> = serde_json::from_str(json).expect("parse");
        assert_eq!(
            instances[0].home.as_deref(),
            Some(Path::new("C:\\games\\pack"))
        );
    }
}
