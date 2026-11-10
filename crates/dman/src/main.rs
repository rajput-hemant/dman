//! dman: terminal download manager.

mod cli;
mod commands;
mod completion;
mod connect;
mod error;
mod format;

use clap::FromArgMatches as _;
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
    // A prompt blocks the thread that would notice Ctrl-C below, and once a signal
    // handler is installed the terminal's default "kill the process" no longer
    // applies. This watcher runs on a worker thread and ends the process if the
    // orderly path did not finish within a moment.
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|e| CliError::general(format!("cannot watch for Ctrl-C: {e}")))?;
    tokio::spawn(async move {
        if sigint.recv().await.is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            std::process::exit(error::EXIT_INTERRUPTED);
        }
    });
    tokio::select! {
        res = commands::dispatch(cli) => res,
        _ = tokio::signal::ctrl_c() => Err(CliError::interrupted()),
    }
}

/// Exit quietly when the reader of our output went away (`dman list | head`).
///
/// Rust ignores SIGPIPE, so a write to a closed pipe fails with EPIPE and `println!`
/// panics with "failed printing to stdout: Broken pipe". That is a normal way for a
/// pipeline to end, not an error. (The daemon is exempt: it must not die with its log pipe.)
fn exit_quietly_on_broken_pipe() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info.to_string();
        if msg.contains("failed printing to std") && msg.contains("Broken pipe") {
            std::process::exit(0);
        }
        default(info);
    }));
}

/// Ends the process for a command line clap rejected (or for `--help`/`--version`).
/// With `--json` a usage error is a JSON object on stderr like every other error.
fn usage_error(e: &clap::Error) -> ! {
    let json = std::env::args()
        .take_while(|a| a != "--")
        .any(|a| a == "--json");
    if e.use_stderr() && json {
        let rendered = e.render().to_string();
        let message = rendered
            .lines()
            .next()
            .map_or("usage error", |l| l.trim_start_matches("error: "))
            .to_owned();
        report(&CliError::invalid(message), true);
        std::process::exit(e.exit_code());
    }
    e.exit()
}

fn main() -> ExitCode {
    // Shell completion requests (`COMPLETE=<shell> dman -- ...`) are answered here and exit.
    completion::handle_env_request();
    let cli = match cli::command()
        .try_get_matches()
        .and_then(|m| Cli::from_arg_matches(&m))
    {
        Ok(cli) => cli,
        Err(e) => usage_error(&e),
    };
    let json = cli.json;
    let is_daemon_run = matches!(
        cli.command,
        Some(Command::Daemon {
            cmd: DaemonCmd::Run { .. }
        })
    );
    if !is_daemon_run {
        exit_quietly_on_broken_pipe();
    }
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
