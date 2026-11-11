//! `dman update` and the opt-in update check.
//!
//! dman only replaces its own binary when it was put there by the dman
//! installer (cargo-dist shell script, which leaves an install receipt).
//! Homebrew, cargo and system package installs are left to their managers.
//! Nothing here runs unless asked: `dman update`, or `[update] check = true`
//! (at most one request per day, notice on stderr).

use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::time::Duration;

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::print_json;
use crate::cli::UpdateArgs;
use crate::connect::Ctx;
use crate::error::CliError;

/// GitHub repository releases are published to.
const REPO: &str = "rajput-hemant/dman";
const DEFAULT_API: &str = "https://api.github.com";
/// Overrides the GitHub API base URL (tests, mirrors).
const ENV_API: &str = "DMAN_UPDATE_API";
const INSTALLER_URL: &str =
    "https://github.com/rajput-hemant/dman/releases/latest/download/dman-installer.sh";
const GIT_URL: &str = "https://github.com/rajput-hemant/dman";
const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
const CACHE_FILE: &str = "update-check.json";
const NOTICE_TIMEOUT: Duration = Duration::from_secs(3);
const UPDATE_TIMEOUT: Duration = Duration::from_secs(60);

/// How this binary got onto the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallMethod {
    Homebrew,
    Cargo,
    Installer,
    System,
    Unknown,
}

impl InstallMethod {
    fn label(self) -> &'static str {
        match self {
            Self::Homebrew => "Homebrew",
            Self::Cargo => "cargo",
            Self::Installer => "the dman installer",
            Self::System => "a system package manager",
            Self::Unknown => "an unknown method",
        }
    }

    /// The command that upgrades dman for this install method.
    pub fn upgrade_command(self) -> String {
        match self {
            Self::Homebrew => "brew upgrade dman".into(),
            Self::Cargo => format!("cargo install --locked --git {GIT_URL} dman"),
            Self::Installer => "dman update".into(),
            Self::System => "update dman with your system package manager".into(),
            Self::Unknown => format!("curl --proto '=https' --tlsv1.2 -LsSf {INSTALLER_URL} | sh"),
        }
    }
}

/// Environment inputs of [`detect`], so detection is a pure function.
#[derive(Debug, Clone, Default)]
pub struct DetectEnv {
    pub home: Option<PathBuf>,
    pub cargo_home: Option<PathBuf>,
    pub homebrew_prefix: Option<PathBuf>,
}

impl DetectEnv {
    pub fn from_process() -> Self {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Self {
            home: var("HOME"),
            cargo_home: var("CARGO_HOME"),
            homebrew_prefix: var("HOMEBREW_PREFIX"),
        }
    }
}

/// The parts of cargo-dist's install receipt (`dman-receipt.json`) we use.
#[derive(Debug, Clone, Deserialize)]
pub struct Receipt {
    pub install_prefix: PathBuf,
}

impl Receipt {
    /// Where the installer writes the receipt: `$XDG_CONFIG_HOME` or `~/.config`.
    /// This is independent of `DMAN_CONFIG_DIR`, it is the installer's choice.
    pub fn path(home: Option<&Path>, xdg_config_home: Option<&Path>) -> Option<PathBuf> {
        let base = match xdg_config_home {
            Some(x) => x.to_path_buf(),
            None => home?.join(".config"),
        };
        Some(base.join("dman").join("dman-receipt.json"))
    }

    pub fn load(path: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    /// Whether `exe` lives in the directory this receipt installed into.
    fn owns(&self, exe: &Path) -> bool {
        let Some(dir) = exe.parent() else {
            return false;
        };
        dir == self.install_prefix || dir == self.install_prefix.join("bin")
    }
}

/// Classify an installed binary. `exe` should be canonicalised by the caller.
///
/// Order: Homebrew, installer receipt that owns this exact location, cargo,
/// system package paths, unknown. The receipt check precedes cargo only so an
/// installer that installs into `~/.cargo/bin` is not mistaken for `cargo install`.
pub fn detect(exe: &Path, env: &DetectEnv, receipt: Option<&Receipt>) -> InstallMethod {
    let mut brew: Vec<PathBuf> = vec![
        "/opt/homebrew".into(),
        "/usr/local/Cellar".into(),
        "/home/linuxbrew/.linuxbrew".into(),
    ];
    if let Some(p) = &env.homebrew_prefix {
        brew.push(p.join("Cellar"));
        brew.push(p.join("opt"));
        if p != Path::new("/usr/local") {
            brew.push(p.clone());
        }
    }
    if brew.iter().any(|p| exe.starts_with(p)) {
        return InstallMethod::Homebrew;
    }
    if receipt.is_some_and(|r| r.owns(exe)) {
        return InstallMethod::Installer;
    }
    let cargo_bin = env
        .cargo_home
        .clone()
        .or_else(|| env.home.as_ref().map(|h| h.join(".cargo")))
        .map(|c| c.join("bin"));
    if cargo_bin.is_some_and(|b| exe.starts_with(b)) {
        return InstallMethod::Cargo;
    }
    if exe.starts_with("/usr/bin") || exe.starts_with("/usr/sbin") {
        return InstallMethod::System;
    }
    InstallMethod::Unknown
}

fn detect_current() -> (InstallMethod, Option<PathBuf>) {
    let Ok(exe) = std::env::current_exe() else {
        return (InstallMethod::Unknown, None);
    };
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let env = DetectEnv::from_process();
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let receipt =
        Receipt::path(env.home.as_deref(), xdg.as_deref()).and_then(|p| Receipt::load(&p));
    (detect(&exe, &env, receipt.as_ref()), Some(exe))
}

/// The Rust target triple of the release archive for this build.
pub fn target_triple() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        _ => return None,
    })
}

/// Parse a release tag (`v1.2.3` or `1.2.3`).
pub fn parse_version(tag: &str) -> Option<Version> {
    Version::parse(tag.trim().trim_start_matches('v')).ok()
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

fn api_base() -> String {
    std::env::var(ENV_API)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_API.into())
}

fn http_client(timeout: Duration) -> Result<reqwest::Client, CliError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .user_agent(concat!("dman/", env!("CARGO_PKG_VERSION")))
        .timeout(timeout)
        .build()
        .map_err(|e| CliError::general(format!("cannot create HTTP client: {e}")))
}

async fn get_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, CliError> {
    let resp = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| download_failed(url, &e))?;
    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| download_failed(url, &e))
}

fn download_failed(url: &str, e: &reqwest::Error) -> CliError {
    CliError::general(format!("download of {url} failed: {e}"))
        .with_hint("check your network connection and retry `dman update`")
}

async fn fetch_release(client: &reqwest::Client, api: &str) -> Result<Release, CliError> {
    let url = format!("{}/repos/{REPO}/releases/latest", api.trim_end_matches('/'));
    let body = get_bytes(client, &url).await?;
    serde_json::from_slice(&body)
        .map_err(|e| CliError::general(format!("unexpected release response: {e}")))
}

/// Lower-case hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(data)
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Check `data` against a checksum file (`<hex>  <name>` or just `<hex>`).
pub fn verify_checksum(data: &[u8], checksum_file: &str, asset_name: &str) -> Result<(), String> {
    let expected = checksum_file
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let hash = it.next()?;
            match it.next().map(|n| n.trim_start_matches('*')) {
                Some(n) if n != asset_name => None,
                _ => Some(hash),
            }
        })
        .next()
        .ok_or_else(|| format!("no checksum for {asset_name} in the checksum file"))?;
    let actual = sha256_hex(data);
    if expected.eq_ignore_ascii_case(&actual) {
        Ok(())
    } else {
        Err(format!(
            "checksum mismatch for {asset_name}: expected {expected}, got {actual}"
        ))
    }
}

/// Replace `exe` with `new_binary`: copy next to it, move the old one to
/// `<exe>.old`, rename the new one in; the backup is removed on success and
/// restored on failure.
pub fn replace_binary(exe: &Path, new_binary: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let with_ext = |ext: &str| {
        let mut name = exe.file_name().unwrap_or_default().to_os_string();
        name.push(ext);
        exe.with_file_name(name)
    };
    let staged = with_ext(".new");
    let backup = with_ext(".old");
    let result = (|| {
        std::fs::copy(new_binary, &staged)?;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
        let _ = std::fs::remove_file(&backup);
        std::fs::rename(exe, &backup)?;
        if let Err(e) = std::fs::rename(&staged, exe) {
            let _ = std::fs::rename(&backup, exe);
            return Err(e);
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            let _ = std::fs::remove_file(&backup);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&staged);
            Err(e)
        }
    }
}

/// Unpack a `.tar.gz` with the system `tar` and return the `dman` binary in it.
fn extract_binary(archive: &Path, dest: &Path) -> Result<PathBuf, CliError> {
    let status = Process::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .status()
        .map_err(|e| CliError::general(format!("cannot run tar: {e}")))?;
    if !status.success() {
        return Err(
            CliError::general("tar could not unpack the release archive")
                .with_hint("retry `dman update`, or reinstall from the releases page"),
        );
    }
    let mut dirs = vec![dest.to_path_buf()];
    for _ in 0..2 {
        let mut next = Vec::new();
        for dir in &dirs {
            for entry in std::fs::read_dir(dir)?.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    next.push(path);
                } else if path.file_name().is_some_and(|n| n == "dman") {
                    return Ok(path);
                }
            }
        }
        dirs = next;
    }
    Err(
        CliError::general("the release archive does not contain a dman binary")
            .with_hint("reinstall from the releases page"),
    )
}

/// Download, verify and install `release` over `exe`.
async fn install_release(
    client: &reqwest::Client,
    release: &Release,
    target: &str,
    exe: &Path,
) -> Result<(), CliError> {
    let name = format!("dman-{target}.tar.gz");
    let find = |n: &str| release.assets.iter().find(|a| a.name == n);
    let archive = find(&name).ok_or_else(|| {
        CliError::general(format!("release {} has no asset {name}", release.tag_name))
            .with_hint("build from source instead: `cargo install dman`")
    })?;
    let sum_name = format!("{name}.sha256");
    let sum = find(&sum_name).ok_or_else(|| {
        CliError::general(format!(
            "release {} has no checksum {sum_name}",
            release.tag_name
        ))
    })?;
    let data = get_bytes(client, &archive.browser_download_url).await?;
    let sum_text = String::from_utf8(get_bytes(client, &sum.browser_download_url).await?)
        .map_err(|_| CliError::general("checksum file is not text"))?;
    verify_checksum(&data, &sum_text, &name).map_err(|e| {
        CliError::general(e).with_hint("nothing was installed; retry `dman update`")
    })?;

    let tmp = tempfile::tempdir_in(exe.parent().unwrap_or(Path::new(".")))
        .or_else(|_| tempfile::tempdir())
        .map_err(|e| CliError::general(format!("cannot create a temporary directory: {e}")))?;
    let archive_path = tmp.path().join(&name);
    std::fs::write(&archive_path, &data)?;
    let unpack = tmp.path().join("unpack");
    std::fs::create_dir(&unpack)?;
    let new_bin = extract_binary(&archive_path, &unpack)?;
    replace_binary(exe, &new_bin).map_err(|e| {
        CliError::general(format!(
            "cannot replace {} (permissions?): {e}",
            exe.display()
        ))
        .with_hint("run `dman update` as a user who may write to that directory")
    })
}

fn confirm(question: &str) -> bool {
    use std::io::{BufRead, Write};
    if !super::can_prompt() {
        return false;
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).is_ok()
        && matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn run_command(program: &Path, args: &[&str]) -> Result<bool, CliError> {
    Process::new(program)
        .args(args)
        .status()
        .map(|s| s.success())
        .map_err(|e| {
            CliError::general(format!("cannot run {}: {e}", program.display()))
                .with_hint("make sure it is installed and on your PATH")
        })
}

/// `dman update [--check] [--yes]`.
pub async fn run(ctx: &Ctx, args: &UpdateArgs) -> Result<i32, CliError> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| CliError::general(format!("own version is not semver: {e}")))?;
    let (method, exe) = detect_current();
    let client = http_client(UPDATE_TIMEOUT)?;
    let release = fetch_release(&client, &api_base()).await?;
    let latest = parse_version(&release.tag_name).ok_or_else(|| {
        CliError::general(format!("unexpected release tag `{}`", release.tag_name))
    })?;
    let newer = latest > current;
    let command = method.upgrade_command();

    if args.check || !newer {
        if ctx.json {
            print_json(&serde_json::json!({
                "current": current.to_string(),
                "latest": latest.to_string(),
                "update_available": newer,
                "install_method": method,
                "command": newer.then_some(&command),
            }))?;
        } else if newer {
            println!(
                "dman {current} (installed via {}); latest is {latest}",
                method.label()
            );
            println!("update: {command}");
        } else {
            println!("dman {current} is up to date (latest {latest})");
        }
        return Ok(0);
    }

    // With --json the only stdout output is the final result object.
    let say = |text: String| {
        if !ctx.json {
            println!("{text}");
        }
    };
    let done = |updated: bool| -> Result<i32, CliError> {
        if ctx.json {
            print_json(&serde_json::json!({
                "current": current.to_string(),
                "latest": latest.to_string(),
                "update_available": true,
                "updated": updated,
                "install_method": method,
                "command": command,
            }))?;
        }
        Ok(0)
    };
    say(format!(
        "dman {current} -> {latest} (installed via {})",
        method.label()
    ));
    match method {
        InstallMethod::Homebrew => {
            if args.yes || confirm("run `brew upgrade dman` now?") {
                let ok = run_command(Path::new("brew"), &["upgrade", "dman"])?;
                if !ok {
                    return Err(CliError::general("`brew upgrade dman` failed")
                        .with_hint("run `brew update` and try `brew upgrade dman` yourself"));
                }
                return done(true);
            }
            say(format!("run: {command}"));
            done(false)
        }
        InstallMethod::Cargo => {
            say(format!("run: {command}"));
            done(false)
        }
        InstallMethod::System | InstallMethod::Unknown => Err(CliError::refused(format!(
            "dman was installed via {}, so it will not replace itself",
            method.label()
        ))
        .with_hint(format!("run: {command}"))),
        InstallMethod::Installer => {
            let (Some(target), Some(exe)) = (target_triple(), exe) else {
                return Err(CliError::refused("no release build for this platform")
                    .with_hint("build from source: `cargo install dman`"));
            };
            install_release(&client, &release, target, &exe).await?;
            say(format!("updated to {latest}"));
            if args.yes || confirm("restart the daemon now (`dman daemon restart`)?") {
                if !run_command(&exe, &["daemon", "restart"])? {
                    say("daemon restart failed; run `dman daemon restart` yourself".into());
                }
            } else {
                say("restart the daemon to use the new version: dman daemon restart".into());
            }
            done(true)
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    checked_at: u64,
    latest: Option<String>,
}

/// The opt-in check. With `enabled == false` this returns at once and makes no
/// request. Otherwise the answer comes from the cache unless it is over a day
/// old, in which case one request is made (failures are silent). Returns the
/// newer version when there is one.
pub async fn check_for_update(
    enabled: bool,
    state_dir: &Path,
    api: &str,
    current: &Version,
    now: u64,
) -> Option<Version> {
    if !enabled {
        return None;
    }
    let cache_path = state_dir.join(CACHE_FILE);
    let mut cache: Cache = std::fs::read(&cache_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if now.saturating_sub(cache.checked_at) >= CHECK_INTERVAL_SECS {
        let fetched = match http_client(NOTICE_TIMEOUT) {
            Ok(client) => fetch_release(&client, api).await.ok(),
            Err(_) => None,
        };
        // A failed attempt also counts, so a flaky network is not retried on every run.
        cache.checked_at = now;
        if let Some(r) = fetched {
            cache.latest = Some(r.tag_name);
        }
        let _ = std::fs::create_dir_all(state_dir);
        if let Ok(json) = serde_json::to_vec(&cache) {
            let _ = std::fs::write(&cache_path, json);
        }
    }
    let latest = parse_version(cache.latest.as_deref()?)?;
    (latest > *current).then_some(latest)
}

/// Print the one-line update notice to stderr, if enabled and one is known.
pub async fn notice(ctx: &Ctx) {
    use std::io::IsTerminal;
    if ctx.json || !std::io::stderr().is_terminal() {
        return;
    }
    let Ok(current) = Version::parse(env!("CARGO_PKG_VERSION")) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let found = check_for_update(
        ctx.config.update.check,
        ctx.paths.state_dir(),
        &api_base(),
        &current,
        now,
    )
    .await;
    if let Some(latest) = found {
        let (method, _) = detect_current();
        eprintln!(
            "dman {latest} is available (you have {current}): {}",
            method.upgrade_command()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn env() -> DetectEnv {
        DetectEnv {
            home: Some("/home/u".into()),
            cargo_home: None,
            homebrew_prefix: None,
        }
    }

    #[test]
    fn detects_homebrew() {
        for p in [
            "/opt/homebrew/Cellar/dman/1.0/bin/dman",
            "/usr/local/Cellar/dman/1.0/bin/dman",
            "/home/linuxbrew/.linuxbrew/Cellar/dman/1.0/bin/dman",
        ] {
            assert_eq!(
                detect(Path::new(p), &env(), None),
                InstallMethod::Homebrew,
                "{p}"
            );
        }
        let mut e = env();
        e.homebrew_prefix = Some("/custom/brew".into());
        assert_eq!(
            detect(Path::new("/custom/brew/bin/dman"), &e, None),
            InstallMethod::Homebrew
        );
        // a plain /usr/local/bin install is not Homebrew even if it is the prefix
        e.homebrew_prefix = Some("/usr/local".into());
        assert_eq!(
            detect(Path::new("/usr/local/bin/dman"), &e, None),
            InstallMethod::Unknown
        );
    }

    #[test]
    fn detects_cargo_and_cargo_home() {
        assert_eq!(
            detect(Path::new("/home/u/.cargo/bin/dman"), &env(), None),
            InstallMethod::Cargo
        );
        let mut e = env();
        e.cargo_home = Some("/data/cargo".into());
        assert_eq!(
            detect(Path::new("/data/cargo/bin/dman"), &e, None),
            InstallMethod::Cargo
        );
        assert_eq!(
            detect(Path::new("/home/u/.cargo/bin/dman"), &e, None),
            InstallMethod::Unknown
        );
    }

    #[test]
    fn receipt_wins_over_cargo_only_at_its_own_location() {
        let r = Receipt {
            install_prefix: "/home/u/.cargo".into(),
        };
        assert_eq!(
            detect(Path::new("/home/u/.cargo/bin/dman"), &env(), Some(&r)),
            InstallMethod::Installer
        );
        let r = Receipt {
            install_prefix: "/home/u/.local/bin".into(),
        };
        assert_eq!(
            detect(Path::new("/home/u/.local/bin/dman"), &env(), Some(&r)),
            InstallMethod::Installer
        );
        // receipt for another location does not claim a cargo install
        assert_eq!(
            detect(Path::new("/home/u/.cargo/bin/dman"), &env(), Some(&r)),
            InstallMethod::Cargo
        );
    }

    #[test]
    fn detects_system_and_unknown() {
        assert_eq!(
            detect(Path::new("/usr/bin/dman"), &env(), None),
            InstallMethod::System
        );
        assert_eq!(
            detect(Path::new("/usr/sbin/dman"), &env(), None),
            InstallMethod::System
        );
        assert_eq!(
            detect(Path::new("/tmp/x/dman"), &env(), None),
            InstallMethod::Unknown
        );
    }

    #[test]
    fn receipt_path_and_parse() {
        assert_eq!(
            Receipt::path(Some(Path::new("/h")), None).unwrap(),
            Path::new("/h/.config/dman/dman-receipt.json")
        );
        assert_eq!(
            Receipt::path(Some(Path::new("/h")), Some(Path::new("/x"))).unwrap(),
            Path::new("/x/dman/dman-receipt.json")
        );
        let json = r#"{"binaries":["dman"],"install_prefix":"/h/.local","install_layout":"flat","version":"0.1.0","source":{"app_name":"dman"}}"#;
        let r: Receipt = serde_json::from_str(json).unwrap();
        assert_eq!(r.install_prefix, Path::new("/h/.local"));
    }

    #[test]
    fn version_comparison() {
        let v = |s: &str| parse_version(s).unwrap();
        assert!(v("v0.2.0") > v("0.1.9"));
        assert!(v("1.0.0") > v("1.0.0-rc.1"));
        assert!(v("0.10.0") > v("0.9.0"));
        assert_eq!(v("v1.2.3"), v("1.2.3"));
        assert!(parse_version("nightly").is_none());
    }

    #[test]
    fn checksum_verification() {
        let data = b"hello";
        let sum = sha256_hex(data);
        assert_eq!(
            sum,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert!(verify_checksum(data, &format!("{sum} *a.tar.gz\n"), "a.tar.gz").is_ok());
        assert!(verify_checksum(data, &sum, "a.tar.gz").is_ok());
        assert!(
            verify_checksum(
                data,
                &format!("{}  a.tar.gz", sum.to_uppercase()),
                "a.tar.gz"
            )
            .is_ok()
        );
        assert!(
            verify_checksum(b"hellO", &sum, "a.tar.gz")
                .unwrap_err()
                .contains("mismatch")
        );
        assert!(verify_checksum(data, &format!("{sum}  other.tar.gz"), "a.tar.gz").is_err());
    }

    #[test]
    fn atomic_replace_swaps_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("dman");
        let new = dir.path().join("fresh");
        std::fs::write(&exe, "old").unwrap();
        std::fs::write(&new, "new").unwrap();
        replace_binary(&exe, &new).unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
        assert!(!dir.path().join("dman.old").exists());
        assert!(!dir.path().join("dman.new").exists());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn failed_replace_keeps_the_old_binary() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("dman");
        std::fs::write(&exe, "old").unwrap();
        assert!(replace_binary(&exe, &dir.path().join("missing")).is_err());
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old");
    }

    fn release_json(server: &MockServer, tag: &str) -> serde_json::Value {
        serde_json::json!({
            "tag_name": tag,
            "assets": [{
                "name": "dman-x.tar.gz",
                "browser_download_url": format!("{}/dl/dman-x.tar.gz", server.uri()),
            }],
        })
    }

    async fn mock_latest(server: &MockServer, tag: &str) {
        Mock::given(method("GET"))
            .and(path(format!("/repos/{REPO}/releases/latest")))
            .respond_with(ResponseTemplate::new(200).set_body_json(release_json(server, tag)))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn check_reports_latest_from_api() {
        let server = MockServer::start().await;
        mock_latest(&server, "v9.0.0").await;
        let client = http_client(Duration::from_secs(5)).unwrap();
        let release = fetch_release(&client, &server.uri()).await.unwrap();
        assert_eq!(
            parse_version(&release.tag_name).unwrap(),
            Version::new(9, 0, 0)
        );
        assert_eq!(release.assets.len(), 1);
    }

    #[tokio::test]
    async fn disabled_check_makes_no_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cur = Version::new(0, 1, 0);
        let got = check_for_update(false, dir.path(), &server.uri(), &cur, 1_000_000).await;
        assert!(got.is_none());
        assert!(server.received_requests().await.unwrap().is_empty());
        assert!(!dir.path().join(CACHE_FILE).exists());
    }

    #[tokio::test]
    async fn enabled_check_requests_at_most_once_a_day() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/repos/{REPO}/releases/latest")))
            .respond_with(ResponseTemplate::new(200).set_body_json(release_json(&server, "v0.2.0")))
            .expect(2)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cur = Version::new(0, 1, 0);
        let t0 = 10_000_000;
        let a = check_for_update(true, dir.path(), &server.uri(), &cur, t0).await;
        assert_eq!(a, Some(Version::new(0, 2, 0)));
        // within a day: answered from the cache
        let b = check_for_update(true, dir.path(), &server.uri(), &cur, t0 + 3600).await;
        assert_eq!(b, Some(Version::new(0, 2, 0)));
        // a day later: one more request
        let c = check_for_update(
            true,
            dir.path(),
            &server.uri(),
            &cur,
            t0 + CHECK_INTERVAL_SECS,
        )
        .await;
        assert_eq!(c, Some(Version::new(0, 2, 0)));
    }

    #[tokio::test]
    async fn check_is_silent_when_up_to_date_or_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let cur = Version::new(0, 1, 0);
        let got = check_for_update(true, dir.path(), "http://127.0.0.1:1", &cur, 5_000_000).await;
        assert!(got.is_none());
        let server = MockServer::start().await;
        mock_latest(&server, "v0.1.0").await;
        let dir2 = tempfile::tempdir().unwrap();
        assert!(
            check_for_update(true, dir2.path(), &server.uri(), &cur, 5_000_000)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn installs_release_end_to_end() {
        let server = MockServer::start().await;
        let work = tempfile::tempdir().unwrap();
        // build a cargo-dist style archive: dman-x/dman
        let pkg = work.path().join("dman-x");
        std::fs::create_dir(&pkg).unwrap();
        std::fs::write(pkg.join("dman"), "NEWBIN").unwrap();
        let tgz = work.path().join("dman-x.tar.gz");
        assert!(
            Process::new("tar")
                .arg("-czf")
                .arg(&tgz)
                .arg("-C")
                .arg(work.path())
                .arg("dman-x")
                .status()
                .unwrap()
                .success()
        );
        let bytes = std::fs::read(&tgz).unwrap();
        let target = "x";
        let name = format!("dman-{target}.tar.gz");
        Mock::given(method("GET"))
            .and(path("/dl/a"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.clone()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/dl/a.sha256"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(format!("{}  {name}\n", sha256_hex(&bytes))),
            )
            .mount(&server)
            .await;
        let release = Release {
            tag_name: "v9.9.9".into(),
            assets: vec![
                Asset {
                    name: name.clone(),
                    browser_download_url: format!("{}/dl/a", server.uri()),
                },
                Asset {
                    name: format!("{name}.sha256"),
                    browser_download_url: format!("{}/dl/a.sha256", server.uri()),
                },
            ],
        };
        let bin_dir = tempfile::tempdir().unwrap();
        let exe = bin_dir.path().join("dman");
        std::fs::write(&exe, "OLDBIN").unwrap();
        let client = http_client(Duration::from_secs(5)).unwrap();
        install_release(&client, &release, target, &exe)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "NEWBIN");

        // a corrupted checksum must leave the binary alone
        let bad = Release {
            tag_name: release.tag_name.clone(),
            assets: vec![
                Asset {
                    name: name.clone(),
                    browser_download_url: format!("{}/dl/a", server.uri()),
                },
                Asset {
                    name: format!("{name}.sha256"),
                    browser_download_url: format!("{}/dl/bad", server.uri()),
                },
            ],
        };
        Mock::given(method("GET"))
            .and(path("/dl/bad"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(format!("{}  {name}\n", "0".repeat(64))),
            )
            .mount(&server)
            .await;
        std::fs::write(&exe, "OLDBIN").unwrap();
        let err = install_release(&client, &bad, target, &exe)
            .await
            .unwrap_err();
        assert!(err.message.contains("checksum mismatch"));
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "OLDBIN");
    }
}
