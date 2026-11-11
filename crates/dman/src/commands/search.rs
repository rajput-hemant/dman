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
        return Err(CliError::invalid("the query is empty")
            .with_hint("give search words, e.g. `dman search ubuntu 24.04`"));
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
        return Err(CliError::refused("no search provider is enabled")
            .with_hint("see `dman providers list`, then `dman providers enable NAME`"));
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
                    return Err(CliError::unreachable("connection to the daemon was lost")
                        .with_hint("check `dman daemon status`, then repeat the search"));
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
        return Err(
            CliError::general(if ctx.json || snapshot.providers.len() > 1 {
                "every provider failed".to_owned()
            } else {
                format!("provider {} failed: {}", failed[0].0, failed[0].1)
            })
            .with_hint("check a provider with `dman providers test NAME`"),
        );
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
            if args.add.is_empty() && !args.no_prompt && interactive() {
                let picks = ask_picks(&mut std::io::stdin().lock(), &mut std::io::stderr(), shown)?;
                if !picks.is_empty() {
                    let (outcomes, exit) = add_results(&client, &snapshot, &picks).await?;
                    added = outcomes;
                    code = exit;
                }
            }
        }
        for a in &added {
            match a {
                Added::Ok { ids, title } => println!("added {} ({title})", ids.join(" ")),
                Added::Failed { index, error } => eprintln!("dman: failed #{index}: {error}"),
            }
        }
    }
    Ok(code)
}

/// The pick prompt appears only when both ends are a terminal.
fn interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Parse a selection such as `1,3`, `2-4` or `1 3-5` into result numbers in the
/// order given, without repeats. Every number must be within `1..=max`.
fn parse_selection(input: &str, max: usize) -> Result<Vec<usize>, String> {
    let number = |t: &str| match t.trim().parse::<usize>() {
        Ok(n) if (1..=max).contains(&n) => Ok(n),
        Ok(n) => Err(format!("{n} is not a result number (1-{max})")),
        Err(_) => Err(format!("`{}` is not a number", t.trim())),
    };
    let mut out = Vec::new();
    for part in input
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
    {
        let range: Vec<usize> = match part.split_once('-') {
            Some((a, b)) => {
                let (a, b) = (number(a)?, number(b)?);
                if b < a {
                    return Err(format!("`{part}` is not a valid range"));
                }
                (a..=b).collect()
            }
            None => vec![number(part)?],
        };
        for n in range {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    Ok(out)
}

/// Ask which results to add until the answer parses; empty input or end of input
/// means none.
fn ask_picks(
    input: &mut impl std::io::BufRead,
    out: &mut impl std::io::Write,
    max: usize,
) -> Result<Vec<usize>, CliError> {
    loop {
        write!(out, "Add which? (e.g. 1,3 or 2-4; Enter to skip) ")?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(Vec::new());
        }
        match parse_selection(&line, max) {
            Ok(picks) => return Ok(picks),
            Err(e) => writeln!(out, "{e}")?,
        }
    }
}

fn progress(ctx: &Ctx, provider: &str, status: &ProviderStatus) {
    if ctx.json {
        return;
    }
    match status {
        ProviderStatus::Running => {}
        ProviderStatus::Done {
            count,
            elapsed_ms,
            mirror,
        } => match mirror {
            Some(m) => eprintln!("{provider}: {count} results in {elapsed_ms} ms (via {m})"),
            None => eprintln!("{provider}: {count} results in {elapsed_ms} ms"),
        },
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
            ))
            .with_hint("pick a number from the results table"));
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
                force: false,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selections() {
        assert_eq!(parse_selection("1,3", 5).unwrap(), vec![1, 3]);
        assert_eq!(parse_selection("2-4", 5).unwrap(), vec![2, 3, 4]);
        assert_eq!(parse_selection(" 1 3-4,3 ", 5).unwrap(), vec![1, 3, 4]);
        assert!(parse_selection("", 5).unwrap().is_empty());
        assert!(parse_selection("0", 5).is_err());
        assert!(parse_selection("6", 5).is_err());
        assert!(parse_selection("4-2", 5).is_err());
        assert!(parse_selection("a", 5).is_err());
        assert!(parse_selection("1-", 5).is_err());
    }

    #[test]
    fn prompt_asks_again_after_a_bad_answer() {
        let mut out = Vec::new();
        let picks = ask_picks(&mut "9\n2-3\n".as_bytes(), &mut out, 4).unwrap();
        assert_eq!(picks, vec![2, 3]);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("not a result number"), "{text}");
        assert_eq!(text.matches("Add which?").count(), 2);
        assert!(
            ask_picks(&mut "\n".as_bytes(), &mut Vec::new(), 4)
                .unwrap()
                .is_empty()
        );
        assert!(
            ask_picks(&mut "".as_bytes(), &mut Vec::new(), 4)
                .unwrap()
                .is_empty()
        );
    }
}
