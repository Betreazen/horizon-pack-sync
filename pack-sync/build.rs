//! Bakes the pack's settings into the program, so every pack owner builds their own pack-sync and
//! no player can point it at another pack.
use std::env;
use std::fs;
use std::path::PathBuf;

const CONFIG_VARIABLE: &str = "PACK_SYNC_CONFIG";
const LANGUAGES: [&str; 2] = ["en", "ru"];
const KEY_HEX_LENGTH: usize = 64;

fn config_path() -> PathBuf {
    if let Some(path) = env::var_os(CONFIG_VARIABLE) {
        return PathBuf::from(path);
    }
    let crate_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets this"));
    let own = crate_dir.join("pack-sync.toml");
    if own.exists() {
        own
    } else {
        crate_dir.join("pack-sync.example.toml")
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed={CONFIG_VARIABLE}");
    let path = config_path();
    println!("cargo:rerun-if-changed={}", path.display());
    let shown = path.display();
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {shown}: {e}"));
    let config: toml::Table = text
        .parse()
        .unwrap_or_else(|e| panic!("{shown} is not TOML: {e}"));
    let value = |key: &str| -> String {
        config
            .get(key)
            .and_then(toml::Value::as_str)
            .unwrap_or_else(|| panic!("{shown} has no text value `{key}`"))
            .trim()
            .to_owned()
    };
    let site = value("site");
    let profile = value("profile");
    let language = value("language");
    let public_key = value("public_key");
    assert!(
        site.starts_with("https://"),
        "{shown}: `site` must start with https://"
    );
    assert!(!profile.is_empty(), "{shown}: `profile` is empty");
    assert!(
        LANGUAGES.contains(&language.as_str()),
        "{shown}: `language` must be en or ru"
    );
    let is_key = public_key.len() == KEY_HEX_LENGTH
        && public_key.bytes().all(|byte| byte.is_ascii_hexdigit());
    assert!(
        public_key.is_empty() || is_key,
        "{shown}: `public_key` must be 64 hex digits"
    );
    for (name, value) in [
        ("PACK_SITE", site),
        ("PACK_PROFILE", profile),
        ("PACK_LANGUAGE", language),
        ("PACK_PUBLIC_KEY", public_key),
    ] {
        println!("cargo:rustc-env={name}={value}");
    }
}
