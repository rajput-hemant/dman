//! pause, resume, remove, purge and reorder.

use std::io::{BufRead, IsTerminal, Write};

use dman_proto::method::{BatchResult, Reorder};
use dman_proto::{ErrorCode, JobId, JobState};

use super::print_json;
use crate::cli::{PurgeArgs, RemoveArgs, ReorderArgs, TargetArgs};
use crate::connect::Ctx;
use crate::error::{CliError, batch_exit};

fn report(ctx: &Ctx, verb: &str, res: &BatchResult) -> Result<i32, CliError> {
    if ctx.json {
        print_json(res)?;
    } else {
        if !res.ok.is_empty() {
            println!("{verb} {}", join_ids(&res.ok));
        }
        for f in &res.failed {
            println!("failed {}: {}", f.id, f.error.message);
        }
    }
    let first: Option<ErrorCode> = res.failed.first().map(|f| f.error.code);
    Ok(batch_exit(
        res.ok.len() + res.failed.len(),
        res.failed.len(),
        first,
    ))
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
            println!("{:>10}  {}", crate::format::size(f.bytes), f.path);
        }
    }
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
        say(ctx, "paused all");
        return Ok(0);
    }
    report(ctx, "paused", &client.pause(&a.ids).await?)
}

pub async fn resume(ctx: &Ctx, a: &TargetArgs) -> Result<i32, CliError> {
    let client = ctx.connect().await?;
    if a.all {
        client.resume_all().await?;
        say(ctx, "resumed all");
        return Ok(0);
    }
    report(ctx, "resumed", &client.resume(&a.ids).await?)
}

fn say(ctx: &Ctx, text: &str) {
    if ctx.json {
        println!("{}", serde_json::json!({ "status": text }));
    } else {
        println!("{text}");
    }
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

/// Confirm a destructive action. Must run before connecting.
fn confirm(question: &str, yes: bool) -> Result<(), CliError> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(CliError::refused(
            "refusing destructive action without --yes",
        ));
    }
    if !is_yes(&ask(&format!("{question} [y/N] "))?) {
        return Err(CliError::refused("aborted"));
    }
    Ok(())
}

pub async fn remove(ctx: &Ctx, a: &RemoveArgs) -> Result<i32, CliError> {
    let what = if a.delete_files {
        format!("Remove {} job(s) and delete their files?", a.ids.len())
    } else {
        format!("Remove {} job(s)?", a.ids.len())
    };
    confirm(&what, a.yes)?;
    let params = dman_proto::method::RemoveParams {
        ids: a.ids.clone(),
        delete_files: a.delete_files,
        force: a.force,
    };
    let res = ctx.connect().await?.remove(params).await?;
    report(ctx, "removed", &res)
}

pub async fn purge(ctx: &Ctx, a: &PurgeArgs) -> Result<i32, CliError> {
    let question = if a.ids.is_empty() {
        "Purge all stopped jobs?".to_owned()
    } else {
        format!("Purge {} job(s)?", a.ids.len())
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
        say(ctx, "nothing to purge");
        return Ok(0);
    }
    report(ctx, "purged", &client.purge(&ids).await?)
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
    say(ctx, &format!("reordered {}", a.id));
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
