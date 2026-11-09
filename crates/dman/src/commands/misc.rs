//! limit, max-concurrent, stats, completions and placeholders.

use clap::CommandFactory;
use clap_complete::Shell;
use dman_proto::method::LimitsUpdate;

use super::print_json;
use crate::cli::{Cli, ConfigCmd, LimitArgs};
use crate::connect::Ctx;
use crate::error::CliError;
use crate::format;

pub fn completions(shell: Shell) -> Result<i32, CliError> {
    clap_complete::generate(shell, &mut Cli::command(), "dman", &mut std::io::stdout());
    Ok(0)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn print_limits(
    ctx: &Ctx,
    l: &dman_proto::Limits,
    s: Option<&dman_proto::method::ScheduleStatus>,
) -> Result<i32, CliError> {
    if ctx.json {
        print_json(l)?;
    } else {
        print!(
            "{}",
            format::key_values(&format::limit_lines(l, s, now_secs()))
        );
    }
    Ok(0)
}

pub async fn limit(ctx: &Ctx, a: &LimitArgs) -> Result<i32, CliError> {
    let client = ctx.connect().await?;
    let limits = if a.download.is_none() && a.upload.is_none() {
        client.limits_get().await?
    } else {
        client
            .limits_set(LimitsUpdate {
                download: a.download,
                upload: a.upload,
                max_concurrent: None,
            })
            .await?
    };
    // The schedule is informational: failures to read it are ignored.
    let sched = client.schedule().await.ok();
    print_limits(ctx, &limits, sched.as_ref())
}

pub async fn max_concurrent(ctx: &Ctx, n: Option<u32>) -> Result<i32, CliError> {
    let client = ctx.connect().await?;
    let limits = match n {
        None => client.limits_get().await?,
        Some(n) => {
            client
                .limits_set(LimitsUpdate {
                    max_concurrent: Some(n),
                    ..LimitsUpdate::default()
                })
                .await?
        }
    };
    if ctx.json {
        print_json(&limits)?;
    } else {
        println!("{}", limits.max_concurrent);
    }
    Ok(0)
}

pub fn stats_text(s: &dman_proto::GlobalStats) -> String {
    let mut pairs = vec![
        ("download", format::speed(s.download_speed)),
        ("upload", format::speed(s.upload_speed)),
        ("active", s.num_active.to_string()),
        ("waiting", s.num_waiting.to_string()),
        ("stopped", s.num_stopped.to_string()),
    ];
    if let Some(free) = s.disk_free {
        pairs.push(("disk free", format::size(free)));
    }
    pairs.push(("total downloaded", format::size(s.total_downloaded)));
    pairs.push(("total uploaded", format::size(s.total_uploaded)));
    format::key_values(&pairs)
}

pub async fn stats(ctx: &Ctx) -> Result<i32, CliError> {
    let client = ctx.connect().await?;
    let s = client.stats().await?;
    if ctx.json {
        print_json(&s)?;
    } else {
        print!("{}", stats_text(&s));
        if s.schedule.as_ref().is_some_and(|b| b.active.is_some()) {
            // Which limits the active window overrides (informational).
            if let (Ok(l), Ok(sc)) = (client.limits_get().await, client.schedule().await) {
                let lines: Vec<(String, String)> = format::limit_lines(&l, Some(&sc), now_secs())
                    .into_iter()
                    .filter(|(_, v)| v.contains("window '"))
                    .map(|(k, v)| (format!("{k} limit"), v))
                    .collect();
                let pairs: Vec<(&str, String)> =
                    lines.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
                print!("{}", format::key_values(&pairs));
            }
        }
    }
    Ok(0)
}

pub async fn schedule(ctx: &Ctx) -> Result<i32, CliError> {
    let s = ctx.connect().await?.schedule().await?;
    if ctx.json {
        print_json(&s)?;
    } else {
        print!("{}", format::schedule_text(&s, now_secs()));
    }
    Ok(0)
}

pub async fn config(ctx: &Ctx, cmd: &ConfigCmd) -> Result<i32, CliError> {
    match cmd {
        ConfigCmd::Reload => {
            let r = ctx.connect().await?.config_reload().await?;
            if ctx.json {
                print_json(&r)?;
            } else {
                print!("{}", format::reload_text(&r));
            }
            Ok(0)
        }
    }
}
