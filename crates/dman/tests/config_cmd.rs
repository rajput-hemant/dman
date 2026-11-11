//! `dman config get|set|unset|edit|path|show` and the error hints, against a config
//! directory and no running daemon.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

const CONFIG: &str = "# my config\n\
    default_profile = \"local\"\n\
    \n\
    [limits]\n\
    download = \"1MiB\" # polite\n\
    \n\
    [profiles.local]\n\
    download_dir = \"/tmp\"\n\
    [profiles.work]\n\
    download_dir = \"/work\"\n\
    [profiles.work.daemon]\n\
    kind = \"remote\"\n\
    url = \"wss://nas.example:6810\"\n\
    secret_ref = \"work\"\n";

fn config_dir() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), CONFIG).unwrap();
    dir
}

fn dman(dir: &TempDir) -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("dman");
    cmd.env("DMAN_CONFIG_DIR", dir.path())
        .env_remove("DMAN_PROFILE")
        .env_remove("RUST_LOG")
        .env_remove("VISUAL")
        .env_remove("EDITOR")
        .write_stdin("");
    cmd
}

fn file(dir: &TempDir) -> String {
    std::fs::read_to_string(dir.path().join("config.toml")).unwrap()
}

fn stdout(cmd: &mut Command) -> String {
    let out = cmd.assert().success().get_output().stdout.clone();
    String::from_utf8(out).unwrap()
}

#[test]
fn path_prints_the_file() {
    let dir = config_dir();
    let out = stdout(dman(&dir).args(["config", "path"]));
    assert_eq!(out.trim(), dir.path().join("config.toml").to_str().unwrap());
    let json = stdout(dman(&dir).args(["--json", "config", "path"]));
    assert!(json.contains("\"exists\": true"), "{json}");
}

#[test]
fn get_reads_scalars_sections_and_defaults() {
    let dir = config_dir();
    assert_eq!(
        stdout(dman(&dir).args(["config", "get", "limits.download"])).trim(),
        "1MiB"
    );
    // Not in the file: the default.
    assert_eq!(
        stdout(dman(&dir).args(["config", "get", "search.dns"])).trim(),
        "system"
    );
    assert_eq!(
        stdout(dman(&dir).args(["config", "get", "limits.max_concurrent"])).trim(),
        "3"
    );
    let section = stdout(dman(&dir).args(["config", "get", "limits"]));
    assert!(section.contains("download = \"1MiB\""), "{section}");
    let json = stdout(dman(&dir).args(["--json", "config", "get", "limits.max_concurrent"]));
    assert_eq!(json.trim(), "3");
}

#[test]
fn unknown_keys_are_not_found_with_a_suggestion() {
    let dir = config_dir();
    dman(&dir)
        .args(["config", "get", "limits.dwonload"])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("unknown config key"))
        .stderr(predicate::str::contains("did you mean `limits.download`"));
    dman(&dir)
        .args(["config", "set", "limist.download", "1MiB"])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("did you mean `limits.download`"));
    dman(&dir)
        .args(["--json", "config", "get", "nope"])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("\"hint\""));
    assert_eq!(file(&dir), CONFIG, "a failed command changes nothing");
}

#[test]
fn set_keeps_comments_and_unset_restores_the_default() {
    let dir = config_dir();
    dman(&dir)
        .args(["config", "set", "limits.download", "5MiB"])
        .assert()
        .success()
        .stdout(predicate::str::contains("limits.download = 5MiB"))
        .stdout(predicate::str::contains("daemon is not running"));
    let text = file(&dir);
    assert!(text.contains("# my config"), "{text}");
    assert!(text.contains("download = \"5MiB\" # polite"), "{text}");
    dman(&dir)
        .args(["config", "set", "ui.mouse", "true", "--no-reload"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not reloaded"));
    assert_eq!(
        stdout(dman(&dir).args(["config", "get", "ui.mouse"])).trim(),
        "true"
    );
    dman(&dir)
        .args(["config", "unset", "limits.download", "--no-reload"])
        .assert()
        .success()
        .stdout(predicate::str::contains("unset limits.download"));
    assert!(!file(&dir).contains("5MiB"));
    dman(&dir)
        .args(["config", "unset", "limits.download"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not set"));
}

#[test]
fn a_bad_value_leaves_the_file_untouched() {
    let dir = config_dir();
    dman(&dir)
        .args(["config", "set", "limits.download", "lots"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("nothing was saved"));
    dman(&dir)
        .args(["config", "set", "limits.max_concurrent", "many"])
        .assert()
        .code(2);
    dman(&dir)
        .args(["config", "set", "limits.dwonload", "1MiB"])
        .assert()
        .code(2);
    assert_eq!(file(&dir), CONFIG);
}

#[test]
fn show_and_get_never_print_secrets() {
    let dir = config_dir();
    let mut text = file(&dir);
    text.push_str(
        "[search]\nproxy = \"http://bob:hunter2@proxy.example:3128\"\n\
         [notify.webhook]\nurl = \"https://hooks.example/T0/B0/SECRETPATH\"\nsecret_ref = \"hook\"\n",
    );
    std::fs::write(dir.path().join("config.toml"), text).unwrap();
    std::fs::write(
        dir.path().join("secrets.toml"),
        "[secrets]\nwork = \"tok-abc123\"\ndaemon = \"tok-def456\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            dir.path().join("secrets.toml"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    for args in [
        &["config", "show"][..],
        &["--json", "config", "show"],
        &["config", "get", "search"],
        &["config", "get", "search.proxy"],
        &["config", "get", "notify"],
    ] {
        let out = stdout(dman(&dir).args(args));
        for leaked in ["hunter2", "SECRETPATH", "tok-abc123", "tok-def456"] {
            assert!(!out.contains(leaked), "{args:?} leaked {leaked}:\n{out}");
        }
    }
    let shown = stdout(dman(&dir).args(["config", "show"]));
    assert!(shown.contains("proxy.example:3128"), "{shown}");
    assert!(shown.contains("secret_ref = \"hook\""), "{shown}");
    // Setting a credential does not echo it either.
    let set = stdout(dman(&dir).args([
        "config",
        "set",
        "search.proxy",
        "http://amy:s3cr3t@p2.example:1",
        "--no-reload",
    ]));
    assert!(!set.contains("s3cr3t"), "{set}");
}

#[cfg(unix)]
#[test]
fn edit_validates_then_replaces_the_file() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = config_dir();
    let tools = tempfile::tempdir().unwrap();
    let script = |name: &str, body: &str| {
        let p = tools.path().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    };
    let good = script("good", "sed -i 's/1MiB/9MiB/' \"$1\"");
    dman(&dir)
        .env("EDITOR", &good)
        .args(["config", "edit", "--no-reload"])
        .assert()
        .success()
        .stdout(predicate::str::contains("saved"));
    let text = file(&dir);
    assert!(
        text.contains("9MiB") && text.contains("# my config"),
        "{text}"
    );

    // Invalid result and no terminal to ask: discarded, file unchanged.
    let before = file(&dir);
    let bad = script("bad", "echo 'limits = [' >> \"$1\"");
    dman(&dir)
        .env("EDITOR", &bad)
        .args(["config", "edit"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("left unchanged"));
    assert_eq!(file(&dir), before);

    // A value that parses but is not a valid configuration is rejected too.
    let wrong = script("wrong", "sed -i 's/9MiB/lots/' \"$1\"");
    dman(&dir)
        .env("VISUAL", &wrong)
        .args(["config", "edit"])
        .assert()
        .code(2);
    assert_eq!(file(&dir), before);

    // No change, a failing editor and a missing editor.
    dman(&dir)
        .env("EDITOR", "true")
        .args(["config", "edit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no changes"));
    dman(&dir)
        .env("EDITOR", "false")
        .args(["config", "edit"])
        .assert()
        .code(1);
    dman(&dir)
        .env("EDITOR", "/nonexistent/editor")
        .args(["config", "edit"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("$EDITOR"));
    assert_eq!(file(&dir), before);
}

#[test]
fn config_keys_complete_without_a_daemon() {
    let dir = config_dir();
    let out = assert_cmd::cargo_bin_cmd!("dman")
        .env("DMAN_CONFIG_DIR", dir.path())
        .env("COMPLETE", "bash")
        .env("_CLAP_IFS", "\n")
        .env("_CLAP_COMPLETE_INDEX", "3")
        .args(["--", "dman", "config", "get", "limits.d"])
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.lines().any(|l| l == "limits.download"), "{text}");
    assert!(out.stderr.is_empty());
}

#[test]
fn config_help_has_examples() {
    let dir = config_dir();
    for sub in ["get", "set", "unset", "edit", "show"] {
        dman(&dir)
            .args(["config", sub, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("Examples:"));
    }
}

#[test]
fn every_daemon_error_ends_with_a_next_step() {
    let dir = config_dir();
    dman(&dir)
        .arg("list")
        .assert()
        .code(3)
        .stderr(predicate::str::contains("hint: run `dman daemon start`"));
    // A remote profile points at its URL instead.
    dman(&dir)
        .args(["--profile", "work", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("hint:"));
    dman(&dir)
        .args(["--profile", "wrok", "list"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("did you mean `work`"));
    dman(&dir)
        .args(["--json", "list"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("\"hint\""));
    dman(&dir)
        .args(["remove", "1"])
        .assert()
        .code(6)
        .stderr(predicate::str::contains("hint: rerun with --yes"));
}
