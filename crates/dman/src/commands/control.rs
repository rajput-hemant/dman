//! pause, resume, remove, purge and reorder.

use std::io::{BufRead, Write};

use dman_proto::method::{BatchResult, Reorder};
use dman_proto::{ErrorCode, JobId, JobState};

use super::{can_prompt, print_json, say};
use crate::cli::{PurgeArgs, RemoveArgs, ReorderArgs, RestoreArgs, TargetArgs};
use crate::connect::Ctx;
use crate::error::{CliError, batch_exit};

async fn report(
    ctx: &Ctx,
    client: &dman_client::Client,
    verb: &str,
    res: &BatchResult,
) -> Result<i32, CliError> {
    if ctx.json {
        print_json(res)?;
    } else {
        if !res.ok.is_empty() {
            println!("{verb} {}", join_ids(&res.ok));
        }
        // Failures are diagnostics: stderr keeps stdout clean for pipes.
        for f in &res.failed {
            eprintln!(
                "dman: failed {}: {}",
                f.id,
                crate::format::printable(&f.error.message)
            );
        }
        if let Some(hint) = batch_hint(client, res).await {
            eprintln!("hint: {hint}");
        }
    }
    let first: Option<ErrorCode> = res.failed.first().map(|f| f.error.code);
    Ok(batch_exit(
        res.ok.len() + res.failed.len(),
        res.failed.len(),
        first,
    ))
}

/// The next step for the failures of a batch: the hint of the first failure with
/// one, plus did-you-mean ids when jobs were not found.
async fn batch_hint(client: &dman_client::Client, res: &BatchResult) -> Option<String> {
    let first = res
        .failed
        .iter()
        .find_map(|f| crate::error::rpc_hint(&f.error))?;
    let missing: Vec<String> = res
        .failed
        .iter()
        .filter(|f| f.error.code == ErrorCode::NotFound)
        .map(|f| f.id.to_string())
        .collect();
    if missing.is_empty() {
        return Some(first);
    }
    let jobs = client
        .list(dman_proto::method::ListParams::default())
        .await
        .ok()?;
    let ids: Vec<String> = jobs.iter().map(|j| j.id.to_string()).collect();
    let mut close: Vec<String> = Vec::new();
    for m in &missing {
        for c in crate::suggest::closest(m, ids.iter().map(String::as_str), 2) {
            if !close.contains(&c) {
                close.push(c);
            }
        }
    }
    Some(match crate::suggest::did_you_mean(&close) {
        Some(d) => format!("{first} ({d})"),
        None => first,
    })
}

fn join_ids(ids: &[JobId]) -> String {
    ids.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

pub async fn rename(ctx: &Ctx, id: JobId, name: &str, file: Option<u32>) -> Result<i32, CliError> {
    let client = ctx.connect().await?;
    client.rename(id, name, file).await?;
    if ctx.json {
        print_json(&serde_json::json!({ "renamed": id, "name": name }))?;
    } else {
        println!("renamed {id} to {name}");
    }
    Ok(0)
}

/// `dman cleanup <id>`: list the unselected partial files, ask, delete.
pub async fn cleanup(ctx: &Ctx, id: JobId, yes: bool) -> Result<i32, CliError> {
    let client = ctx.connect().await?;
    let found = client.cleanup_unselected(id, true).await?;
    if found.files.is_empty() {
        if ctx.json {
            print_json(&found)?;
        } else {
            println!("no unselected partial files to delete");
        }
        return Ok(0);
    }
    if !ctx.json {
        for f in &found.files {
            println!(
                "{:>10}  {}",
                crate::format::size(f.bytes),
                crate::format::printable(&f.path)
            );
        }
    }
    // Needs a terminal or --yes, like every destructive command.
    confirm(
        &format!(
            "Delete {} unselected partial file(s) ({})?",
            found.files.len(),
            crate::format::size(found.bytes)
        ),
        yes,
    )?;
    let done = client.cleanup_unselected(id, false).await?;
    if ctx.json {
        print_json(&done)?;
    } else {
        println!(
            "deleted {} file(s) ({})",
            done.deleted,
            crate::format::size(done.bytes)
        );
    }
    Ok(0)
}

pub async fn pause(ctx: &Ctx, a: &TargetArgs) -> Result<i32, CliError> {
    let client = ctx.connect().await?;
    if a.all {
        client.pause_all().await?;
        say(ctx, "paused all", "paused_all")?;
        return Ok(0);
    }
    report(ctx, &client, "paused", &client.pause(&a.ids).await?).await
}

pub async fn resume(ctx: &Ctx, a: &TargetArgs) -> Result<i32, CliError> {
    let client = ctx.connect().await?;
    if a.all {
        client.resume_all().await?;
        say(ctx, "resumed all", "resumed_all")?;
        return Ok(0);
    }
    report(ctx, &client, "resumed", &client.resume(&a.ids).await?).await
}

/// Ask a question on stderr and read one line from stdin.
fn ask(question: &str) -> Result<String, CliError> {
    eprint!("{question}");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

fn is_yes(answer: &str) -> bool {
    matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Confirm a destructive action (a terminal answers it; elsewhere `--yes` is needed).
fn confirm(question: &str, yes: bool) -> Result<(), CliError> {
    if yes {
        return Ok(());
    }
    if !can_prompt() {
        return Err(
            CliError::refused("refusing destructive action without --yes")
                .with_hint("rerun with --yes to confirm, or from a terminal to be asked"),
        );
    }
    if !is_yes(&ask(&format!("{question} [y/N] "))?) {
        return Err(CliError::refused("aborted"));
    }
    Ok(())
}

pub async fn remove(ctx: &Ctx, a: &RemoveArgs) -> Result<i32, CliError> {
    let what = if a.delete_files {
        format!("Remove {} download(s) and delete their files?", a.ids.len())
    } else {
        format!(
            "Remove {} download(s)? (`dman restore` brings them back)",
            a.ids.len()
        )
    };
    confirm(&what, a.yes)?;
    let params = dman_proto::method::RemoveParams {
        ids: a.ids.clone(),
        delete_files: a.delete_files,
        force: a.force,
    };
    let client = ctx.connect().await?;
    let res = client.remove(params).await?;
    report(ctx, &client, "removed", &res).await
}

pub async fn restore(ctx: &Ctx, a: &RestoreArgs) -> Result<i32, CliError> {
    let client = ctx.connect().await?;
    let res = client.restore(&a.ids).await?;
    report(ctx, &client, "restored", &res).await
}

pub async fn purge(ctx: &Ctx, a: &PurgeArgs) -> Result<i32, CliError> {
    let question = if a.ids.is_empty() {
        "Purge all finished, failed and removed downloads from the history?".to_owned()
    } else {
        format!("Purge {} download(s) from the history?", a.ids.len())
    };
    confirm(&question, a.yes)?;
    let client = ctx.connect().await?;
    let ids = if a.ids.is_empty() {
        let stopped = [JobState::Complete, JobState::Error, JobState::Removed];
        let jobs = client
            .list(dman_proto::method::ListParams {
                states: stopped.to_vec(),
            })
            .await?;
        jobs.into_iter().map(|j| j.id).collect()
    } else {
        a.ids.clone()
    };
    if ids.is_empty() {
        if ctx.json {
            let none = BatchResult {
                ok: Vec::new(),
                failed: Vec::new(),
            };
            return report(ctx, &client, "purged", &none).await;
        }
        say(ctx, "nothing to purge", "nothing_to_purge")?;
        return Ok(0);
    }
    report(ctx, &client, "purged", &client.purge(&ids).await?).await
}

fn reorder_target(a: &ReorderArgs) -> Reorder {
    if let Some(n) = a.to {
        Reorder::To(n)
    } else if a.up {
        Reorder::By(-1)
    } else if a.down {
        Reorder::By(1)
    } else if a.top {
        Reorder::Top
    } else {
        Reorder::Bottom
    }
}

pub async fn reorder(ctx: &Ctx, a: &ReorderArgs) -> Result<i32, CliError> {
    ctx.connect()
        .await?
        .reorder(a.id, reorder_target(a))
        .await?;
    if ctx.json {
        print_json(&serde_json::json!({ "reordered": a.id }))?;
    } else {
        println!("reordered {}", a.id);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yes_answers() {
        assert!(is_yes("y"));
        assert!(is_yes("YES"));
        assert!(!is_yes(""));
        assert!(!is_yes("n"));
    }

    #[test]
    fn confirm_with_yes_skips_prompts() {
        assert!(confirm("?", true).is_ok());
    }
}
