//! `dman daemon ...` and the bare `dman` entry point.

use std::fs::OpenOptions;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use dman_client::Client;
use dman_config::Config;

use super::print_json;
use crate::cli::DaemonCmd;
use crate::connect::Ctx;
use crate::error::CliError;

const WAIT_LIMIT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(100);
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);
const LOG_TAIL_LINES: usize = 15;

pub async fn run(ctx: &Ctx, cmd: &DaemonCmd) -> Result<i32, CliError> {
    match cmd {
        DaemonCmd::Run { detach } => run_foreground(ctx, *detach).await,
        DaemonCmd::Start => start(ctx).await,
        DaemonCmd::Stop => stop(ctx).await,
        DaemonCmd::Restart => restart(ctx).await,
        DaemonCmd::Status => status(ctx).await,
        DaemonCmd::GenConfig => gen_config(ctx),
    }
}

fn say(ctx: &Ctx, human: &str, status: &str) {
    if ctx.json {
        println!("{}", serde_json::json!({ "status": status }));
    } else {
        println!("{human}");
    }
}

/// Bare `dman`: make sure a daemon is up, then show the placeholder message.
pub async fn bare(ctx: &Ctx) -> Result<i32, CliError> {
    let client = match ctx.connect().await {
        Ok(c) => c,
        Err(e) if Ctx::is_not_running(&e) && ctx.is_local() => spawn_and_wait(ctx).await?,
        Err(e) => return Err(e),
    };
    let version = client.daemon_version();
    if ctx.json {
        print_json(&serde_json::json!({ "daemon_version": version }))?;
    } else {
        println!(
            "connected to daemon {version} \u{2014} the interactive UI arrives in milestone M2; try `dman list`"
        );
    }
    Ok(0)
}

async fn run_foreground(ctx: &Ctx, detach: bool) -> Result<i32, CliError> {
    ctx.require_local("`daemon run`")?;
    if detach && let Err(e) = nix::unistd::setsid() {
        tracing::warn!("setsid failed: {e}");
    }
    let opts = dman_daemon::DaemonOptions {
        paths: ctx.paths.clone(),
        config: ctx.config.clone(),
        profile: ctx.name.clone(),
    };
    dman_daemon::run(opts)
        .await
        .map_err(|e| CliError::general(format!("{e:#}")))?;
    Ok(0)
}

async fn start(ctx: &Ctx) -> Result<i32, CliError> {
    ctx.require_local("`daemon start`")?;
    match ctx.connect().await {
        Ok(_) => {
            say(ctx, "already running", "already_running");
            return Ok(0);
        }
        Err(e) if Ctx::is_not_running(&e) => {}
        Err(e) => return Err(e),
    }
    let client = spawn_and_wait(ctx).await?;
    say(
        ctx,
        &format!("started daemon {}", client.daemon_version()),
        "started",
    );
    Ok(0)
}

fn spawn_daemon(ctx: &Ctx) -> Result<Child, CliError> {
    let log = ctx.paths.log_file();
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let out = OpenOptions::new().create(true).append(true).open(&log)?;
    let err = out.try_clone()?;
    std::process::Command::new(std::env::current_exe()?)
        .args(["daemon", "run", "--detach", "--profile", &ctx.name])
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .map_err(|e| CliError::general(format!("cannot start daemon: {e}")))
}

fn log_tail(ctx: &Ctx) -> String {
    let text = std::fs::read_to_string(ctx.paths.log_file()).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(LOG_TAIL_LINES)..].join("\n")
}

fn start_failure(ctx: &Ctx, reason: &str) -> CliError {
    CliError::unreachable(format!(
        "{reason}; last lines of {}:\n{}",
        ctx.paths.log_file().display(),
        log_tail(ctx)
    ))
}

/// Spawn a detached daemon and poll until it accepts connections.
async fn spawn_and_wait(ctx: &Ctx) -> Result<Client, CliError> {
    let mut child = spawn_daemon(ctx)?;
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        match tokio::time::timeout(ATTEMPT_TIMEOUT, ctx.connect()).await {
            Ok(Ok(client)) => return Ok(client),
            Ok(Err(e)) if !Ctx::is_not_running(&e) => return Err(e),
            _ => {}
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(start_failure(
                ctx,
                &format!("daemon exited early ({status})"),
            ));
        }
        if Instant::now() >= deadline {
            return Err(start_failure(ctx, "daemon did not start within 15 s"));
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn stop(ctx: &Ctx) -> Result<i32, CliError> {
    let client = match ctx.connect().await {
        Ok(c) => c,
        Err(e) if Ctx::is_not_running(&e) => {
            say(ctx, "daemon not running", "not_running");
            return Ok(0);
        }
        Err(e) => return Err(e),
    };
    // The daemon may close the socket before it answers.
    if let Err(e) = client.shutdown().await
        && !matches!(e, dman_client::ClientError::Disconnected)
    {
        return Err(e.into());
    }
    drop(client);
    let deadline = Instant::now() + WAIT_LIMIT;
    while ctx.connect().await.is_ok() {
        if Instant::now() >= deadline {
            return Err(CliError::general("daemon did not stop within 15 s"));
        }
        tokio::time::sleep(POLL).await;
    }
    // The API closes before the engine has stopped; a local daemon holds its
    // lock until everything (including aria2c) is shut down.
    if ctx.is_local() {
        while lock_held(&ctx.paths.lock_file(&ctx.name)) {
            if Instant::now() >= deadline {
                return Err(CliError::general("daemon did not stop within 15 s"));
            }
            tokio::time::sleep(POLL).await;
        }
    }
    say(ctx, "stopped", "stopped");
    Ok(0)
}

/// Whether another process holds the daemon's single-instance lock.
fn lock_held(path: &std::path::Path) -> bool {
    match std::fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        Err(_) => false,
    }
}

async fn restart(ctx: &Ctx) -> Result<i32, CliError> {
    ctx.require_local("`daemon restart`")?;
    stop(ctx).await?;
    start(ctx).await
}

async fn status(ctx: &Ctx) -> Result<i32, CliError> {
    let client = ctx.connect().await?;
    let engines = client.engine_status().await?;
    let stats = client.stats().await?;
    if ctx.json {
        print_json(&serde_json::json!({
            "running": true,
            "daemon_version": client.daemon_version(),
            "profile": client.profile(),
            "engines": engines,
            "stats": stats,
        }))?;
        return Ok(0);
    }
    println!(
        "daemon {} running (profile {})",
        client.daemon_version(),
        client.profile()
    );
    for e in &engines {
        println!("{}", engine_line(e));
    }
    print!("{}", super::misc::stats_text(&stats));
    Ok(0)
}

fn engine_line(e: &dman_proto::method::EngineStatus) -> String {
    let name = format!("{:?}", e.engine).to_lowercase();
    if e.available {
        format!(
            "engine {name}: available {}",
            e.version.as_deref().unwrap_or("")
        )
        .trim_end()
        .to_owned()
    } else {
        format!(
            "engine {name}: unavailable ({})",
            e.reason.as_deref().unwrap_or("unknown reason")
        )
    }
}

fn gen_config(ctx: &Ctx) -> Result<i32, CliError> {
    let created = Config::write_default(&ctx.paths)?;
    let path = ctx.paths.config_file();
    if ctx.json {
        print_json(&serde_json::json!({ "path": path, "created": created }))?;
    } else if created {
        println!("created {}", path.display());
    } else {
        println!("{} already exists", path.display());
    }
    Ok(0)
}
