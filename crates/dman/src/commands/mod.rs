//! Command implementations. Each returns the process exit code on success.

pub mod add;
pub mod control;
pub mod daemon;
pub mod list;
pub mod misc;
pub mod providers;
pub mod search;

use crate::cli::{Cli, Command};
use crate::connect::Ctx;
use crate::error::CliError;

/// Pretty-print a value as JSON on stdout.
pub fn print_json<T: serde::Serialize>(value: &T) -> Result<(), CliError> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Run the selected command and return the process exit code.
pub async fn dispatch(cli: Cli) -> Result<i32, CliError> {
    let Some(command) = cli.command.as_ref() else {
        return daemon::bare(&Ctx::load(&cli)?).await;
    };
    match command {
        Command::Completions { shell } => misc::completions(*shell),
        Command::Search(args) => search::run(&Ctx::load(&cli)?, args).await,
        Command::Providers { cmd } => providers::run(&Ctx::load(&cli)?, cmd).await,
        Command::Trackers { cmd } => providers::trackers(&Ctx::load(&cli)?, cmd).await,
        Command::Serve => misc::unavailable("serve", "M7"),
        Command::Daemon { cmd } => daemon::run(&Ctx::load(&cli)?, cmd).await,
        Command::Add(args) => add::run(&Ctx::load(&cli)?, args).await,
        Command::List(args) => list::list(&Ctx::load(&cli)?, args).await,
        Command::Get { id, files, peers } => {
            list::get(&Ctx::load(&cli)?, *id, *files, *peers).await
        }
        Command::Rename { id, name, file } => {
            control::rename(&Ctx::load(&cli)?, *id, name, *file).await
        }
        Command::Preview { magnet, timeout } => {
            add::preview(&Ctx::load(&cli)?, magnet, *timeout).await
        }
        Command::Pause(a) => control::pause(&Ctx::load(&cli)?, a).await,
        Command::Resume(a) => control::resume(&Ctx::load(&cli)?, a).await,
        Command::Remove(a) => control::remove(&Ctx::load(&cli)?, a).await,
        Command::Purge(a) => control::purge(&Ctx::load(&cli)?, a).await,
        Command::Reorder(a) => control::reorder(&Ctx::load(&cli)?, a).await,
        Command::Limit(a) => misc::limit(&Ctx::load(&cli)?, a).await,
        Command::MaxConcurrent { n } => misc::max_concurrent(&Ctx::load(&cli)?, *n).await,
        Command::Schedule => misc::schedule(&Ctx::load(&cli)?).await,
        Command::Stats => misc::stats(&Ctx::load(&cli)?).await,
    }
}
