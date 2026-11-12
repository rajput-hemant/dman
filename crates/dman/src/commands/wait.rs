//! `dman wait`, and the tracking loop shared with `add --wait` and `get --watch`.

use std::collections::BTreeMap;
use std::time::Duration;

use dman_client::{Client, ClientError};
use dman_proto::method::{Event, ListParams};
use dman_proto::{ErrorCode, Job, JobId, JobState};
use tokio::sync::broadcast::error::RecvError;
use tokio::time::Instant;

use super::print_json;
use super::progress::Renderer;
use crate::cli::WaitArgs;
use crate::connect::Ctx;
use crate::error::{CliError, EXIT_TIMEOUT, batch_exit};
use crate::format;

/// More jobs than this are summarised on one line instead of one line each, so the
/// in-place redraw always fits on the screen.
const MAX_ROWS: usize = 8;

/// Parse `90`, `90s`, `15m`, `2h`, `1d` or a combination such as `1h30m`.
/// A bare number is seconds; zero is refused.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let bad = || format!("invalid duration '{s}' (examples: 90s, 15m, 2h, 1h30m)");
    let t = s.trim();
    if t.is_empty() {
        return Err(bad());
    }
    let mut total: u64 = 0;
    let mut digits = String::new();
    for c in t.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let unit: u64 = match c.to_ascii_lowercase() {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return Err(bad()),
        };
        let n: u64 = digits.parse().map_err(|_| bad())?;
        digits.clear();
        total = n
            .checked_mul(unit)
            .and_then(|v| total.checked_add(v))
            .ok_or_else(bad)?;
    }
    if !digits.is_empty() {
        // A trailing bare number: seconds (`90`), but not after units (`1h30`).
        if total != 0 {
            return Err(bad());
        }
        total = digits.parse().map_err(|_| bad())?;
    }
    if total == 0 {
        return Err(format!("the duration '{s}' must be greater than zero"));
    }
    Ok(Duration::from_secs(total))
}

/// A job whose download will not change any more. A torrent that finished and now
/// seeds counts: waiting for the end of seeding would never return for most users.
pub fn is_final(state: JobState) -> bool {
    matches!(
        state,
        JobState::Complete | JobState::Seeding | JobState::Error | JobState::Removed
    )
}

fn is_failed(state: JobState) -> bool {
    matches!(state, JobState::Error | JobState::Removed)
}

/// The tracked jobs as last seen.
pub struct Tracked {
    pub jobs: Vec<Job>,
    pub timed_out: bool,
}

impl Tracked {
    /// 0 when every job completed, 124 on timeout, else the batch failure code
    /// (1 when all failed, 8 when only some did).
    pub fn exit_code(&self) -> i32 {
        if self.timed_out {
            return EXIT_TIMEOUT;
        }
        let failed = self.jobs.iter().filter(|j| is_failed(j.state)).count();
        batch_exit(self.jobs.len(), failed, Some(ErrorCode::General))
    }
}

fn disconnected() -> CliError {
    ClientError::Disconnected.into()
}

async fn refresh(
    client: &Client,
    jobs: &mut BTreeMap<JobId, Job>,
    ids: &[JobId],
) -> Result<(), CliError> {
    let all = client.list(ListParams::default()).await?;
    for j in all {
        if ids.contains(&j.id) {
            jobs.insert(j.id, j);
        }
    }
    Ok(())
}

/// Apply a pushed event to the tracked jobs.
fn apply(jobs: &mut BTreeMap<JobId, Job>, ids: &[JobId], event: Event) {
    match event {
        Event::Snapshot { jobs: all, .. } => {
            let present: Vec<JobId> = all.iter().map(|j| j.id).collect();
            for j in all {
                if ids.contains(&j.id) {
                    jobs.insert(j.id, j);
                }
            }
            for (id, j) in jobs.iter_mut() {
                if !present.contains(id) {
                    j.state = JobState::Removed;
                }
            }
        }
        Event::JobsUpdated { jobs: changed } => {
            for j in changed {
                if ids.contains(&j.id) {
                    jobs.insert(j.id, j);
                }
            }
        }
        Event::JobsRemoved { ids: gone } => {
            for id in gone {
                if let Some(j) = jobs.get_mut(&id) {
                    j.state = JobState::Removed;
                }
            }
        }
        _ => {}
    }
}

/// The rows to draw: every job, or the unfinished ones plus a summary line when
/// there are many.
fn rows(jobs: &BTreeMap<JobId, Job>) -> (Vec<&Job>, Option<String>) {
    let all: Vec<&Job> = jobs.values().collect();
    if all.len() <= MAX_ROWS {
        return (all, None);
    }
    let done = all.iter().filter(|j| is_final(j.state)).count();
    let mut shown: Vec<&Job> = all
        .iter()
        .copied()
        .filter(|j| !is_final(j.state))
        .take(MAX_ROWS - 1)
        .collect();
    if shown.is_empty() {
        shown = all.iter().copied().take(MAX_ROWS - 1).collect();
    }
    let footer = format!("{done} of {} finished", all.len());
    (shown, Some(footer))
}

/// Block until every job in `ids` is final (or `timeout` passes), drawing progress
/// with `renderer` when given. Uses the daemon's event subscription.
pub async fn track(
    client: &Client,
    ids: &[JobId],
    timeout: Option<Duration>,
    mut renderer: Option<Renderer>,
) -> Result<Tracked, CliError> {
    // Subscribe first so no update between the listing and the loop is missed.
    let mut rx = client.subscribe().await?;
    let mut jobs = BTreeMap::new();
    refresh(client, &mut jobs, ids).await?;
    if let Some(missing) = ids.iter().find(|id| !jobs.contains_key(id)) {
        return Err(CliError::new(
            ErrorCode::NotFound,
            format!("no download with ID {missing} (see `dman list`)"),
        ));
    }
    let deadline = timeout.map(|t| Instant::now() + t);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut timed_out = false;
    loop {
        let finished = jobs.values().all(|j| is_final(j.state));
        if let Some(r) = renderer.as_mut() {
            let (shown, footer) = rows(&jobs);
            r.draw(&shown, footer.as_deref(), finished);
        }
        if finished {
            break;
        }
        let expired = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => apply(&mut jobs, ids, ev),
                Err(RecvError::Lagged(_)) => refresh(client, &mut jobs, ids).await?,
                Err(RecvError::Closed) => return Err(disconnected()),
            },
            () = client.closed() => return Err(disconnected()),
            _ = tick.tick() => {}
            () = expired => {
                timed_out = true;
                if let Some(r) = renderer.as_mut() {
                    let (shown, footer) = rows(&jobs);
                    r.draw(&shown, footer.as_deref(), true);
                }
                break;
            }
        }
    }
    Ok(Tracked {
        jobs: jobs.into_values().collect(),
        timed_out,
    })
}

/// Print how the wait ended; returns the exit code.
pub fn conclude(ctx: &Ctx, tracked: &Tracked, timeout: Option<Duration>) -> Result<i32, CliError> {
    let code = tracked.exit_code();
    if ctx.json {
        print_json(&serde_json::json!({
            "jobs": tracked.jobs,
            "timed_out": tracked.timed_out,
        }))?;
        return Ok(code);
    }
    for j in &tracked.jobs {
        println!(
            "{}  {}  {}",
            j.id,
            format::state(j.state),
            format::printable(&j.name)
        );
    }
    if tracked.timed_out {
        let left = tracked.jobs.iter().filter(|j| !is_final(j.state)).count();
        let after = timeout.map_or_else(String::new, |t| {
            format!(" after {}", format::eta(Some(t.as_secs())))
        });
        eprintln!(
            "dman: timed out{after}; {left} download(s) still unfinished (they keep running)"
        );
    }
    for j in tracked.jobs.iter().filter(|j| j.state == JobState::Error) {
        eprintln!(
            "dman: #{} failed: {}",
            j.id,
            format::printable(j.error.as_deref().unwrap_or("unknown error"))
        );
    }
    Ok(code)
}

pub async fn run(ctx: &Ctx, args: &WaitArgs) -> Result<i32, CliError> {
    let mut ids = args.ids.clone();
    ids.sort_unstable();
    ids.dedup();
    let client = ctx.connect().await?;
    let renderer = (!ctx.json).then(|| Renderer::new(false));
    let tracked = track(&client, &ids, args.timeout, renderer).await?;
    conclude(ctx, &tracked, args.timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dman_proto::{Engine, SourceKind};

    fn job(id: u64, state: JobState) -> Job {
        Job {
            id: JobId(id),
            engine: Engine::Aria2,
            kind: SourceKind::Http,
            name: format!("job{id}"),
            state,
            dir: "/".into(),
            source: "s".into(),
            total_bytes: 0,
            completed_bytes: 0,
            uploaded_bytes: 0,
            download_speed: 0,
            upload_speed: 0,
            connections: 0,
            seeders: None,
            info_hash: None,
            added_at: 0,
            completed_at: None,
            queue_position: None,
            error: None,
        }
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("15m").unwrap(), Duration::from_secs(900));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86_400));
        for bad in ["", "m", "0", "0s", "1h30", "abc", "-5", "1x", "1.5h"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn final_states() {
        assert!(is_final(JobState::Complete));
        assert!(is_final(JobState::Seeding));
        assert!(is_final(JobState::Error));
        assert!(!is_final(JobState::Active));
        assert!(!is_final(JobState::Paused));
        assert!(!is_final(JobState::Waiting));
    }

    #[test]
    fn exit_codes_by_outcome() {
        let t = |states: &[JobState], timed_out| Tracked {
            jobs: states
                .iter()
                .enumerate()
                .map(|(i, s)| job(i as u64 + 1, *s))
                .collect(),
            timed_out,
        };
        use JobState::*;
        assert_eq!(t(&[Complete, Seeding], false).exit_code(), 0);
        assert_eq!(t(&[Complete, Error], false).exit_code(), 8);
        assert_eq!(t(&[Error, Removed], false).exit_code(), 1);
        assert_eq!(t(&[Active], true).exit_code(), EXIT_TIMEOUT);
    }

    #[test]
    fn events_update_only_tracked_jobs() {
        let ids = [JobId(1), JobId(2)];
        let mut m = BTreeMap::new();
        apply(
            &mut m,
            &ids,
            Event::JobsUpdated {
                jobs: vec![job(1, JobState::Active), job(3, JobState::Active)],
            },
        );
        assert_eq!(m.len(), 1);
        apply(
            &mut m,
            &ids,
            Event::JobsUpdated {
                jobs: vec![job(1, JobState::Complete)],
            },
        );
        assert_eq!(m[&JobId(1)].state, JobState::Complete);
        apply(
            &mut m,
            &ids,
            Event::JobsRemoved {
                ids: vec![JobId(1)],
            },
        );
        assert_eq!(m[&JobId(1)].state, JobState::Removed);
    }

    #[test]
    fn many_jobs_are_summarised() {
        let m: BTreeMap<JobId, Job> = (1..=12)
            .map(|i| {
                let s = if i <= 5 {
                    JobState::Complete
                } else {
                    JobState::Active
                };
                (JobId(i), job(i, s))
            })
            .collect();
        let (shown, footer) = rows(&m);
        assert_eq!(shown.len(), MAX_ROWS - 1);
        assert!(shown.iter().all(|j| !is_final(j.state)));
        assert_eq!(footer.as_deref(), Some("5 of 12 finished"));
    }
}
