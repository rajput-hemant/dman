//! dman: terminal download manager.

mod cli;
mod commands;
mod connect;
mod error;
mod format;

use clap::Parser;
use cli::{Cli, Command, DaemonCmd};
use error::CliError;
use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

fn init_tracing(cli: &Cli, is_daemon: bool) {
    let default = match (cli.verbose, is_daemon) {
        (0, true) | (1, false) => "info",
        (0, false) => "warn",
        (1, true) | (2, _) => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .try_init();
}

fn report(err: &CliError, json: bool) {
    if err.message.is_empty() {
        return;
    }
    if json {
        let code = serde_json::to_value(err.code).unwrap_or_default();
        let body = serde_json::json!({ "error": { "code": code, "message": err.message } });
        eprintln!("{body}");
    } else {
        eprintln!("dman: {}", err.message);
    }
}

async fn run(cli: Cli, is_daemon_run: bool) -> Result<i32, CliError> {
    if is_daemon_run || cli.command.is_none() {
        // The daemon handles SIGINT/SIGTERM itself so it can flush state, and
        // the TUI reads Ctrl-C as a key and restores the terminal itself.
        return commands::dispatch(cli).await;
    }
    tokio::select! {
        res = commands::dispatch(cli) => res,
        _ = tokio::signal::ctrl_c() => Err(CliError::interrupted()),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    let is_daemon_run = matches!(
        cli.command,
        Some(Command::Daemon {
            cmd: DaemonCmd::Run { .. }
        })
    );
    // Log output would corrupt the full-screen UI, so the TUI runs without it.
    let is_tui =
        cli.command.is_none() && !cli.json && std::io::IsTerminal::is_terminal(&std::io::stdout());
    if !is_tui {
        init_tracing(&cli, is_daemon_run);
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            report(
                &CliError::general(format!("cannot start runtime: {e}")),
                json,
            );
            return ExitCode::from(1);
        }
    };
    let code = match runtime.block_on(run(cli, is_daemon_run)) {
        Ok(code) => code,
        Err(e) => {
            report(&e, json);
            e.exit
        }
    };
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
