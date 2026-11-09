//! Command implementations. Each returns the process exit code on success.

pub mod add;
pub mod control;
pub mod daemon;
pub mod list;
pub mod misc;
pub mod providers;
pub mod quality;
pub mod search;
pub mod serve;
pub mod update;

use crate::cli::{Cli, Command};
use crate::connect::Ctx;
use crate::error::CliError;

/// Pretty-print a value as JSON on stdout.
pub fn print_json<T: serde::Serialize>(value: &T) -> Result<(), CliError> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// One status line: `{"status": "<status>"}` with `--json`, else the human text.
pub fn say(ctx: &Ctx, human: &str, status: &str) -> Result<(), CliError> {
    if ctx.json {
        print_json(&serde_json::json!({ "status": status }))
    } else {
        println!("{human}");
        Ok(())
    }
}

/// The error for a command whose feature was compiled out of this build.
fn missing(what: &str, feature: &str) -> CliError {
    CliError::refused(format!(
        "this build of dman has no {what}; rebuild with --features {feature}"
    ))
}

/// Prompts are shown only when both ends are a terminal (stdin to answer, stderr
/// where the question appears). Anything else must pass `--yes` or fails.
pub fn can_prompt() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Run the selected command and return the process exit code.
pub async fn dispatch(cli: Cli) -> Result<i32, CliError> {
    let Some(command) = cli.command.as_ref() else {
        let ctx = Ctx::load(&cli)?;
        let code = daemon::bare(&ctx).await?;
        update::notice(&ctx).await;
        return Ok(code);
    };
    match command {
        Command::Completions(args) => crate::completion::run(args, cli.json),
        Command::Search(_) | Command::Providers { .. } | Command::Trackers { .. }
            if !cfg!(feature = "search") =>
        {
            Err(missing("torrent search", "search"))
        }
        Command::Serve(_) if !cfg!(feature = "web") => Err(missing("web UI", "web")),
        Command::Search(args) => search::run(&Ctx::load(&cli)?, args).await,
        Command::Providers { cmd } => providers::run(&Ctx::load(&cli)?, cmd).await,
        Command::Trackers { cmd } => providers::trackers(&Ctx::load(&cli)?, cmd).await,
        Command::Serve(args) => serve::run(&Ctx::load(&cli)?, args).await,
        Command::Daemon { cmd } => daemon::run(&Ctx::load(&cli)?, cmd).await,
        Command::Add(args) => add::run(&Ctx::load(&cli)?, args).await,
        Command::List(args) => {
            let ctx = Ctx::load(&cli)?;
            let code = list::list(&ctx, args).await?;
            if !args.watch {
                update::notice(&ctx).await;
            }
            Ok(code)
        }
        Command::Update(args) => update::run(&Ctx::load(&cli)?, args).await,
        Command::Get { id, files, peers } => {
            list::get(&Ctx::load(&cli)?, *id, *files, *peers).await
        }
        Command::Rename { id, name, file } => {
            control::rename(&Ctx::load(&cli)?, *id, name, *file).await
        }
        Command::Cleanup { id, yes } => control::cleanup(&Ctx::load(&cli)?, *id, *yes).await,
        Command::Config { cmd } => misc::config(&Ctx::load(&cli)?, cmd).await,
        Command::Video { cmd } => match cmd {
            crate::cli::VideoCmd::Prefs(a) => quality::prefs(&Ctx::load(&cli)?, a).await,
        },
        Command::Probe {
            url,
            playlist,
            cookies_from_browser,
        } => {
            add::probe(
                &Ctx::load(&cli)?,
                url,
                *playlist,
                cookies_from_browser.clone(),
            )
            .await
        }
        Command::Preview { magnet, timeout } => {
            add::preview(&Ctx::load(&cli)?, magnet, *timeout).await
        }
        Command::Pause(a) => control::pause(&Ctx::load(&cli)?, a).await,
        Command::Resume(a) => control::resume(&Ctx::load(&cli)?, a).await,
        Command::Remove(a) => control::remove(&Ctx::load(&cli)?, a).await,
        Command::Restore(a) => control::restore(&Ctx::load(&cli)?, a).await,
        Command::Purge(a) => control::purge(&Ctx::load(&cli)?, a).await,
        Command::Reorder(a) => control::reorder(&Ctx::load(&cli)?, a).await,
        Command::Limit(a) => misc::limit(&Ctx::load(&cli)?, a).await,
        Command::MaxConcurrent { n } => misc::max_concurrent(&Ctx::load(&cli)?, *n).await,
        Command::Schedule => misc::schedule(&Ctx::load(&cli)?).await,
        Command::Stats => misc::stats(&Ctx::load(&cli)?).await,
    }
}
