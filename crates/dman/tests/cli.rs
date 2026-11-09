//! End-to-end tests of the `dman` binary against a daemon that is not running.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// A config dir whose local profile listens on a port nobody uses.
fn config_dir() -> TempDir {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let dir = tempfile::tempdir().unwrap();
    let cfg = format!(
        "default_profile = \"local\"\n\
         [profiles.local]\n\
         download_dir = \"/tmp\"\n\
         [profiles.local.daemon]\n\
         kind = \"local\"\n\
         listen = \"127.0.0.1:{port}\"\n\
         secret_ref = \"daemon\"\n"
    );
    std::fs::write(dir.path().join("config.toml"), cfg).unwrap();
    dir
}

fn dman(dir: &TempDir) -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("dman");
    cmd.env("DMAN_CONFIG_DIR", dir.path())
        .env_remove("DMAN_PROFILE")
        .env_remove("RUST_LOG")
        .write_stdin("");
    cmd
}

#[test]
fn help_works() {
    let dir = config_dir();
    dman(&dir)
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("download"));
}

#[test]
fn completions_mention_dman() {
    let dir = config_dir();
    dman(&dir)
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dman"));
}

#[test]
fn unknown_flag_is_usage_error() {
    let dir = config_dir();
    dman(&dir).arg("--bogus").assert().code(2);
}

#[test]
fn list_without_daemon_exits_3() {
    let dir = config_dir();
    dman(&dir)
        .arg("list")
        .assert()
        .code(3)
        .stderr(predicate::str::contains("not running"));
}

#[test]
fn json_errors_go_to_stderr_as_json() {
    let dir = config_dir();
    dman(&dir)
        .args(["--json", "list"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("\"error\""));
}

#[test]
fn remove_without_yes_is_refused_before_connecting() {
    let dir = config_dir();
    dman(&dir)
        .args(["remove", "1"])
        .assert()
        .code(6)
        .stderr(predicate::str::contains("without --yes"));
}

#[test]
fn daemon_status_exits_3() {
    let dir = config_dir();
    dman(&dir).args(["daemon", "status"]).assert().code(3);
}

#[test]
fn placeholders_exit_2() {
    let dir = config_dir();
    dman(&dir)
        .arg("serve")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("not available yet"));
}

#[test]
fn gen_config_reports_existing() {
    let dir = config_dir();
    dman(&dir)
        .args(["daemon", "gen-config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already exists"));
}
