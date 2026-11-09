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

// ---- completions, help, broken pipe --------------------------------------------------

/// Run the shell completion protocol of bash for `words` (the last one is being completed).
fn complete(dir: &TempDir, words: &[&str]) -> Vec<String> {
    let out = assert_cmd::cargo_bin_cmd!("dman")
        .env("DMAN_CONFIG_DIR", dir.path())
        .env_remove("DMAN_PROFILE")
        .env("COMPLETE", "bash")
        .env("_CLAP_IFS", "\n")
        .env("_CLAP_COMPLETE_INDEX", (words.len() - 1).to_string())
        .arg("--")
        .args(words)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(out.stderr.is_empty(), "completion must be quiet: {out:?}");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn static_values_complete() {
    let dir = config_dir();
    let q = complete(&dir, &["dman", "add", "--quality", ""]);
    assert!(
        q.contains(&"1080p".to_owned()) && q.contains(&"audio:mp3".to_owned()),
        "{q:?}"
    );
    let c = complete(&dir, &["dman", "add", "--container", ""]);
    assert_eq!(c, ["mp4", "mkv", "webm"]);
    let s = complete(&dir, &["dman", "list", "--state", ""]);
    assert!(s.contains(&"seeding".to_owned()), "{s:?}");
    let sh = complete(&dir, &["dman", "completions", ""]);
    assert!(
        sh.contains(&"zsh".to_owned()) && sh.contains(&"install".to_owned()),
        "{sh:?}"
    );
}

#[test]
fn profiles_complete_without_a_daemon() {
    let dir = config_dir();
    assert_eq!(complete(&dir, &["dman", "--profile", ""]), ["local"]);
}

#[test]
fn daemon_values_are_empty_when_the_daemon_is_unreachable() {
    let dir = config_dir();
    for words in [
        &["dman", "pause", ""][..],
        &["dman", "get", ""],
        &["dman", "providers", "enable", ""],
        &["dman", "search", "x", "--provider", ""],
    ] {
        let got = complete(&dir, words);
        assert!(
            got.iter().all(|c| c.starts_with('-')),
            "{words:?} -> {got:?}"
        );
    }
    assert!(
        !dir.path().join("secrets.toml").exists(),
        "completion must not write files"
    );
}

fn install_cmd(home: &TempDir, shell: &str, args: &[&str]) -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("dman");
    cmd.env("HOME", home.path())
        .env("SHELL", shell)
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("ZDOTDIR")
        .env_remove("DMAN_PROFILE")
        .args(["completions", "install"])
        .args(args);
    cmd
}

#[test]
fn install_writes_file_and_refuses_to_overwrite_without_force() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join(".config/fish/completions/dman.fish");
    install_cmd(&home, "/usr/bin/fish", &[])
        .assert()
        .success()
        .stdout(predicate::str::contains("dman.fish"))
        .stdout(predicate::str::contains("nothing to add"));
    assert!(std::fs::read_to_string(&file).unwrap().contains("COMPLETE"));
    install_cmd(&home, "/usr/bin/fish", &[])
        .assert()
        .success()
        .stdout(predicate::str::contains("already up to date"));
    std::fs::write(&file, "mine").unwrap();
    install_cmd(&home, "/usr/bin/fish", &[])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--force"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
    install_cmd(&home, "/usr/bin/fish", &["--force"])
        .assert()
        .success();
    assert_ne!(std::fs::read_to_string(&file).unwrap(), "mine");
}

#[test]
fn install_zsh_prints_rc_lines_and_honours_shell_flag() {
    let home = tempfile::tempdir().unwrap();
    install_cmd(&home, "/bin/sh", &["--shell", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fpath=(~/.zfunc $fpath)"))
        .stdout(predicate::str::contains("compinit"));
    assert!(home.path().join(".zfunc/_dman").is_file());
    install_cmd(&home, "/bin/tcsh", &[]).assert().code(2);
}

#[test]
fn help_has_examples() {
    let dir = config_dir();
    for (args, needle) in [
        (&["--help"][..], "dman completions install"),
        (&["add", "--help"], "dman add https://example.org/big.iso"),
        (&["search", "--help"], "dman search ubuntu"),
        (&["list", "--help"], "dman list --state"),
        (&["serve", "--help"], "dman serve --base-path"),
        (&["daemon", "start", "--help"], "dman daemon status"),
    ] {
        dman(&dir)
            .args(args)
            .assert()
            .success()
            .stdout(predicate::str::contains("Examples:"))
            .stdout(predicate::str::contains(needle));
    }
}

#[test]
fn typos_suggest_a_close_match() {
    let dir = config_dir();
    dman(&dir).arg("lisst").assert().code(2).stderr(
        predicate::str::contains("similar subcommand").and(predicate::str::contains("list")),
    );
}

#[test]
fn broken_pipe_is_a_normal_exit() {
    let dir = config_dir();
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("dman"))
        .env("DMAN_CONFIG_DIR", dir.path())
        .args(["completions", "bash"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Close our read end at once: the child's writes fail with EPIPE.
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{:?}", out.status);
    assert!(!String::from_utf8_lossy(&out.stderr).contains("panicked"));
}
