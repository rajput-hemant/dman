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
fn serve_stop_without_daemon_and_bad_bind() {
    let dir = config_dir();
    dman(&dir)
        .args(["serve", "--stop"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not running"));
    dman(&dir)
        .args(["serve", "--bind", "not-an-ip"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("web.bind"));
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

#[test]
fn rename_and_preview_need_a_daemon_and_arguments() {
    let dir = config_dir();
    dman(&dir).args(["rename", "1"]).assert().code(2);
    dman(&dir).args(["rename", "1", "new.bin"]).assert().code(3);
    dman(&dir)
        .args(["preview", "magnet:?xt=urn:btih:abc", "--timeout", "5"])
        .assert()
        .code(3);
}

#[test]
fn search_commands_need_a_daemon_and_arguments() {
    let dir = config_dir();
    dman(&dir).args(["search"]).assert().code(2);
    dman(&dir).args(["search", "ubuntu"]).assert().code(3);
    dman(&dir).args(["search", "--history"]).assert().code(3);
    dman(&dir)
        .args(["search", "x", "--add", "0"])
        .assert()
        .code(2);
    dman(&dir).args(["providers", "list"]).assert().code(3);
    dman(&dir).args(["providers", "enable"]).assert().code(2);
    dman(&dir)
        .args(["providers", "import", "/nonexistent.toml"])
        .assert()
        .code(3);
    dman(&dir).args(["trackers", "status"]).assert().code(3);
}

#[test]
fn cleanup_and_config_reload_need_a_daemon_and_arguments() {
    let dir = config_dir();
    dman(&dir).args(["cleanup", "1", "--yes"]).assert().code(3);
    dman(&dir).args(["config", "reload"]).assert().code(3);
    dman(&dir).args(["cleanup"]).assert().code(2);
    dman(&dir).args(["config"]).assert().failure();
}

#[tokio::test]
async fn update_check_reads_latest_release_and_changes_nothing() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/rajput-hemant/dman/releases/latest"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "tag_name": "v99.0.0", "assets": [] })),
        )
        .mount(&server)
        .await;
    let uri = server.uri();
    let out = tokio::task::spawn_blocking(move || {
        let dir = config_dir();
        dman(&dir)
            .env("DMAN_UPDATE_API", uri)
            .args(["update", "--check", "--json"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["latest"], "99.0.0");
    assert_eq!(v["update_available"], true);
}
