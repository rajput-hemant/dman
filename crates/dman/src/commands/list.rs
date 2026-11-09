//! `dman list` and `dman get`.

use std::collections::BTreeMap;

use dman_client::Client;
use dman_proto::method::{Event, ListParams};
use dman_proto::{Job, JobId, JobState};
use tokio::sync::broadcast::error::RecvError;

use super::print_json;
use crate::cli::ListArgs;
use crate::connect::Ctx;
use crate::error::CliError;
use crate::format;

const CLEAR_SCREEN: &str = "\x1b[2J\x1b[H";

pub async fn list(ctx: &Ctx, args: &ListArgs) -> Result<i32, CliError> {
    let states: Vec<JobState> = args.state.iter().map(|s| (*s).into()).collect();
    let client = ctx.connect().await?;
    if args.watch {
        watch(ctx, &client, &states).await
    } else {
        let jobs = client.list(ListParams { states }).await?;
        render(ctx, &jobs, false)?;
        Ok(0)
    }
}

fn render(ctx: &Ctx, jobs: &[Job], clear: bool) -> Result<(), CliError> {
    if ctx.json {
        return print_json(&jobs);
    }
    if clear {
        print!("{CLEAR_SCREEN}");
    }
    if jobs.is_empty() {
        println!("no downloads");
    } else {
        print!("{}", format::job_table(jobs, format::terminal_width()));
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

fn visible(jobs: &BTreeMap<JobId, Job>, states: &[JobState]) -> Vec<Job> {
    jobs.values()
        .filter(|j| states.is_empty() || states.contains(&j.state))
        .cloned()
        .collect()
}

async fn watch(ctx: &Ctx, client: &Client, states: &[JobState]) -> Result<i32, CliError> {
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
            render(ctx, &visible(&jobs, states), true)?;
        }
    }
}

pub async fn get(ctx: &Ctx, id: JobId, files: bool, peers: bool) -> Result<i32, CliError> {
    let client = ctx.connect().await?;
    if !files && !peers && !ctx.json {
        print!("{}", details(&client.get(id).await?));
        return Ok(0);
    }
    let d = client.detail(id).await?;
    if ctx.json {
        print_json(&d)?;
        return Ok(0);
    }
    print!("{}", details(&d.job));
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
    Ok(0)
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
    pairs.push(("added", j.added_at.to_string()));
    if let Some(c) = j.completed_at {
        pairs.push(("completed", c.to_string()));
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

    #[test]
    fn events_update_map_and_filter() {
        let mut m = BTreeMap::new();
        assert!(apply(
            &mut m,
            Event::JobsUpdated {
                jobs: vec![job(1, JobState::Active), job(2, JobState::Paused)]
            }
        ));
        assert_eq!(visible(&m, &[JobState::Paused]).len(), 1);
        assert!(apply(
            &mut m,
            Event::JobsRemoved {
                ids: vec![JobId(1)]
            }
        ));
        assert_eq!(visible(&m, &[]).len(), 1);
        assert!(!apply(
            &mut m,
            Event::Stats {
                stats: Default::default()
            }
        ));
    }
}
