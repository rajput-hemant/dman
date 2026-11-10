//! Restarting a download from zero when its server cannot resume it.
//!
//! After a pause (or a daemon restart) aria2 resumes an HTTP/FTP download with
//! a range request. A server without range support answers with the whole
//! file, and aria2 gives up with error code 8 ("No URI available."). It does
//! not start over by itself: its `always-resume=false` path aborts aria2 1.37
//! (assertion in the disk cache), and its `.aria2` control file makes every
//! re-add try to resume again.
//!
//! dman therefore restarts such a job itself, once per failure the user did
//! not ask to retry: it drops the failed result, removes the job's own
//! `.aria2` control file (only that file, inside the job directory, see
//! [`crate::ops`]), and re-adds the download with `continue=false` and
//! `allow-overwrite=true`, so aria2 downloads into the same file from the
//! first byte. A `Message` tells the user why the progress was lost. A second
//! failure of the same kind leaves the job failed with an explanation.

use std::path::PathBuf;

use dman_aria2::{Aria2Client, Gid, Status};
use dman_proto::method::Level;
use dman_proto::{JobId, SourceKind};
use dman_store::{Desired, JobRecord};
use serde_json::Value;

use super::Manager;
use super::util::recorded_files;
use crate::ops;

/// aria2's error code for "the server does not support resuming".
const RESUME_UNSUPPORTED: u64 = 8;
/// Record key: the job was already restarted from zero for this reason.
const RESTARTED: &str = "restarted_from_zero";

/// aria2 failed `st` because its server cannot resume the download.
pub(crate) fn resume_unsupported(st: &Status) -> bool {
    st.error_code == Some(RESUME_UNSUPPORTED)
}

/// The error text shown for a download whose server cannot resume it.
pub(crate) fn explain(st: &Status) -> String {
    let detail = st
        .error_message
        .as_deref()
        .filter(|m| !m.is_empty())
        .unwrap_or("error 8");
    format!(
        "the server does not support resuming this download, so it cannot continue \
         where it stopped (aria2: {detail}); retry to download it again from the beginning"
    )
}

/// Whether a failure described by `st` should restart `rec` from zero now.
pub(crate) fn should_restart(rec: &JobRecord, st: &Status) -> bool {
    resume_unsupported(st)
        && matches!(
            rec.kind,
            SourceKind::Http | SourceKind::Ftp | SourceKind::Sftp
        )
        && rec.desired != Desired::Stopped
        && rec.extra.get(RESTARTED).and_then(Value::as_bool) != Some(true)
}

/// A user retry of a failed job may restart it from zero once more.
pub(crate) fn allow_again(rec: &mut JobRecord) {
    rec.extra.remove(RESTARTED);
}

impl Manager {
    /// Restarts every job flagged by the last poll (see the module docs).
    pub(super) async fn restart_unresumable(&mut self, client: &Aria2Client) {
        let ids: Vec<JobId> = self
            .jobs
            .values()
            .filter(|e| e.restart_from_zero)
            .map(|e| e.rec.id)
            .collect();
        for id in ids {
            self.restart_from_zero(client, id).await;
        }
    }

    async fn restart_from_zero(&mut self, client: &Aria2Client, id: JobId) {
        let Some(e) = self.jobs.get_mut(&id) else {
            return;
        };
        e.restart_from_zero = false;
        e.rec.extra.insert(RESTARTED.into(), Value::Bool(true));
        e.dirty = true;
        let name = e.rec.name.clone();
        let dir = PathBuf::from(&e.rec.dir);
        let mut files = recorded_files(&e.rec);
        if files.is_empty() {
            files.push(dir.join(&e.rec.name));
        }
        if let Some(gid) = e.rec.gid.clone() {
            let _ = client.remove_download_result(&Gid::new(gid)).await;
        }
        // Only the control files: the data file is overwritten by the new download.
        let controls: Vec<PathBuf> = files
            .iter()
            .map(|f| {
                let f = if f.is_absolute() {
                    f.clone()
                } else {
                    dir.join(f)
                };
                let mut s = f.into_os_string();
                s.push(".aria2");
                PathBuf::from(s)
            })
            .collect();
        let report = tokio::task::spawn_blocking(move || ops::delete_exact_files(&dir, &controls))
            .await
            .unwrap_or_default();
        if !report.refused.is_empty() || !report.failed.is_empty() {
            tracing::warn!(%id, ?report, "could not remove the aria2 control file");
        }
        self.message(
            Level::Warn,
            format!(
                "{name}: the server does not support resuming, so the download starts again \
                 from the beginning"
            ),
        );
        self.readd_with_or_fail(
            client,
            id,
            &[("continue", "false"), ("allow-overwrite", "true")],
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dman_proto::{Engine, JobOptions, JobState};

    fn rec(kind: SourceKind) -> JobRecord {
        JobRecord {
            id: JobId(1),
            engine: Engine::Aria2,
            kind,
            name: "f.bin".into(),
            dir: "/d".into(),
            source: "http://x/f.bin".into(),
            options: JobOptions::default(),
            desired: Desired::Queued,
            state: JobState::Active,
            gid: Some("g".into()),
            info_hash: None,
            total_bytes: 0,
            completed_bytes: 0,
            uploaded_base: 0,
            uploaded_session: 0,
            added_at: 0,
            completed_at: None,
            queue_key: 0,
            error: None,
            has_blob: false,
            extra: Default::default(),
        }
    }

    fn failed(code: u64) -> Status {
        Status {
            error_code: Some(code),
            error_message: Some("No URI available.".into()),
            ..Default::default()
        }
    }

    #[test]
    fn restarts_once_and_only_plain_downloads() {
        let mut r = rec(SourceKind::Http);
        assert!(should_restart(&r, &failed(8)));
        assert!(!should_restart(&r, &failed(1)));
        assert!(!should_restart(&rec(SourceKind::Torrent), &failed(8)));
        r.extra.insert(RESTARTED.into(), Value::Bool(true));
        assert!(!should_restart(&r, &failed(8)));
        allow_again(&mut r);
        assert!(should_restart(&r, &failed(8)));
        r.desired = Desired::Stopped;
        assert!(!should_restart(&r, &failed(8)));
        assert!(explain(&failed(8)).contains("does not support resuming"));
    }
}
