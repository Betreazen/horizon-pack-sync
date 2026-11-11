//! The one-time install on a player's computer, with the player's agreement.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ed25519_dalek::VerifyingKey;

use crate::client::{ClientState, Sync};
use crate::launcher::RealLauncher;
use crate::site::{HttpSite, Site};

/// Baked in from pack-sync.toml by build.rs.
const DEFAULT_SITE: &str = env!("PACK_SITE");
const PROFILE_NAME: &str = env!("PACK_PROFILE");
const LANGUAGE: &str = env!("PACK_LANGUAGE");
const INSTALL_ROOT: &str = "PackSync";
const PROGRAM_NAME: &str = "pack-sync.exe";
const STATE_FILE: &str = "client.json";
const BUILDS_FOLDER: &str = "builds";
/// Characters Windows refuses in a folder name.
const FORBIDDEN_IN_FOLDER: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// What the installer says, in the pack's language.
struct Texts {
    agreement: &'static str,
    confirm: &'static str,
    cancelled: &'static str,
    code: &'static str,
    checking: &'static str,
    downloading: &'static str,
    done: &'static str,
}

const ENGLISH: Texts = Texts {
    agreement: "\
pack-sync installs this server's mod pack and keeps it up to date by itself every time the game starts.

Horizon mods are ordinary programs with full access to your computer; there is no sandbox.
Updates install without asking again, and each one is checked against the server owner's signature.
Continue only if you trust the server's owner.",
    confirm: "Install? (yes/no): ",
    cancelled: "installation cancelled",
    code: "The code from your profile on the site: ",
    checking: "Checking the build...",
    downloading: "Downloading build {sequence}, this takes a few minutes...",
    done: "Done. In the Horizon launcher, pick the profile \"{profile}\" and press Play.",
};

const RUSSIAN: Texts = Texts {
    agreement: "\
pack-sync поставит сборку модов этого сервера и будет обновлять её сам при каждом запуске игры.

Моды Horizon это обычные программы с полным доступом к вашему компьютеру, песочницы у них нет.
Обновления ставятся без отдельного вопроса, и каждое проверяется по подписи владельца сервера.
Продолжайте, только если доверяете владельцу сервера.",
    confirm: "Установить? (да/нет): ",
    cancelled: "установка отменена",
    code: "Код из профиля на сайте: ",
    checking: "Проверяю сборку...",
    downloading: "Скачиваю сборку {sequence}, это займёт несколько минут...",
    done: "Готово. В лаунчере Horizon выберите профиль «{profile}» и нажмите «Играть».",
};

fn texts() -> &'static Texts {
    if LANGUAGE == "ru" { &RUSSIAN } else { &ENGLISH }
}

fn ask(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout()
        .flush()
        .context("cannot write to the console")?;
    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .context("cannot read the answer")?;
    Ok(line.trim().to_owned())
}

fn is_yes(answer: &str) -> bool {
    matches!(answer.to_lowercase().as_str(), "да" | "д" | "yes" | "y")
}

/// One folder per pack, so a player on two servers keeps two separate installs.
fn folder_name(profile: &str) -> String {
    let name: String = profile
        .chars()
        .map(|c| {
            if FORBIDDEN_IN_FOLDER.contains(&c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim().trim_end_matches('.').trim().to_owned();
    if name.is_empty() {
        "pack".to_owned()
    } else {
        name
    }
}

fn install_home() -> Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(local)
        .join(INSTALL_ROOT)
        .join(folder_name(PROFILE_NAME)))
}

/// Copies this program to its permanent place; the download may be anywhere.
fn copy_self(home: &Path) -> Result<PathBuf> {
    fs::create_dir_all(home).context("cannot create the install folder")?;
    let target = home.join(PROGRAM_NAME);
    let current = std::env::current_exe().context("cannot find this program")?;
    if current != target {
        fs::copy(&current, &target).context("cannot copy pack-sync to its folder")?;
    }
    Ok(target)
}

fn pre_launch_command(program: &Path) -> Result<String> {
    let path = program
        .to_str()
        .context("the install path is not valid text")?;
    Ok(format!("\"{path}\" client"))
}

fn ensure_instance(launcher: &RealLauncher) -> Result<String> {
    let find = |launcher: &RealLauncher| -> Result<Option<String>> {
        Ok(launcher
            .instances()?
            .into_iter()
            .find(|instance| instance.name == PROFILE_NAME)
            .map(|instance| instance.id))
    };
    if let Some(id) = find(launcher)? {
        return Ok(id);
    }
    launcher.create_instance(PROFILE_NAME)?;
    find(launcher)?.context("the launcher did not create the profile")
}

pub fn run(code: Option<String>, site: Option<String>, key: &VerifyingKey) -> Result<()> {
    let text = texts();
    println!("{}\n", text.agreement);
    if !is_yes(&ask(text.confirm)?) {
        bail!("{}", text.cancelled);
    }
    let code = match code {
        Some(code) => code,
        None => ask(text.code)?,
    };
    let base = site.as_deref().unwrap_or(DEFAULT_SITE);
    let launcher_path = RealLauncher::default_path()?;
    let launcher = RealLauncher::new(launcher_path.clone());
    let home = install_home()?;
    let program = copy_self(&home)?;

    let http = HttpSite::new(base, &code)?;
    println!("{}", text.checking);
    let manifest = http.manifest()?.open(key)?;
    let instance = ensure_instance(&launcher)?;
    let sequence = manifest.sequence.to_string();
    println!("{}", text.downloading.replace("{sequence}", &sequence));
    let builds = home.join(BUILDS_FOLDER);
    let sync = Sync {
        site: &http,
        launcher: &launcher,
        key,
        builds_dir: &builds,
        staged: None,
    };
    let installed = sync.install_fresh(&manifest, &instance)?;
    launcher.set_pre_launch(&instance, &pre_launch_command(&program)?)?;

    crate::files::write_json(
        &home.join(STATE_FILE),
        &ClientState {
            site: base.to_owned(),
            code,
            instance,
            launcher: launcher_path,
            program: installed.program,
            version_tag: installed.tag,
            horizon_base: manifest.horizon_base.clone(),
            sequence: manifest.sequence,
            build: Some(installed.build.program),
            build_sha256: Some(installed.build.sha256),
        },
    )?;
    println!("{}", text.done.replace("{profile}", PROFILE_NAME));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_explicit_yes_counts_as_agreement() {
        for answer in ["да", "Да", "y", "YES"] {
            assert!(is_yes(answer), "{answer}");
        }
        for answer in ["", "нет", "n", "ладно"] {
            assert!(!is_yes(answer), "{answer}");
        }
    }

    #[test]
    fn the_pre_launch_command_quotes_the_path() {
        let command = pre_launch_command(Path::new(r"C:\Users\Имя Фамилия\pack-sync.exe"));
        assert_eq!(
            command.expect("command"),
            r#""C:\Users\Имя Фамилия\pack-sync.exe" client"#
        );
    }

    #[test]
    fn each_profile_gets_a_folder_windows_accepts() {
        assert_eq!(folder_name("Сервер"), "Сервер");
        assert_eq!(folder_name("My: Server?"), "My_ Server_");
        assert_eq!(folder_name("  ..  "), "pack");
        assert_eq!(folder_name("a/b\\c"), "a_b_c");
    }

    #[test]
    fn both_languages_have_every_placeholder() {
        for text in [&ENGLISH, &RUSSIAN] {
            assert!(text.downloading.contains("{sequence}"));
            assert!(text.done.contains("{profile}"));
        }
    }
}
