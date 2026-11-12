//! `dman list` and `dman get`.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use dman_client::Client;
use dman_proto::method::{Event, ListParams};
use dman_proto::{Engine, Job, JobId, JobState};
use tokio::sync::broadcast::error::RecvError;

use super::print_json;
use super::progress::Renderer;
use super::wait::track;
use crate::cli::{EngineArg, ListArgs, SortKey};
use crate::connect::Ctx;
use crate::error::CliError;
use crate::format::{self, Column};

const CLEAR_SCREEN: &str = "\x1b[2J\x1b[H";

pub async fn list(ctx: &Ctx, args: &ListArgs) -> Result<i32, CliError> {
    let view = View::from_args(args);
    let client = ctx.connect().await?;
    if args.watch {
        watch(ctx, &client, &view).await
    } else {
        let jobs = client
            .list(ListParams {
                states: view.states.clone(),
            })
            .await?;
        render(ctx, &view.apply(jobs), &view.columns, false)?;
        Ok(0)
    }
}

/// Client-side filtering, sorting and limiting of the job list.
struct View {
    states: Vec<JobState>,
    name: Option<String>,
    engine: Option<Engine>,
    sort: Option<SortKey>,
    reverse: bool,
    limit: Option<usize>,
    columns: Vec<Column>,
}

impl View {
    fn from_args(args: &ListArgs) -> Self {
        Self {
            states: args.state.iter().map(|s| (*s).into()).collect(),
            name: args.name.as_ref().map(|n| n.to_lowercase()),
            engine: args.engine.map(|e| match e {
                EngineArg::Aria2 => Engine::Aria2,
                EngineArg::Ytdlp => Engine::YtDlp,
            }),
            sort: args.sort,
            reverse: args.reverse,
            limit: args.limit,
            columns: args.columns.clone(),
        }
    }

    fn matches(&self, j: &Job) -> bool {
        (self.states.is_empty() || self.states.contains(&j.state))
            && self.engine.is_none_or(|e| j.engine == e)
            && self
                .name
                .as_ref()
                .is_none_or(|n| j.name.to_lowercase().contains(n))
    }

    /// Filter, sort and limit `jobs`.
    fn apply(&self, jobs: Vec<Job>) -> Vec<Job> {
        let mut out: Vec<Job> = jobs.into_iter().filter(|j| self.matches(j)).collect();
        if let Some(key) = self.sort {
            out.sort_by(|a, b| compare(key, a, b));
            if self.reverse {
                out.reverse();
            }
        } else if self.reverse {
            out.reverse();
        }
        if let Some(n) = self.limit {
            out.truncate(n);
        }
        out
    }
}

/// Natural order of each key: biggest or newest first, soonest ETA first (unknown
/// last), names A-Z. `--reverse` flips it.
fn compare(key: SortKey, a: &Job, b: &Job) -> Ordering {
    let by = match key {
        SortKey::Speed => b.download_speed.cmp(&a.download_speed),
        SortKey::Size => b.total_bytes.cmp(&a.total_bytes),
        SortKey::Added => b.added_at.cmp(&a.added_at),
        SortKey::Progress => b.progress().total_cmp(&a.progress()),
        SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        SortKey::Eta => match (a.eta_secs(), b.eta_secs()) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
    };
    by.then(a.id.cmp(&b.id))
}

fn render(ctx: &Ctx, jobs: &[Job], columns: &[Column], watching: bool) -> Result<(), CliError> {
    if ctx.json {
        // Watching streams one compact document per update (JSON lines).
        if watching {
            println!("{}", serde_json::to_string(&jobs)?);
            return Ok(());
        }
        return print_json(&jobs);
    }
    // Redraw in place only on a terminal; a pipe gets the plain tables one after another.
    if watching && std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        print!("{CLEAR_SCREEN}");
    }
    if jobs.is_empty() {
        println!("no downloads");
    } else {
        print!(
            "{}",
            format::job_table_with(jobs, format::terminal_width(), columns)
        );
    }
    Ok(())
}

/// Apply one pushed event to the local job map; returns whether it changed.
fn apply(jobs: &mut BTreeMap<JobId, Job>, event: Event) -> bool {
    match event {
        Event::Snapshot { jobs: all, .. } => {
            *jobs = all.into_iter().map(|j| (j.id, j)).collect();
        }
        Event::JobsUpdated { jobs: changed } => {
            jobs.extend(changed.into_iter().map(|j| (j.id, j)));
        }
        Event::JobsRemoved { ids } => {
            for id in ids {
                jobs.remove(&id);
            }
        }
        _ => return false,
    }
    true
}

async fn watch(ctx: &Ctx, client: &Client, view: &View) -> Result<i32, CliError> {
    let mut rx = client.subscribe().await?;
    let mut jobs = BTreeMap::new();
    loop {
        let changed = tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => apply(&mut jobs, ev),
                Err(RecvError::Lagged(_)) => {
                    jobs = client.list(ListParams::default()).await?
                        .into_iter().map(|j| (j.id, j)).collect();
                    true
                }
                Err(RecvError::Closed) => return Err(dman_client::ClientError::Disconnected.into()),
            },
            () = client.closed() => return Err(dman_client::ClientError::Disconnected.into()),
        };
        if changed {
            render(
                ctx,
                &view.apply(jobs.values().cloned().collect()),
                &view.columns,
                true,
            )?;
        }
    }
}

pub async fn get(
    ctx: &Ctx,
    id: JobId,
    files: bool,
    peers: bool,
    watch: bool,
) -> Result<i32, CliError> {
    let client = ctx.connect().await?;
    let mut code = 0;
    if watch {
        // The bar goes to stderr; the details below still go to stdout afterwards.
        let renderer = (!ctx.json).then(|| Renderer::new(true));
        let tracked = track(&client, &[id], None, renderer).await?;
        code = tracked.exit_code();
    }
    let d = client.detail(id).await?;
    if ctx.json {
        print_json(&d)?;
        return Ok(code);
    }
    print!("{}", details(&d.job));
    if !d.renames.is_empty() {
        println!();
        println!("renames:");
        for r in &d.renames {
            println!(
                "  {} -> {}  ({})",
                format::printable(&r.from),
                format::printable(&r.to),
                format::local_time(r.at)
            );
        }
    }
    if files {
        println!();
        if d.files.is_empty() {
            println!("no files known");
        } else {
            print!("{}", format::file_table(&d.files));
        }
    }
    if peers {
        println!();
        if d.peers.is_empty() {
            println!("no peers");
        } else {
            print!("{}", format::peer_table(&d.peers));
        }
    }
    Ok(code)
}

fn details(j: &Job) -> String {
    let mut pairs = vec![
        ("id", j.id.to_string()),
        ("name", j.name.clone()),
        ("state", format::state(j.state).to_owned()),
        ("engine", format!("{:?}", j.engine).to_lowercase()),
        ("kind", format!("{:?}", j.kind).to_lowercase()),
        ("source", j.source.clone()),
        ("dir", j.dir.clone()),
        ("progress", format::progress(j)),
        (
            "size",
            format!(
                "{} of {}",
                format::size(j.completed_bytes),
                format::size(j.total_bytes)
            ),
        ),
        ("download", format::speed(j.download_speed)),
        ("upload", format::speed(j.upload_speed)),
        ("uploaded", format::size(j.uploaded_bytes)),
        ("eta", format::eta(j.eta_secs())),
        ("connections", j.connections.to_string()),
    ];
    if let Some(s) = j.seeders {
        pairs.push(("seeders", s.to_string()));
    }
    if let Some(h) = &j.info_hash {
        pairs.push(("info hash", h.clone()));
    }
    if let Some(p) = j.queue_position {
        pairs.push(("queue", p.to_string()));
    }
    let now = jiff::Timestamp::now().as_second();
    pairs.push(("added", crate::format::local_time_ago(j.added_at, now)));
    if let Some(c) = j.completed_at {
        pairs.push(("completed", crate::format::local_time_ago(c, now)));
    }
    if let Some(e) = &j.error {
        pairs.push(("error", e.clone()));
    }
    format::key_values(&pairs)
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
            name: "n".into(),
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

    fn view() -> View {
        View {
            states: vec![],
            name: None,
            engine: None,
            sort: None,
            reverse: false,
            limit: None,
            columns: vec![],
        }
    }

    fn named(id: u64, name: &str, speed: u64, total: u64, done: u64) -> Job {
        Job {
            name: name.into(),
            download_speed: speed,
            total_bytes: total,
            completed_bytes: done,
            added_at: id as i64,
            ..job(id, JobState::Active)
        }
    }

    fn ids(jobs: &[Job]) -> Vec<u64> {
        jobs.iter().map(|j| j.id.0).collect()
    }

    #[test]
    fn name_filter_is_case_insensitive_and_engine_filters() {
        let mut y = named(3, "Clip", 0, 0, 0);
        y.engine = Engine::YtDlp;
        let jobs = vec![
            named(1, "Ubuntu ISO", 0, 0, 0),
            named(2, "debian", 0, 0, 0),
            y,
        ];
        let v = View {
            name: Some("ubu".into()),
            ..view()
        };
        assert_eq!(ids(&v.apply(jobs.clone())), vec![1]);
        let v = View {
            engine: Some(Engine::YtDlp),
            ..view()
        };
        assert_eq!(ids(&v.apply(jobs)), vec![3]);
    }

    #[test]
    fn sorting_limit_and_reverse() {
        let jobs = vec![
            named(1, "b", 10, 300, 30),
            named(2, "a", 30, 100, 90),
            named(3, "c", 0, 200, 20),
        ];
        let sorted = |key, reverse, limit| {
            ids(&View {
                sort: Some(key),
                reverse,
                limit,
                ..view()
            }
            .apply(jobs.clone()))
        };
        assert_eq!(sorted(SortKey::Speed, false, None), vec![2, 1, 3]);
        assert_eq!(sorted(SortKey::Speed, true, None), vec![3, 1, 2]);
        assert_eq!(sorted(SortKey::Size, false, None), vec![1, 3, 2]);
        assert_eq!(sorted(SortKey::Name, false, None), vec![2, 1, 3]);
        assert_eq!(sorted(SortKey::Added, false, None), vec![3, 2, 1]);
        assert_eq!(sorted(SortKey::Progress, false, None), vec![2, 1, 3]);
        // Soonest ETA first; the job with no speed (unknown ETA) last.
        assert_eq!(sorted(SortKey::Eta, false, None), vec![2, 1, 3]);
        assert_eq!(sorted(SortKey::Speed, false, Some(2)), vec![2, 1]);
    }

    #[test]
    fn events_update_map_and_filter() {
        let mut m = BTreeMap::new();
        assert!(apply(
            &mut m,
            Event::JobsUpdated {
                jobs: vec![job(1, JobState::Active), job(2, JobState::Paused)]
            }
        ));
        let paused = View {
            states: vec![JobState::Paused],
            ..view()
        };
        assert_eq!(paused.apply(m.values().cloned().collect()).len(), 1);
        assert!(apply(
            &mut m,
            Event::JobsRemoved {
                ids: vec![JobId(1)]
            }
        ));
        assert_eq!(view().apply(m.values().cloned().collect()).len(), 1);
        assert!(!apply(
            &mut m,
            Event::Stats {
                stats: Default::default()
            }
        ));
    }
}
