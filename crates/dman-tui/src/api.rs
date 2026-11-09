//! Daemon calls requested by `update` ([`ApiCall`]) and their results
//! ([`ApiReply`]). The runtime executes calls; formatting the outcome for the
//! status line happens here, in pure code.

use dman_client::ReconnectingClient;
use dman_proto::method::{
    BatchResult, FsListParams, FsListing, LimitsUpdate, RemoveParams, Reorder,
};
use dman_proto::{AddOutcome, AddParams, JobId, Limits};

use crate::format::{limit, plural};

/// A daemon call.
#[derive(Debug, Clone, PartialEq)]
pub enum ApiCall {
    /// `jobs.pause`.
    Pause(Vec<JobId>),
    /// `jobs.resume`.
    Resume(Vec<JobId>),
    /// `queue.pause_all`.
    PauseAll,
    /// `queue.resume_all`.
    ResumeAll,
    /// `jobs.remove`. `delete_files` is only ever set after the double confirmation.
    Remove {
        /// Jobs to remove.
        ids: Vec<JobId>,
        /// Also delete files on disk.
        delete_files: bool,
    },
    /// `jobs.purge` with explicit ids (never the "empty = all" form).
    Purge(Vec<JobId>),
    /// `jobs.reorder`.
    Reorder {
        /// Job to move.
        id: JobId,
        /// Where to.
        to: Reorder,
    },
    /// `limits.set`.
    SetLimits(LimitsUpdate),
    /// `jobs.add` (used by the add view).
    Add(Box<AddParams>),
    /// `fs.list` (directory picker of the add view).
    FsList(FsListParams),
    /// `daemon.shutdown`.
    Shutdown,
}

impl ApiCall {
    /// Present-tense label for "in progress" messages, e.g. `Pausing 3 downloads`.
    pub fn progress_label(&self) -> String {
        let n = |ids: &Vec<JobId>| plural(ids.len(), "download");
        match self {
            ApiCall::Pause(ids) => format!("Pausing {}", n(ids)),
            ApiCall::Resume(ids) => format!("Resuming {}", n(ids)),
            ApiCall::PauseAll => "Pausing all".into(),
            ApiCall::ResumeAll => "Resuming all".into(),
            ApiCall::Remove {
                ids,
                delete_files: false,
            } => format!("Removing {}", n(ids)),
            ApiCall::Remove {
                ids,
                delete_files: true,
            } => format!("Removing {} and deleting files", n(ids)),
            ApiCall::Purge(ids) => format!("Purging {}", n(ids)),
            ApiCall::Reorder { .. } => "Moving in queue".into(),
            ApiCall::SetLimits(_) => "Changing limits".into(),
            ApiCall::Add(p) => format!("Adding {}", plural(p.sources.len(), "source")),
            ApiCall::FsList(_) => "Listing directory".into(),
            ApiCall::Shutdown => "Shutting down the daemon".into(),
        }
    }

    /// Short noun for failure messages, e.g. `Pause failed: …`.
    pub const fn verb(&self) -> &'static str {
        match self {
            ApiCall::Pause(_) => "Pause",
            ApiCall::Resume(_) => "Resume",
            ApiCall::PauseAll => "Pause all",
            ApiCall::ResumeAll => "Resume all",
            ApiCall::Remove { .. } => "Remove",
            ApiCall::Purge(_) => "Purge",
            ApiCall::Reorder { .. } => "Move",
            ApiCall::SetLimits(_) => "Set limits",
            ApiCall::Add(_) => "Add",
            ApiCall::FsList(_) => "List directory",
            ApiCall::Shutdown => "Shutdown",
        }
    }

    /// Execute against the daemon.
    ///
    /// # Errors
    /// The client error, rendered as text for the status line.
    pub async fn execute(self, client: &ReconnectingClient) -> Result<ApiReply, String> {
        let r = match self {
            ApiCall::Pause(ids) => client.pause(&ids).await.map(ApiReply::Batch),
            ApiCall::Resume(ids) => client.resume(&ids).await.map(ApiReply::Batch),
            ApiCall::PauseAll => client.pause_all().await.map(|()| ApiReply::Done),
            ApiCall::ResumeAll => client.resume_all().await.map(|()| ApiReply::Done),
            ApiCall::Remove { ids, delete_files } => client
                .remove(RemoveParams {
                    ids,
                    delete_files,
                    force: false,
                })
                .await
                .map(ApiReply::Batch),
            ApiCall::Purge(ids) => client.purge(&ids).await.map(ApiReply::Batch),
            ApiCall::Reorder { id, to } => client.reorder(id, to).await.map(|()| ApiReply::Done),
            ApiCall::SetLimits(u) => client.limits_set(u).await.map(ApiReply::Limits),
            ApiCall::Add(p) => client.add(*p).await.map(ApiReply::Added),
            ApiCall::FsList(p) => client.fs_list(p).await.map(ApiReply::Listing),
            ApiCall::Shutdown => client.shutdown().await.map(|()| ApiReply::Done),
        };
        r.map_err(|e| e.to_string())
    }
}

/// Successful result of an [`ApiCall`].
#[derive(Debug, Clone, PartialEq)]
pub enum ApiReply {
    /// No payload.
    Done,
    /// Per-item outcome.
    Batch(BatchResult),
    /// Limits now in effect.
    Limits(Limits),
    /// One outcome per added source.
    Added(Vec<AddOutcome>),
    /// A daemon directory listing.
    Listing(FsListing),
}

/// Severity of a status message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Everything worked.
    Ok,
    /// Some items failed.
    Partial,
    /// Nothing worked.
    Failed,
}

/// Status-line text for the result of `call`.
pub fn summarize(call: &ApiCall, result: &Result<ApiReply, String>) -> (Outcome, String) {
    let verb = call.verb();
    match result {
        Err(e) => (Outcome::Failed, format!("{verb} failed: {e}")),
        Ok(ApiReply::Batch(b)) => summarize_batch(verb, b),
        Ok(ApiReply::Limits(l)) => (
            Outcome::Ok,
            format!(
                "Limits: down {} · up {} · max {} concurrent",
                limit(l.download),
                limit(l.upload),
                l.max_concurrent
            ),
        ),
        Ok(ApiReply::Added(outcomes)) => summarize_add(outcomes),
        Ok(ApiReply::Listing(l)) => (Outcome::Ok, format!("Listed {}", l.path)),
        Ok(ApiReply::Done) => (Outcome::Ok, format!("{verb}: done")),
    }
}

fn summarize_batch(verb: &str, b: &BatchResult) -> (Outcome, String) {
    let ok = plural(b.ok.len(), "download");
    match b.failed.first() {
        None => (Outcome::Ok, format!("{verb}: {ok} ok")),
        Some(first) => {
            let outcome = if b.ok.is_empty() {
                Outcome::Failed
            } else {
                Outcome::Partial
            };
            let text = format!(
                "{verb}: {ok} ok, {} failed (#{}: {})",
                b.failed.len(),
                first.id,
                first.error.message
            );
            (outcome, text)
        }
    }
}

fn summarize_add(outcomes: &[AddOutcome]) -> (Outcome, String) {
    let failed: Vec<&str> = outcomes
        .iter()
        .filter_map(|o| match o {
            AddOutcome::Failed { error } => Some(error.message.as_str()),
            AddOutcome::Added { .. } => None,
        })
        .collect();
    let added = outcomes.len() - failed.len();
    match failed.first() {
        None => (Outcome::Ok, format!("Added {}", plural(added, "download"))),
        Some(first) => {
            let outcome = if added == 0 {
                Outcome::Failed
            } else {
                Outcome::Partial
            };
            (
                outcome,
                format!("Added {added}, {} failed: {first}", failed.len()),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use dman_proto::RpcError;
    use dman_proto::method::BatchFailure;

    use super::*;

    #[test]
    fn batch_summaries() {
        let call = ApiCall::Pause(vec![JobId(1), JobId(2)]);
        let all_ok = Ok(ApiReply::Batch(BatchResult {
            ok: vec![JobId(1), JobId(2)],
            failed: vec![],
        }));
        assert_eq!(
            summarize(&call, &all_ok),
            (Outcome::Ok, "Pause: 2 downloads ok".into())
        );
        let partial = Ok(ApiReply::Batch(BatchResult {
            ok: vec![JobId(1)],
            failed: vec![BatchFailure {
                id: JobId(2),
                error: RpcError::not_found("job 2"),
            }],
        }));
        let (o, text) = summarize(&call, &partial);
        assert_eq!(o, Outcome::Partial);
        assert!(text.contains("1 failed (#2: "), "{text}");
        let err = Err("disconnected from daemon".into());
        assert_eq!(summarize(&call, &err).0, Outcome::Failed);
    }
}
