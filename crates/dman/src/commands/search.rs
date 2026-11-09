//! `dman search`.

use std::time::Duration;

use dman_proto::method::Event;
use dman_proto::search::{
    ProviderStatus, SearchResolveParams, SearchResultDto, SearchSnapshot, SearchStartParams,
};
use dman_proto::{AddOutcome, AddParams, AddSource, ErrorCode, JobOptions};
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use super::print_json;
use crate::cli::SearchArgs;
use crate::connect::Ctx;
use crate::error::{CliError, batch_exit};
use crate::format;

/// How often the finished flag is polled in case the finishing event was missed.
const POLL: Duration = Duration::from_secs(2);

pub async fn run(ctx: &Ctx, args: &SearchArgs) -> Result<i32, CliError> {
    if args.history || args.clear_history {
        return history(ctx, args).await;
    }
    let query = args.query.join(" ");
    if query.trim().is_empty() {
        return Err(CliError::invalid("the query is empty"));
    }
    let client = ctx.connect().await?;
    // Subscribe before starting so no update of this search can be missed.
    let mut events = client.subscribe().await?;
    let started = client
        .search_start(SearchStartParams {
            query: query.clone(),
            providers: (!args.provider.is_empty()).then(|| args.provider.clone()),
            category: args.category.clone(),
        })
        .await?;
    if started.providers.is_empty() {
        return Err(CliError::refused(
            "no search provider is enabled; see `dman providers list` and `dman providers enable NAME`",
        ));
    }
    let id = started.search_id;
    let mut tick = tokio::time::interval(POLL);
    let snapshot = loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Ok(Event::SearchUpdate { search_id, provider, status, .. }) if search_id == id => {
                    progress(ctx, &provider, &status);
                }
                Ok(Event::SearchFinished { search_id }) if search_id == id => {
                    break client.search_results(id).await?;
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => {
                    return Err(CliError::unreachable("connection to the daemon was lost"));
                }
            },
            _ = tick.tick() => {
                let snap = client.search_results(id).await?;
                if snap.finished {
                    break snap;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                let _ = client.search_cancel(id).await;
                return Err(CliError::interrupted());
            }
        }
    };

    let failed: Vec<(&String, &String)> = snapshot
        .providers
        .iter()
        .filter_map(|(n, s)| match s {
            ProviderStatus::Failed { error } => Some((n, error)),
            _ => None,
        })
        .collect();
    let all_failed = failed.len() == snapshot.providers.len();
    if !ctx.json {
        for (n, e) in &failed {
            eprintln!("dman: provider {n} failed: {e}");
        }
    }
    if all_failed && snapshot.results.is_empty() {
        return Err(CliError::general(
            if ctx.json || snapshot.providers.len() > 1 {
                "every provider failed".to_owned()
            } else {
                format!("provider {} failed: {}", failed[0].0, failed[0].1)
            },
        ));
    }

    let shown = if args.limit == 0 {
        snapshot.results.len()
    } else {
        args.limit.min(snapshot.results.len())
    };
    let mut added = Vec::new();
    let mut code = 0;
    if !args.add.is_empty() {
        let (outcomes, exit) = add_results(&client, &snapshot, &args.add).await?;
        added = outcomes;
        code = exit;
    }
    if ctx.json {
        print_json(&json!({
            "search_id": id,
            "query": snapshot.query,
            "providers": snapshot.providers.iter().map(|(n, s)| json!({"name": n, "status": s})).collect::<Vec<_>>(),
            "total": snapshot.results.len(),
            "results": &snapshot.results[..shown],
            "added": added,
        }))?;
    } else {
        if snapshot.results.is_empty() {
            println!("no results for `{}`", snapshot.query);
        } else {
            print!(
                "{}",
                format::search_table(&snapshot.results[..shown], 1, format::terminal_width())
            );
            if shown < snapshot.results.len() {
                println!(
                    "... {} more (use --limit 0 to show all)",
                    snapshot.results.len() - shown
                );
            }
        }
        for a in &added {
            match a {
                Added::Ok { ids, title } => println!("added {} ({title})", ids.join(" ")),
                Added::Failed { index, error } => println!("failed #{index}: {error}"),
            }
        }
    }
    Ok(code)
}

fn progress(ctx: &Ctx, provider: &str, status: &ProviderStatus) {
    if ctx.json {
        return;
    }
    match status {
        ProviderStatus::Running => {}
        ProviderStatus::Done { count, elapsed_ms } => {
            eprintln!("{provider}: {count} results in {elapsed_ms} ms");
        }
        ProviderStatus::Failed { error } => eprintln!("{provider}: failed: {error}"),
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
enum Added {
    Ok { ids: Vec<String>, title: String },
    Failed { index: usize, error: String },
}

/// Adds the results numbered `picks` (1-based); detail-only results are
/// resolved to a magnet first. Returns one entry per pick and the exit code.
async fn add_results(
    client: &dman_client::Client,
    snap: &SearchSnapshot,
    picks: &[usize],
) -> Result<(Vec<Added>, i32), CliError> {
    for &n in picks {
        if n > snap.results.len() {
            return Err(CliError::invalid(format!(
                "no result number {n} (the search found {})",
                snap.results.len()
            )));
        }
    }
    let mut out: Vec<Option<Added>> = picks.iter().map(|_| None).collect();
    let mut sources = Vec::new();
    let mut slots = Vec::new();
    for (slot, &n) in picks.iter().enumerate() {
        let r: &SearchResultDto = &snap.results[n - 1];
        let magnet = match &r.magnet {
            Some(m) => Ok(m.clone()),
            None if r.detail_url.is_some() => client
                .search_resolve(SearchResolveParams {
                    search_id: Some(snap.search_id),
                    index: Some(n - 1),
                    ..SearchResolveParams::default()
                })
                .await
                .map(|m| m.magnet)
                .map_err(|e| format!("cannot read the magnet link: {e}")),
            None => Err("the result has no magnet link or detail page".to_owned()),
        };
        match magnet {
            Ok(m) => {
                sources.push(AddSource::Uris { uris: vec![m] });
                slots.push(slot);
            }
            Err(error) => out[slot] = Some(Added::Failed { index: n, error }),
        }
    }
    let mut first_failure = out
        .iter()
        .any(Option::is_some)
        .then_some(ErrorCode::General);
    if !sources.is_empty() {
        let outcomes = client
            .add(AddParams {
                sources,
                options: JobOptions::default(),
                paused: false,
            })
            .await?;
        for (slot, outcome) in slots.into_iter().zip(outcomes) {
            let n = picks[slot];
            out[slot] = Some(match outcome {
                AddOutcome::Added { .. } => Added::Ok {
                    ids: outcome.ids().iter().map(ToString::to_string).collect(),
                    title: snap.results[n - 1].title.clone(),
                },
                AddOutcome::Failed { error } => {
                    first_failure.get_or_insert(error.code);
                    Added::Failed {
                        index: n,
                        error: error.message,
                    }
                }
            });
        }
    }
    let out: Vec<Added> = out.into_iter().flatten().collect();
    let failed = out
        .iter()
        .filter(|a| matches!(a, Added::Failed { .. }))
        .count();
    Ok((out, batch_exit(picks.len(), failed, first_failure)))
}

async fn history(ctx: &Ctx, args: &SearchArgs) -> Result<i32, CliError> {
    let client = ctx.connect().await?;
    if args.clear_history {
        client.search_history_clear().await?;
        if !ctx.json {
            println!("search history cleared");
        }
        return Ok(0);
    }
    let list = client.search_history().await?;
    if ctx.json {
        print_json(&list)?;
    } else if list.is_empty() {
        println!("no searches yet");
    } else {
        print!("{}", format::history_table(&list));
    }
    Ok(0)
}
