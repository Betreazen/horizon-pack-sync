//! The `pack-sync-hub` program itself.

use std::process::Command;

fn hub() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pack-sync-hub"));
    for name in ["PACK_GITHUB_REPO", "PACK_GITHUB_TOKEN", "PACK_SERVER_KEY"] {
        command.env_remove(name);
    }
    command
}

#[test]
fn a_code_is_printed_once_and_listed_by_name() {
    let dir = tempfile::tempdir().expect("dir");
    let data = dir.path().to_str().expect("path");
    let added = hub()
        .args(["--data", data, "code", "add", "alex"])
        .output()
        .expect("run");
    assert!(added.status.success());
    let code = String::from_utf8(added.stdout).expect("utf8");
    assert_eq!(code.trim().len(), 32);

    let listed = hub()
        .args(["--data", data, "code", "list"])
        .output()
        .expect("run");
    let list = String::from_utf8(listed.stdout).expect("utf8");
    assert!(list.starts_with("alex\t"), "{list}");
    assert!(!list.contains(code.trim()));
}

#[test]
fn revoking_an_unknown_player_fails() {
    let dir = tempfile::tempdir().expect("dir");
    let data = dir.path().to_str().expect("path");
    let revoked = hub()
        .args(["--data", data, "code", "revoke", "nobody"])
        .output()
        .expect("run");
    assert!(!revoked.status.success());
}

#[test]
fn serving_without_the_secrets_says_what_is_missing() {
    let dir = tempfile::tempdir().expect("dir");
    let data = dir.path().to_str().expect("path");
    let served = hub()
        .args(["--data", data, "serve", "--bind", "127.0.0.1:0"])
        .output()
        .expect("run");
    assert!(!served.status.success());
    let error = String::from_utf8(served.stderr).expect("utf8");
    assert!(error.contains("PACK_GITHUB_REPO"), "{error}");
}
