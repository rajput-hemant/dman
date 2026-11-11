//! `dman config get|set|unset|edit|path|show|reload`.
//!
//! These work on `config.toml` itself, so they run without a daemon and even when the
//! file is broken (`edit` is how it gets fixed). Only the reload afterwards needs one.

use std::io::{BufRead, Write};
use std::path::Path;

use dman_config::{Config, ConfigDoc, ConfigError, Paths, TomlValue};
use dman_proto::ErrorCode;
use dman_proto::method::ReloadResult;

use super::{can_prompt, print_json};
use crate::cli::{Cli, ConfigCmd};
use crate::connect::Ctx;
use crate::error::CliError;
use crate::{format, suggest};

pub async fn run(cli: &Cli, cmd: &ConfigCmd) -> Result<i32, CliError> {
    match cmd {
        ConfigCmd::Reload => reload(&Ctx::load(cli)?).await,
        ConfigCmd::Path => path(cli.json),
        ConfigCmd::Show => show(cli.json),
        ConfigCmd::Get { key } => get(key, cli.json),
        ConfigCmd::Set {
            key,
            value,
            no_reload,
        } => set(cli, key, value, *no_reload).await,
        ConfigCmd::Unset { key, no_reload } => unset(cli, key, *no_reload).await,
        ConfigCmd::Edit { no_reload } => edit(cli, *no_reload).await,
    }
}

async fn reload(ctx: &Ctx) -> Result<i32, CliError> {
    let r = ctx.connect().await?.config_reload().await?;
    if ctx.json {
        print_json(&r)?;
    } else {
        print!("{}", format::reload_text(&r));
    }
    Ok(0)
}

fn path(json: bool) -> Result<i32, CliError> {
    let file = Paths::discover()?.config_file();
    if json {
        print_json(&serde_json::json!({ "path": file, "exists": file.exists() }))?;
    } else {
        println!("{}", file.display());
    }
    Ok(0)
}

/// The effective configuration with every secret masked.
fn redacted_tree(config: &Config) -> Result<TomlValue, CliError> {
    let mut tree = dman_config::effective_value(config)?;
    dman_config::redact_config(&mut tree);
    Ok(tree)
}

fn show(json: bool) -> Result<i32, CliError> {
    let config = Config::load(&Paths::discover()?)?;
    let tree = redacted_tree(&config)?;
    if json {
        print_json(&tree)?;
    } else {
        print!("{}", dman_config::render_toml(&tree));
    }
    Ok(0)
}

/// The error for a key that is not in the configuration, with the closest keys.
fn unknown_key(key: &str, config: &Config) -> CliError {
    let keys = dman_config::config_keys(config);
    let close = suggest::closest(key, keys.iter().map(String::as_str), 3);
    let hint = suggest::did_you_mean(&close).unwrap_or_else(|| {
        let sections = dman_config::top_level_keys().join(", ");
        format!("see all keys with `dman config show`; sections: {sections}")
    });
    CliError::new(ErrorCode::NotFound, format!("unknown config key `{key}`")).with_hint(hint)
}

fn get(key: &str, json: bool) -> Result<i32, CliError> {
    let config = Config::load(&Paths::discover()?)?;
    let parts = dman_config::split_key(key)?;
    let tree = redacted_tree(&config)?;
    let Some(value) = dman_config::lookup(&tree, &parts) else {
        return Err(unknown_key(key, &config));
    };
    if json {
        print_json(value)?;
    } else {
        let text = dman_config::render_value(value);
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
    }
    Ok(0)
}

/// What happened to the running daemon after the file changed.
enum Reloaded {
    Done(ReloadResult),
    /// `--no-reload`.
    Skipped,
    NotRunning,
    /// The selected profile's daemon is elsewhere and reads its own file.
    Remote(String),
}

async fn reload_daemon(cli: &Cli, no_reload: bool) -> Result<Reloaded, CliError> {
    if no_reload {
        return Ok(Reloaded::Skipped);
    }
    let ctx = Ctx::load(cli).map_err(|e| {
        CliError::general(format!(
            "the file was saved, but the daemon was not reloaded: {e}"
        ))
        .with_hint("fix the profile selection, then run `dman config reload`")
    })?;
    if !ctx.is_local() {
        return Ok(Reloaded::Remote(ctx.name.clone()));
    }
    let client = match ctx.connect().await {
        Ok(c) => c,
        Err(e) if Ctx::is_not_running(&e) => return Ok(Reloaded::NotRunning),
        Err(e) => return Err(saved_but(e)),
    };
    client
        .config_reload()
        .await
        .map(Reloaded::Done)
        .map_err(|e| saved_but(e.into()))
}

fn saved_but(e: CliError) -> CliError {
    let mut err = CliError::new(
        e.code,
        format!(
            "the file was saved, but the daemon did not reload it: {}",
            e.message
        ),
    );
    err.hint = e
        .hint
        .or_else(|| Some("run `dman config reload` once the daemon is healthy".to_owned()));
    err
}

/// The human text for the reload step (empty when there is nothing to say).
fn reloaded_text(r: &Reloaded) -> String {
    match r {
        Reloaded::Done(r) => {
            let mut out = format::reload_text(r);
            if !r.restart_required.is_empty() {
                out.push_str("run `dman daemon restart` to apply those\n");
            }
            out
        }
        Reloaded::Skipped => "not reloaded (--no-reload)\n".to_owned(),
        Reloaded::NotRunning => {
            "saved; the daemon is not running, so this applies when it starts\n".to_owned()
        }
        Reloaded::Remote(name) => {
            format!("saved; profile '{name}' is remote, so its daemon reads its own config.toml\n")
        }
    }
}

fn reloaded_json(r: &Reloaded) -> serde_json::Value {
    match r {
        Reloaded::Done(r) => serde_json::json!({
            "status": "reloaded",
            "applied": r.applied,
            "restart_required": r.restart_required,
        }),
        Reloaded::Skipped => serde_json::json!({ "status": "skipped" }),
        Reloaded::NotRunning => serde_json::json!({ "status": "daemon_not_running" }),
        Reloaded::Remote(name) => {
            serde_json::json!({ "status": "remote_profile", "profile": name })
        }
    }
}

/// `config.toml` as a document; a missing file starts from the commented default.
fn open_doc(paths: &Paths) -> Result<ConfigDoc, CliError> {
    if paths.config_file().exists() {
        Ok(ConfigDoc::open(paths)?)
    } else {
        Ok(ConfigDoc::parse(
            dman_config::DEFAULT_CONFIG_TOML,
            paths.config_file(),
        )?)
    }
}

/// A parse error without the echoed source line: for values typed on the command
/// line the position refers to a document the user never saw, and the value may be
/// a credential.
fn brief(e: ConfigError, key: &str, config: &Config) -> CliError {
    match e {
        ConfigError::Parse { message, .. } => {
            let reason: Vec<&str> = message
                .lines()
                .filter(|l| {
                    let t = l.trim_start();
                    !l.starts_with("TOML parse error")
                        && !t.starts_with('|')
                        && !t.split_once('|').is_some_and(|(n, _)| {
                            !n.trim().is_empty() && n.trim().chars().all(|c| c.is_ascii_digit())
                        })
                })
                .collect();
            let text = if reason.is_empty() {
                "not a valid value".to_owned()
            } else {
                reason.join("; ")
            };
            let close = if text.contains("unknown field") {
                let keys = dman_config::config_keys(config);
                suggest::closest(key, keys.iter().map(String::as_str), 3)
            } else {
                Vec::new()
            };
            let section = key.split('.').next().unwrap_or(key);
            let hint = match suggest::did_you_mean(&close) {
                Some(dym) => format!("nothing was saved; {dym}"),
                None => format!(
                    "nothing was saved; `dman config get {section}` shows the accepted keys, \
                     `dman config edit` edits the file by hand"
                ),
            };
            CliError::invalid(text).with_hint(hint)
        }
        other => CliError::from(other),
    }
}

async fn set(cli: &Cli, key: &str, value: &str, no_reload: bool) -> Result<i32, CliError> {
    let parts = dman_config::split_key(key)?;
    let paths = Paths::discover()?;
    let current = Config::load(&paths)?;
    if !dman_config::top_level_keys().iter().any(|k| *k == parts[0]) {
        return Err(unknown_key(key, &current).with_hint(
            "this version has no such section; to keep settings it does not know, use `dman config edit`",
        ));
    }
    let mut doc = open_doc(&paths)?;
    let config = match doc.set_checked(&parts, value) {
        Ok(c) => c,
        Err(ConfigError::BadPath(_)) => {
            return Err(
                CliError::invalid(format!("cannot set `{key}` this way")).with_hint(
                    "lists of tables such as [[schedule]] are changed with `dman config edit`",
                ),
            );
        }
        Err(e) => return Err(brief(e, key, &current)),
    };
    doc.save()?;
    let shown = redacted_tree(&config)?;
    let shown = dman_config::lookup(&shown, &parts).cloned();
    let reloaded = reload_daemon(cli, no_reload).await?;
    if cli.json {
        print_json(&serde_json::json!({
            "key": key,
            "value": shown,
            "saved": true,
            "reload": reloaded_json(&reloaded),
        }))?;
    } else {
        let text = shown
            .as_ref()
            .map(dman_config::render_value)
            .unwrap_or_default();
        println!("{key} = {text}");
        print!("{}", reloaded_text(&reloaded));
    }
    Ok(0)
}

async fn unset(cli: &Cli, key: &str, no_reload: bool) -> Result<i32, CliError> {
    let parts = dman_config::split_key(key)?;
    let paths = Paths::discover()?;
    let current = Config::load(&paths)?;
    if !dman_config::config_keys(&current).iter().any(|k| k == key) {
        return Err(unknown_key(key, &current));
    }
    let mut doc = open_doc(&paths)?;
    if !doc.remove(&parts)? {
        if cli.json {
            print_json(&serde_json::json!({ "key": key, "saved": false, "changed": false }))?;
        } else {
            println!("{key} is not set in config.toml; the default already applies");
        }
        return Ok(0);
    }
    let config = doc.validate().map_err(|e| brief(e, key, &current))?;
    doc.save()?;
    let shown = redacted_tree(&config)?;
    let shown = dman_config::lookup(&shown, &parts).cloned();
    let reloaded = reload_daemon(cli, no_reload).await?;
    if cli.json {
        print_json(&serde_json::json!({
            "key": key,
            "default": shown,
            "saved": true,
            "reload": reloaded_json(&reloaded),
        }))?;
    } else {
        match shown.as_ref().map(dman_config::render_value) {
            Some(d) if !d.contains('\n') => println!("unset {key} (default: {d})"),
            _ => println!("unset {key}"),
        }
        print!("{}", reloaded_text(&reloaded));
    }
    Ok(0)
}

/// Program and leading arguments of `$VISUAL` / `$EDITOR`, else the platform's editor.
fn editor_command(visual: Option<String>, editor: Option<String>) -> Vec<String> {
    let pick = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
    let line = pick(visual)
        .or_else(|| pick(editor))
        .unwrap_or_else(|| if cfg!(windows) { "notepad" } else { "vi" }.to_owned());
    line.split_whitespace().map(str::to_owned).collect()
}

fn run_editor(file: &Path) -> Result<(), CliError> {
    let words = editor_command(std::env::var("VISUAL").ok(), std::env::var("EDITOR").ok());
    let (program, args) = words.split_first().ok_or_else(|| {
        CliError::general("no editor configured").with_hint("set $VISUAL or $EDITOR")
    })?;
    let status = std::process::Command::new(program)
        .args(args)
        .arg(file)
        .status()
        .map_err(|e| {
            CliError::general(format!("cannot run the editor `{program}`: {e}"))
                .with_hint("set $VISUAL or $EDITOR to an editor on your PATH")
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::general(format!(
            "the editor exited with {status}; nothing was saved"
        ))
        .with_hint("run `dman config edit` to try again"))
    }
}

/// Ask a yes/no question on stderr; Enter means `default`.
fn ask_yes(question: &str, default: bool) -> bool {
    eprint!("{question} {} ", if default { "[Y/n]" } else { "[y/N]" });
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        return false;
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "" => default,
        a => matches!(a, "y" | "yes"),
    }
}

async fn edit(cli: &Cli, no_reload: bool) -> Result<i32, CliError> {
    let paths = Paths::discover()?;
    let file = paths.config_file();
    let original = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(CliError::general(format!(
                "cannot read {}: {e}",
                file.display()
            )));
        }
    };
    let start = if original.is_empty() {
        dman_config::DEFAULT_CONFIG_TOML
    } else {
        original.as_str()
    };
    // The editor works on a copy next to nothing important: the real file changes only
    // after the copy has been validated.
    let mut tmp = tempfile::Builder::new()
        .prefix("dman-config-")
        .suffix(".toml")
        .tempfile()?;
    tmp.write_all(start.as_bytes())?;
    tmp.flush()?;
    let text = loop {
        run_editor(tmp.path())?;
        let text = std::fs::read_to_string(tmp.path())?;
        let checked = ConfigDoc::parse(&text, &file).and_then(|d| d.validate().map(|_| ()));
        match checked {
            Ok(()) => break text,
            Err(e) => {
                eprintln!("dman: {e}");
                if can_prompt() && ask_yes("Open the editor again?", true) {
                    continue;
                }
                return Err(CliError::invalid(
                    "the edited configuration is not valid; config.toml was left unchanged",
                )
                .with_hint("run `dman config edit` to try again"));
            }
        }
    };
    if text == start {
        if cli.json {
            print_json(&serde_json::json!({ "path": file, "saved": false }))?;
        } else {
            println!("no changes");
        }
        return Ok(0);
    }
    // Someone else (another editor, `dman config set`) may have written meanwhile.
    let now = std::fs::read_to_string(&file).unwrap_or_default();
    if now != original {
        let (_, kept) = tmp.keep().map_err(|e| CliError::general(e.to_string()))?;
        return Err(CliError::refused(format!(
            "{} changed while you were editing; your version is kept in {}",
            file.display(),
            kept.display()
        ))
        .with_hint("merge the two by hand, or run `dman config edit` again"));
    }
    ConfigDoc::parse(&text, &file)?.save()?;
    let reloaded = reload_daemon(cli, no_reload).await?;
    if cli.json {
        print_json(&serde_json::json!({
            "path": file,
            "saved": true,
            "reload": reloaded_json(&reloaded),
        }))?;
    } else {
        println!("saved {}", file.display());
        print!("{}", reloaded_text(&reloaded));
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_precedence_and_fallback() {
        let s = |x: &str| Some(x.to_owned());
        assert_eq!(editor_command(s("code -w"), s("nano")), ["code", "-w"]);
        assert_eq!(editor_command(None, s("nano")), ["nano"]);
        assert_eq!(editor_command(s("  "), s("nano")), ["nano"]);
        let fallback = editor_command(None, None);
        assert_eq!(fallback, [if cfg!(windows) { "notepad" } else { "vi" }]);
    }

    #[test]
    fn brief_drops_the_echoed_source() {
        let e = Config::from_toml_str(
            "[limits]\ndownload = \"lots\"\n",
            Path::new("/x/config.toml"),
        )
        .unwrap_err();
        let err = brief(e, "limits.download", &Config::default());
        assert_eq!(err.exit, 2);
        assert!(!err.message.contains("lots\"\n"), "{}", err.message);
        assert!(!err.message.contains(" | "), "{}", err.message);
        let hint = err.hint.unwrap();
        assert!(hint.contains("nothing was saved"), "{hint}");
        assert!(hint.contains("`dman config get limits`"), "{hint}");
    }

    #[test]
    fn a_misspelled_key_on_set_gets_a_suggestion() {
        let e = Config::from_toml_str(
            "[limits]\ndownlod = \"5MiB\"\n",
            Path::new("/x/config.toml"),
        )
        .unwrap_err();
        let hint = brief(e, "limits.downlod", &Config::default()).hint.unwrap();
        assert!(hint.contains("`limits.download`"), "{hint}");
    }

    #[test]
    fn unknown_keys_get_suggestions() {
        let config = Config::default();
        let e = unknown_key("limits.dwonload", &config);
        assert_eq!(e.exit, 5);
        assert!(e.hint.unwrap().contains("`limits.download`"));
        let e = unknown_key("zzzzzzzz", &config);
        assert!(e.hint.unwrap().contains("sections:"));
    }

    #[test]
    fn reload_text_names_restarts() {
        let r = Reloaded::Done(ReloadResult {
            applied: vec!["limits".into()],
            restart_required: vec!["aria2.rpc_port".into()],
        });
        let t = reloaded_text(&r);
        assert!(
            t.contains("applied limits") && t.contains("dman daemon restart"),
            "{t}"
        );
        assert!(reloaded_text(&Reloaded::NotRunning).contains("not running"));
    }
}
