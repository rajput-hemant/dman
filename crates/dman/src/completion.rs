//! Shell completion: the dynamic completion engine (job IDs, providers, profiles, ...) and
//! `dman completions [install]`.
//!
//! The shell calls `COMPLETE=<shell> dman -- <words>`; `main` hands that to
//! [`handle_env_request`] before anything else runs. Completers must be fast and silent:
//! they never print errors, never start the daemon and never create files.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap_complete::engine::{ArgValueCandidates, CompletionCandidate, PathCompleter};
use clap_complete::env::{CompleteEnv, Shells};
use dman_client::Client;
use dman_proto::JobState;
use dman_proto::method::ListParams;

use crate::cli::{CompletionsArgs, CompletionsCmd, InstallShell};
use crate::connect::{CLIENT_NAME, Ctx};
use crate::error::CliError;

/// Environment variable of clap's completion protocol.
const ENV_VAR: &str = "COMPLETE";

/// Upper bound for connecting to the daemon while completing.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
/// Upper bound for the request itself once connected.
const CALL_TIMEOUT: Duration = Duration::from_millis(300);

/// If the shell is asking for completions (`COMPLETE=<shell>`), answer and exit.
/// Must run before anything is written to stdout.
pub fn handle_env_request() {
    CompleteEnv::with_factory(crate::cli::command)
        .var(ENV_VAR)
        .complete();
}

// ---------------------------------------------------------------------------------------
// Candidate sources
// ---------------------------------------------------------------------------------------

/// Quality presets of `add --quality`.
pub const QUALITY_PRESETS: &[&str] = &[
    "best",
    "2160p",
    "1440p",
    "1080p",
    "720p",
    "480p",
    "audio:m4a",
    "audio:mp3",
    "audio:opus",
];
/// Audio preferences of `video prefs --audio`.
pub const AUDIO_PREFS: &[&str] = &["best", "m4a", "opus", "mp3"];
/// Containers of `add --container`.
pub const CONTAINERS: &[&str] = &["mp4", "mkv", "webm"];
/// Browsers yt-dlp can read cookies from.
pub const BROWSERS: &[&str] = &[
    "firefox", "chrome", "chromium", "edge", "brave", "opera", "vivaldi", "safari",
];
/// Search categories.
pub const CATEGORIES: &[&str] = &["movies", "tv", "audio", "software", "books", "xxx", "other"];
/// SponsorBlock categories.
pub const SPONSORBLOCK: &[&str] = &[
    "sponsor",
    "intro",
    "outro",
    "selfpromo",
    "preview",
    "filler",
    "interaction",
    "music_offtopic",
];

fn fixed(values: &'static [&'static str]) -> ArgValueCandidates {
    ArgValueCandidates::new(move || values.iter().map(CompletionCandidate::new).collect())
}

pub fn quality() -> ArgValueCandidates {
    fixed(QUALITY_PRESETS)
}
pub fn audio() -> ArgValueCandidates {
    fixed(AUDIO_PREFS)
}
pub fn container() -> ArgValueCandidates {
    fixed(CONTAINERS)
}
pub fn browser() -> ArgValueCandidates {
    fixed(BROWSERS)
}
pub fn category() -> ArgValueCandidates {
    fixed(CATEGORIES)
}
pub fn sponsorblock() -> ArgValueCandidates {
    fixed(SPONSORBLOCK)
}

/// Job IDs (name as description) of jobs in any state.
pub fn job_ids_any() -> ArgValueCandidates {
    ArgValueCandidates::new(|| job_candidates(&[]))
}
/// Jobs that can be paused.
pub fn job_ids_running() -> ArgValueCandidates {
    ArgValueCandidates::new(|| {
        job_candidates(&[JobState::Active, JobState::Waiting, JobState::Seeding])
    })
}
/// Jobs that can be resumed (or retried).
pub fn job_ids_paused() -> ArgValueCandidates {
    ArgValueCandidates::new(|| job_candidates(&[JobState::Paused, JobState::Error]))
}
/// Jobs that `purge` can drop from the history.
pub fn job_ids_stopped() -> ArgValueCandidates {
    ArgValueCandidates::new(|| {
        job_candidates(&[JobState::Complete, JobState::Error, JobState::Removed])
    })
}
/// Jobs that `restore` can bring back (removed from the list).
pub fn job_ids_removed() -> ArgValueCandidates {
    ArgValueCandidates::new(|| job_candidates(&[JobState::Removed]))
}
/// Jobs `cleanup` applies to (finished torrents).
pub fn job_ids_finished() -> ArgValueCandidates {
    ArgValueCandidates::new(|| job_candidates(&[JobState::Complete, JobState::Seeding]))
}

/// Provider names of every provider.
pub fn providers_any() -> ArgValueCandidates {
    ArgValueCandidates::new(|| provider_candidates(|_| true))
}
/// Providers that are currently disabled.
pub fn providers_disabled() -> ArgValueCandidates {
    ArgValueCandidates::new(|| provider_candidates(|p| !p.enabled))
}
/// Providers that are currently enabled.
pub fn providers_enabled() -> ArgValueCandidates {
    ArgValueCandidates::new(|| provider_candidates(|p| p.enabled))
}
/// Custom providers (the only removable ones).
pub fn providers_custom() -> ArgValueCandidates {
    ArgValueCandidates::new(|| provider_candidates(|p| !p.builtin))
}

/// Profile names from `config.toml` (no daemon needed).
pub fn profiles() -> ArgValueCandidates {
    ArgValueCandidates::new(|| profile_candidates(None))
}

/// Dotted keys of `dman config get|set|unset` (no daemon needed).
pub fn config_keys() -> ArgValueCandidates {
    ArgValueCandidates::new(|| {
        let Ok(paths) = dman_config::Paths::discover() else {
            return Vec::new();
        };
        let config = dman_config::Config::load(&paths).unwrap_or_default();
        dman_config::config_keys(&config)
            .into_iter()
            .map(CompletionCandidate::new)
            .collect()
    })
}

/// Files that may be given to `dman add`: directories and torrent/metalink files.
pub fn add_sources() -> clap_complete::engine::ArgValueCompleter {
    clap_complete::engine::ArgValueCompleter::new(PathCompleter::any().filter(|p| {
        p.is_dir()
            || p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                ["torrent", "metalink", "meta4", "txt"]
                    .iter()
                    .any(|x| e.eq_ignore_ascii_case(x))
            })
    }))
}
/// Any file.
pub fn any_file() -> clap_complete::engine::ArgValueCompleter {
    clap_complete::engine::ArgValueCompleter::new(PathCompleter::file())
}
/// Directories only.
pub fn any_dir() -> clap_complete::engine::ArgValueCompleter {
    clap_complete::engine::ArgValueCompleter::new(PathCompleter::dir())
}

/// `--profile NAME` / `--profile=NAME` among the words being completed.
fn profile_from_args(args: impl IntoIterator<Item = OsString>) -> Option<String> {
    let args: Vec<String> = args
        .into_iter()
        .filter_map(|a| a.into_string().ok())
        .collect();
    let mut found = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--profile=") {
            found = Some(v.to_owned());
        } else if a == "--profile" {
            found = it.next().cloned();
        }
    }
    found.filter(|p| !p.is_empty())
}

fn profile_candidates(config_dir: Option<&Path>) -> Vec<CompletionCandidate> {
    let paths = match config_dir {
        Some(d) => dman_config::Paths::with_root(d),
        None => match dman_config::Paths::discover() {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        },
    };
    let Ok(config) = dman_config::Config::load(&paths) else {
        return Vec::new();
    };
    config
        .profiles
        .iter()
        .map(|(name, profile)| {
            let kind = match &profile.daemon {
                dman_config::DaemonTarget::Local { .. } => "local daemon".to_owned(),
                dman_config::DaemonTarget::Remote { url, .. } => format!("remote {url}"),
            };
            let default = if *name == config.default_profile {
                " (default)"
            } else {
                ""
            };
            CompletionCandidate::new(name).help(Some(format!("{kind}{default}").into()))
        })
        .collect()
}

/// Run `f` against a connected daemon client, or return `None` quietly on any problem.
fn with_daemon<T>(f: impl AsyncFnOnce(Client) -> Option<T>) -> Option<T> {
    let profile = profile_from_args(std::env::args_os());
    let ctx = Ctx::load_profile(profile.as_deref(), false).ok()?;
    let (url, token) = ctx.endpoint_with(false).ok()?;
    let tls = ctx.tls_options().ok()?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    rt.block_on(async {
        let client = tokio::time::timeout(
            CONNECT_TIMEOUT,
            Client::connect_tls(&url, token, CLIENT_NAME, &tls),
        )
        .await
        .ok()?
        .ok()?;
        tokio::time::timeout(CALL_TIMEOUT, f(client)).await.ok()?
    })
}

fn job_candidates(states: &[JobState]) -> Vec<CompletionCandidate> {
    with_daemon(async |client| {
        let jobs = client
            .list(ListParams {
                states: states.to_vec(),
            })
            .await
            .ok()?;
        Some(
            jobs.into_iter()
                .map(|j| CompletionCandidate::new(j.id.to_string()).help(Some(j.name.into())))
                .collect(),
        )
    })
    .unwrap_or_default()
}

fn provider_candidates(
    keep: impl Fn(&dman_proto::search::ProviderInfo) -> bool,
) -> Vec<CompletionCandidate> {
    with_daemon(async |client| {
        let list = client.providers_list().await.ok()?;
        Some(
            list.into_iter()
                .filter(|p| keep(p))
                .map(|p| {
                    let state = if p.enabled { "enabled" } else { "disabled" };
                    CompletionCandidate::new(&p.name)
                        .help(Some(format!("{} ({state})", p.kind).into()))
                })
                .collect(),
        )
    })
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------
// Registration scripts and `dman completions`
// ---------------------------------------------------------------------------------------

/// The registration script of `shell` (what `COMPLETE=<shell> dman` prints), calling `dman`
/// through `$PATH`.
pub fn registration_script(shell: &str) -> Result<Vec<u8>, CliError> {
    let shells = Shells::builtins();
    let completer = shells.completer(shell).ok_or_else(|| {
        CliError::invalid(format!("unsupported shell '{shell}'"))
            .with_hint("use bash, zsh, fish, elvish or powershell")
    })?;
    let mut buf = Vec::new();
    completer.write_registration(ENV_VAR, "dman", "dman", "dman", &mut buf)?;
    Ok(buf)
}

pub fn run(args: &CompletionsArgs, json: bool) -> Result<i32, CliError> {
    match (&args.cmd, args.shell) {
        (Some(CompletionsCmd::Install { shell, force }), _) => {
            let env = InstallEnv::from_process();
            let report = install(&env, *shell, *force)?;
            print_report(&report, json)?;
            Ok(0)
        }
        (None, Some(shell)) => {
            let script = registration_script(&shell.to_string())?;
            use std::io::Write as _;
            std::io::stdout().write_all(&script)?;
            Ok(0)
        }
        (None, None) => Err(CliError::invalid(
            "specify a shell (bash, zsh, fish, elvish, powershell) to print its script, \
             or run `dman completions install`",
        )
        .with_hint("e.g. `dman completions install`")),
    }
}

/// What the install needs to know about the user's environment.
#[derive(Debug, Clone, Default)]
pub struct InstallEnv {
    pub home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub zdotdir: Option<PathBuf>,
    pub shell: Option<String>,
}

impl InstallEnv {
    pub fn from_process() -> Self {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Self {
            home: var("HOME"),
            // Per the XDG spec relative values must be ignored.
            xdg_data_home: var("XDG_DATA_HOME").filter(|p| p.is_absolute()),
            xdg_config_home: var("XDG_CONFIG_HOME").filter(|p| p.is_absolute()),
            zdotdir: var("ZDOTDIR"),
            shell: std::env::var("SHELL").ok().filter(|s| !s.is_empty()),
        }
    }

    fn home(&self) -> Result<&Path, CliError> {
        self.home.as_deref().ok_or_else(|| {
            CliError::invalid("$HOME is not set; cannot choose where to install")
                .with_hint("set HOME, or print the script with `dman completions <shell>`")
        })
    }

    /// Shell named by `$SHELL` (`/usr/bin/zsh` -> zsh).
    pub fn detect_shell(&self) -> Option<InstallShell> {
        let name = Path::new(self.shell.as_deref()?).file_name()?.to_str()?;
        InstallShell::from_name(name)
    }

    fn bash_path(&self) -> Result<PathBuf, CliError> {
        let base = match &self.xdg_data_home {
            Some(d) => d.clone(),
            None => self.home()?.join(".local/share"),
        };
        Ok(base.join("bash-completion/completions/dman"))
    }

    fn zsh_path(&self) -> Result<PathBuf, CliError> {
        Ok(self.home()?.join(".zfunc/_dman"))
    }

    fn zshrc(&self) -> Result<PathBuf, CliError> {
        let dir = match &self.zdotdir {
            Some(d) => d.clone(),
            None => self.home()?.to_path_buf(),
        };
        Ok(dir.join(".zshrc"))
    }

    fn fish_path(&self) -> Result<PathBuf, CliError> {
        let base = match &self.xdg_config_home {
            Some(d) => d.clone(),
            None => self.home()?.join(".config"),
        };
        Ok(base.join("fish/completions/dman.fish"))
    }
}

/// What `install` did, and what the user still has to do.
#[derive(Debug)]
pub struct InstallReport {
    pub shell: InstallShell,
    pub path: PathBuf,
    pub action: InstallAction,
    /// Lines the user should add to their rc file (empty = nothing to do).
    pub rc_lines: Vec<String>,
    /// Where `rc_lines` go.
    pub rc_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallAction {
    Created,
    Replaced,
    Unchanged,
}

pub fn install(
    env: &InstallEnv,
    shell: Option<InstallShell>,
    force: bool,
) -> Result<InstallReport, CliError> {
    let shell = match shell.or_else(|| env.detect_shell()) {
        Some(s) => s,
        None => {
            return Err(CliError::invalid(match &env.shell {
                Some(s) => format!(
                    "cannot install completions for $SHELL={s}; use --shell bash|zsh|fish, or \
                     print a script with `dman completions <shell>`"
                ),
                None => "cannot detect your shell ($SHELL is not set); use --shell bash|zsh|fish"
                    .to_owned(),
            })
            .with_hint("e.g. `dman completions install --shell bash`"));
        }
    };
    let path = match shell {
        InstallShell::Bash => env.bash_path()?,
        InstallShell::Zsh => env.zsh_path()?,
        InstallShell::Fish => env.fish_path()?,
    };
    let script = registration_script(shell.name())?;
    let action = match std::fs::read(&path) {
        Ok(existing) if existing == script => InstallAction::Unchanged,
        Ok(_) if !force => {
            return Err(CliError::refused(format!(
                "{} already exists with different content; not overwriting it \
                 (rerun with --force to replace it)",
                path.display()
            ))
            .with_hint("rerun with --force to replace it"));
        }
        Ok(_) => InstallAction::Replaced,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => InstallAction::Created,
        Err(e) => {
            return Err(CliError::general(format!(
                "cannot read {}: {e}",
                path.display()
            )));
        }
    };
    if action != InstallAction::Unchanged {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| CliError::general(format!("cannot create {}: {e}", dir.display())))?;
        }
        std::fs::write(&path, &script)
            .map_err(|e| CliError::general(format!("cannot write {}: {e}", path.display())))?;
    }
    let (rc_file, rc_lines) = match shell {
        InstallShell::Bash => bash_rc(env, &path),
        InstallShell::Zsh => zsh_rc(env)?,
        InstallShell::Fish => (None, Vec::new()),
    };
    Ok(InstallReport {
        shell,
        path,
        action,
        rc_lines,
        rc_file,
    })
}

/// Bash needs the `bash-completion` package to load files from the completions directory.
fn bash_rc(env: &InstallEnv, installed: &Path) -> (Option<PathBuf>, Vec<String>) {
    const MARKERS: &[&str] = &[
        "/usr/share/bash-completion/bash_completion",
        "/etc/bash_completion",
        "/opt/homebrew/etc/profile.d/bash_completion.sh",
        "/usr/local/etc/profile.d/bash_completion.sh",
    ];
    if MARKERS.iter().any(|m| Path::new(m).exists()) {
        return (None, Vec::new());
    }
    let bashrc = env.home.as_ref().map(|h| h.join(".bashrc"));
    (bashrc, vec![format!("source {}", installed.display())])
}

fn zsh_rc(env: &InstallEnv) -> Result<(Option<PathBuf>, Vec<String>), CliError> {
    let zshrc = env.zshrc()?;
    let text = std::fs::read_to_string(&zshrc).unwrap_or_default();
    let code: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .collect();
    let has_fpath = code
        .iter()
        .any(|l| l.contains("fpath") && l.contains("zfunc"));
    let has_compinit = code.iter().any(|l| l.contains("compinit"));
    let mut lines = Vec::new();
    if !has_fpath {
        lines.push("fpath=(~/.zfunc $fpath)".to_owned());
    }
    if !has_compinit {
        lines.push("autoload -Uz compinit && compinit".to_owned());
    }
    Ok((Some(zshrc), lines))
}

fn print_report(r: &InstallReport, json: bool) -> Result<(), CliError> {
    if json {
        let action = match r.action {
            InstallAction::Created => "created",
            InstallAction::Replaced => "replaced",
            InstallAction::Unchanged => "unchanged",
        };
        return crate::commands::print_json(&serde_json::json!({
            "shell": r.shell.name(),
            "path": r.path,
            "action": action,
            "rc_file": r.rc_file,
            "add_to_rc": r.rc_lines,
        }));
    }
    let verb = match r.action {
        InstallAction::Created => "installed",
        InstallAction::Replaced => "replaced",
        InstallAction::Unchanged => "already up to date:",
    };
    println!(
        "{verb} {} completions in {}",
        r.shell.name(),
        r.path.display()
    );
    if r.rc_lines.is_empty() {
        println!("nothing to add to your shell configuration.");
    } else {
        let rc = r.rc_file.as_ref().map_or_else(
            || "your shell rc file".to_owned(),
            |p| p.display().to_string(),
        );
        println!("add these lines to {rc}:");
        for l in &r.rc_lines {
            println!("  {l}");
        }
    }
    println!(
        "open a new shell (or `exec {}`) to use them.",
        r.shell.name()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(c: &[CompletionCandidate]) -> Vec<String> {
        c.iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect()
    }

    fn env_in(dir: &Path, shell: &str) -> InstallEnv {
        InstallEnv {
            home: Some(dir.to_path_buf()),
            shell: Some(shell.to_owned()),
            ..InstallEnv::default()
        }
    }

    #[test]
    fn profile_flag_is_found_in_words() {
        let a = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            profile_from_args(a(&["dman", "--", "dman", "--profile", "nas", "pause", ""])),
            Some("nas".into())
        );
        assert_eq!(
            profile_from_args(a(&["dman", "--profile=x", "list"])),
            Some("x".into())
        );
        assert_eq!(profile_from_args(a(&["dman", "list"])), None);
    }

    #[test]
    fn fixed_lists_are_complete() {
        for (c, want) in [
            (CONTAINERS, "mkv"),
            (QUALITY_PRESETS, "audio:opus"),
            (BROWSERS, "firefox"),
            (CATEGORIES, "tv"),
            (SPONSORBLOCK, "sponsor"),
        ] {
            assert!(c.contains(&want));
        }
        let q = fixed(QUALITY_PRESETS).candidates();
        assert_eq!(texts(&q).len(), QUALITY_PRESETS.len());
    }

    #[test]
    fn profiles_come_from_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "default_profile = \"local\"\n\
             [profiles.local]\n\
             [profiles.local.daemon]\n\
             kind = \"local\"\n\
             listen = \"127.0.0.1:1\"\n\
             secret_ref = \"daemon\"\n\
             [profiles.nas]\n\
             [profiles.nas.daemon]\n\
             kind = \"remote\"\n\
             url = \"wss://nas.example:6810\"\n\
             secret_ref = \"nas\"\n",
        )
        .unwrap();
        let c = profile_candidates(Some(dir.path()));
        assert_eq!(texts(&c), ["local", "nas"]);
        assert!(c[0].get_help().unwrap().to_string().contains("(default)"));
    }

    #[test]
    fn registration_scripts_exist_for_every_shell() {
        for s in ["bash", "zsh", "fish", "elvish", "powershell"] {
            let script = registration_script(s).unwrap();
            assert!(String::from_utf8(script).unwrap().contains("dman"), "{s}");
        }
        assert!(registration_script("tcsh").is_err());
    }

    #[test]
    fn detects_the_shell_from_the_environment() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            env_in(d.path(), "/usr/bin/zsh").detect_shell(),
            Some(InstallShell::Zsh)
        );
        assert_eq!(
            env_in(d.path(), "/bin/bash").detect_shell(),
            Some(InstallShell::Bash)
        );
        assert_eq!(env_in(d.path(), "/bin/tcsh").detect_shell(), None);
    }

    #[test]
    fn install_writes_the_right_file_per_shell() {
        let d = tempfile::tempdir().unwrap();
        for (shell, rel) in [
            ("bash", ".local/share/bash-completion/completions/dman"),
            ("zsh", ".zfunc/_dman"),
            ("fish", ".config/fish/completions/dman.fish"),
        ] {
            let r = install(&env_in(d.path(), &format!("/bin/{shell}")), None, false).unwrap();
            assert_eq!(r.path, d.path().join(rel));
            assert_eq!(r.action, InstallAction::Created);
            assert!(std::fs::read_to_string(&r.path).unwrap().contains("dman"));
        }
    }

    #[test]
    fn install_is_idempotent_and_refuses_to_overwrite_without_force() {
        let d = tempfile::tempdir().unwrap();
        let env = env_in(d.path(), "/usr/bin/fish");
        let first = install(&env, None, false).unwrap();
        assert_eq!(
            install(&env, None, false).unwrap().action,
            InstallAction::Unchanged
        );
        std::fs::write(&first.path, "# mine\n").unwrap();
        let err = install(&env, None, false).unwrap_err();
        assert!(err.message.contains("--force"), "{}", err.message);
        assert_eq!(std::fs::read_to_string(&first.path).unwrap(), "# mine\n");
        let r = install(&env, None, true).unwrap();
        assert_eq!(r.action, InstallAction::Replaced);
        assert_ne!(std::fs::read_to_string(&first.path).unwrap(), "# mine\n");
    }

    #[test]
    fn zsh_tells_what_is_missing_from_zshrc() {
        let d = tempfile::tempdir().unwrap();
        let env = env_in(d.path(), "/bin/zsh");
        let r = install(&env, None, false).unwrap();
        assert_eq!(r.rc_lines.len(), 2);
        assert_eq!(r.rc_file, Some(d.path().join(".zshrc")));
        std::fs::write(
            d.path().join(".zshrc"),
            "# fpath=(~/.zfunc $fpath)\nfpath=(~/.zfunc $fpath)\n",
        )
        .unwrap();
        let r = install(&env, None, false).unwrap();
        assert_eq!(r.rc_lines, ["autoload -Uz compinit && compinit"]);
        std::fs::write(
            d.path().join(".zshrc"),
            "fpath=(~/.zfunc $fpath)\nautoload -Uz compinit\ncompinit\n",
        )
        .unwrap();
        assert!(install(&env, None, false).unwrap().rc_lines.is_empty());
    }

    #[test]
    fn unknown_shell_and_missing_home_are_usage_errors() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            install(&env_in(d.path(), "/bin/tcsh"), None, false)
                .unwrap_err()
                .exit,
            2
        );
        assert!(install(&InstallEnv::default(), Some(InstallShell::Fish), false).is_err());
    }
}
