//! `dman set`: change options of existing jobs (`jobs.set_options`, and
//! `jobs.reorder` for the queue position).

use dman_proto::method::Reorder;
use dman_proto::{ErrorCode, JobId, JobOptions};

use crate::cli::SetArgs;
use crate::commands::add::parse_key_value;
use crate::connect::Ctx;
use crate::error::{CliError, batch_exit};

/// The option changes of `args` (only the fields that were given).
pub fn options_of(args: &SetArgs) -> Result<JobOptions, CliError> {
    if args.seed_ratio.is_some_and(|r| !r.is_finite() || r < 0.0) {
        return Err(CliError::invalid("--seed-ratio must be 0 or more"));
    }
    let mut o = JobOptions {
        dir: args.dir.clone(),
        max_download_limit: args.max_download_limit,
        max_upload_limit: args.max_upload_limit,
        select_files: args.select.clone(),
        seed_ratio: args.seed_ratio,
        seed_time: args.seed_time,
        headers: args.header.clone(),
        user_agent: args.user_agent.clone(),
        referer: args.referer.clone(),
        connections_per_server: args.connections,
        split: args.split,
        ..JobOptions::default()
    };
    for kv in &args.options {
        let (k, v) = parse_key_value(kv)?;
        o.engine_options.insert(k, v);
    }
    Ok(o)
}

/// A refusal of the daemon, with what to do about it.
fn explain(id: JobId, e: CliError) -> CliError {
    if e.code != ErrorCode::Refused {
        return e;
    }
    let hint = if e.message.contains("while the download runs") {
        format!(" (run `dman pause {id}`, then set it, then `dman resume {id}`)")
    } else if e.message.contains("is finished") {
        " (a finished download keeps its options; `dman rename` moves its files)".to_owned()
    } else {
        String::new()
    };
    CliError::new(e.code, format!("{}{hint}", e.message))
}

pub async fn run(ctx: &Ctx, args: &SetArgs) -> Result<i32, CliError> {
    let delta = options_of(args)?;
    let changes_options = delta != JobOptions::default();
    let client = ctx.connect().await?;
    let mut failed = 0;
    let mut first = None;
    let mut done = Vec::new();
    for (i, &id) in args.ids.iter().enumerate() {
        let mut result = Ok(());
        if changes_options {
            result = client
                .set_options(id, delta.clone())
                .await
                .map_err(|e| explain(id, e.into()));
        }
        if result.is_ok()
            && let Some(pos) = args.position
        {
            let to = pos.saturating_add(u32::try_from(i).unwrap_or(u32::MAX));
            result = client
                .reorder(id, Reorder::To(to))
                .await
                .map_err(CliError::from);
        }
        match result {
            Ok(()) => done.push(id),
            Err(e) => {
                failed += 1;
                first.get_or_insert(e.code);
                if ctx.json {
                    let code = serde_json::to_value(e.code).unwrap_or_default();
                    let body = serde_json::json!({
                        "error": { "code": code, "message": e.message, "id": id }
                    });
                    eprintln!("{body}");
                } else {
                    eprintln!("dman: job {id}: {}", e.message);
                }
            }
        }
    }
    if ctx.json {
        let mut jobs = Vec::with_capacity(done.len());
        for &id in &done {
            jobs.push(client.get(id).await?);
        }
        super::print_json(&jobs)?;
    } else if !done.is_empty() {
        let ids: Vec<String> = done.iter().map(ToString::to_string).collect();
        println!("updated {}", ids.join(" "));
    }
    Ok(batch_exit(args.ids.len(), failed, first))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_given_fields_are_sent() {
        let a = SetArgs {
            ids: vec![JobId(1)],
            max_download_limit: Some(1024),
            select: Some(vec![1, 3]),
            header: vec!["Cookie: a=b".into()],
            options: vec!["bt-max-peers=10".into()],
            ..SetArgs::default()
        };
        let o = options_of(&a).unwrap();
        assert_eq!(o.max_download_limit, Some(1024));
        assert_eq!(o.select_files, Some(vec![1, 3]));
        assert_eq!(o.headers, ["Cookie: a=b"]);
        assert_eq!(o.engine_options["bt-max-peers"], "10");
        assert_eq!(o.dir, None);
        assert_eq!(o.seed_ratio, None);
        let only_position = SetArgs {
            position: Some(0),
            ..SetArgs::default()
        };
        assert_eq!(options_of(&only_position).unwrap(), JobOptions::default());
    }

    #[test]
    fn invalid_values_are_usage_errors() {
        let a = SetArgs {
            seed_ratio: Some(-1.0),
            ..SetArgs::default()
        };
        assert_eq!(options_of(&a).unwrap_err().exit, 2);
        let a = SetArgs {
            options: vec!["novalue".into()],
            ..SetArgs::default()
        };
        assert_eq!(options_of(&a).unwrap_err().exit, 2);
    }

    #[test]
    fn refusals_get_a_hint() {
        let e = explain(
            JobId(7),
            CliError::refused(
                "`dir` cannot be changed while the download runs; pause the download first",
            ),
        );
        assert!(e.message.contains("dman pause 7"), "{}", e.message);
        assert_eq!(e.exit, 6);
        let e = explain(JobId(7), CliError::refused("job 7 is finished"));
        assert!(e.message.contains("dman rename"), "{}", e.message);
        let e = explain(JobId(7), CliError::invalid("bad"));
        assert_eq!(e.message, "bad");
    }
}
