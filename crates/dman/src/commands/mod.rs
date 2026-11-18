//! Command implementations. Each returns the process exit code on success.

pub mod add;
pub mod clipboard;
pub mod config;
pub mod control;
pub mod daemon;
pub mod hints;
pub mod integrate;
pub mod list;
pub mod misc;
pub mod progress;
pub mod providers;
pub mod quality;
pub mod search;
pub mod serve;
pub mod set;
pub mod update;
pub mod wait;

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
    CliError::refused(format!("this build of dman has no {what}"))
        .with_hint(format!("rebuild with --features {feature}"))
}

/// Prompts are shown only when both ends are a terminal (stdin to answer, stderr
/// where the question appears). Anything else must pass `--yes` or fails.
pub fn can_prompt() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// `y` / `yes` in any case is yes, an empty answer is `default`, anything else is no.
pub fn is_yes(answer: &str, default: bool) -> bool {
    match answer.trim().to_ascii_lowercase().as_str() {
        "" => default,
        a => matches!(a, "y" | "yes"),
    }
}

/// Ask a yes/no question on stderr and read the answer from stdin; Enter means
/// `default`. The end of input (Ctrl-D, a closed stdin) is always no.
pub fn ask_yes(question: &str, default: bool) -> std::io::Result<bool> {
    ask_yes_with(
        question,
        default,
        &mut std::io::stdin().lock(),
        &mut std::io::stderr(),
    )
}

fn ask_yes_with(
    question: &str,
    default: bool,
    input: &mut impl std::io::BufRead,
    out: &mut impl std::io::Write,
) -> std::io::Result<bool> {
    let choices = if default { "[Y/n]" } else { "[y/N]" };
    write!(out, "{question} {choices} ")?;
    out.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(false);
    }
    Ok(is_yes(&line, default))
}

/// Run the selected command and return the process exit code. A "not found" failure
/// gets did-you-mean suggestions from what the daemon actually has.
pub async fn dispatch(cli: Cli) -> Result<i32, CliError> {
    let (profile, json) = (cli.profile.clone(), cli.json);
    match dispatch_command(cli).await {
        Err(e) => Err(hints::enrich(profile.as_deref(), json, e).await),
        ok => ok,
    }
}

async fn dispatch_command(cli: Cli) -> Result<i32, CliError> {
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
        Command::Set(args) => set::run(&Ctx::load(&cli)?, args).await,
        Command::Handle(args) => integrate::handle(&cli, args).await,
        Command::Integrate { cmd } => integrate::run(*cmd, cli.json),
        Command::List(args) => {
            let ctx = Ctx::load(&cli)?;
            let code = list::list(&ctx, args).await?;
            if !args.watch {
                update::notice(&ctx).await;
            }
            Ok(code)
        }
        Command::Update(args) => update::run(&Ctx::load(&cli)?, args).await,
        Command::Wait(args) => wait::run(&Ctx::load(&cli)?, args).await,
        Command::Get {
            id,
            files,
            peers,
            watch,
        } => list::get(&Ctx::load(&cli)?, *id, *files, *peers, *watch).await,
        Command::Rename { id, name, file } => {
            control::rename(&Ctx::load(&cli)?, *id, name, *file).await
        }
        Command::Cleanup { id, yes } => control::cleanup(&Ctx::load(&cli)?, *id, *yes).await,
        Command::Config { cmd } => config::run(&cli, cmd).await,
        Command::Video { cmd } => match cmd {
            crate::cli::VideoCmd::Prefs(a) => quality::prefs(&Ctx::load(&cli)?, a).await,
        },
        Command::Probe {
            url,
            playlist,
            cookies_from_browser,
            plan: true,
            plan_args,
        } => {
            add::plan(
                &Ctx::load(&cli)?,
                url,
                &add::plan_options(plan_args, *playlist, cookies_from_browser.clone()),
            )
            .await
        }
        Command::Probe {
            url,
            playlist,
            cookies_from_browser,
            ..
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yes_answers() {
        assert!(is_yes("y", false) && is_yes(" YES\n", false));
        assert!(!is_yes("n", true) && !is_yes("sure", true));
        assert!(is_yes("", true) && !is_yes("\n", false));
    }

    #[test]
    fn end_of_input_is_no_even_when_enter_means_yes() {
        let ask = |input: &str, default| {
            let mut out = Vec::new();
            let yes = ask_yes_with("Go?", default, &mut input.as_bytes(), &mut out).unwrap();
            (yes, String::from_utf8(out).unwrap())
        };
        assert_eq!(ask("\n", true), (true, "Go? [Y/n] ".to_owned()));
        assert_eq!(ask("", true), (false, "Go? [Y/n] ".to_owned()));
        assert_eq!(ask("y\n", false), (true, "Go? [y/N] ".to_owned()));
        assert!(!ask("\n", false).0);
    }
}
