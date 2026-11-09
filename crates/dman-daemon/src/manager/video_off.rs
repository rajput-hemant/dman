//! Stand-in for `manager::video` in builds without the `video` feature. No yt-dlp process
//! ever runs, so video jobs found in the store stay where they are and can be removed.

use dman_proto::{AddSource, ErrorCode, JobId, JobOptions, JobState, RpcError};
use dman_store::{Desired, LiveFigures};
use tokio::sync::mpsc;

use super::Manager;
use crate::video::unavailable;

/// Never constructed: there is no forwarder task to send anything.
#[derive(Debug)]
pub(crate) enum VideoMsg {}

pub(crate) type VideoTx = mpsc::UnboundedSender<VideoMsg>;

/// Never constructed: there is no yt-dlp process.
#[derive(Debug)]
pub(crate) struct VideoRun {
    pub(crate) stopping: bool,
}

impl Manager {
    pub(super) async fn add_video(
        &mut self,
        _src: AddSource,
        _options: &JobOptions,
        _dir: &str,
        _paused: bool,
    ) -> Result<Vec<JobId>, RpcError> {
        Err(unavailable())
    }

    pub(super) fn start_video(&mut self, _id: JobId) {}

    /// With no video job running, aria2 keeps the whole global limit.
    pub(super) fn aria2_download_limit(&self, global: u64) -> u64 {
        global
    }

    pub(super) async fn balance_limits(&mut self) {
        self.sync_global_limits().await;
    }

    /// There is no process to stop; the job only changes state.
    pub(super) fn stop_video(&mut self, id: JobId, state: JobState) {
        let Some(e) = self.jobs.get_mut(&id) else {
            return;
        };
        e.live = LiveFigures::default();
        if e.rec.state != state && !e.rec.state.is_stopped() {
            e.rec.state = state;
            e.dirty = true;
            self.urgent = true;
        }
    }

    pub(super) fn pump_videos(&mut self) {}

    pub(super) fn video_speed(&self) -> u64 {
        0
    }

    pub(super) async fn on_video_msg(&mut self, msg: VideoMsg) {
        match msg {}
    }

    /// `jobs.remove` of a video job: forgets the record; its files are never deleted here.
    pub(super) async fn remove_video(
        &mut self,
        id: JobId,
        delete_files: bool,
    ) -> Result<(), RpcError> {
        if delete_files {
            return Err(RpcError::new(
                ErrorCode::Refused,
                "this build of dman has no video support, so it will not delete the files of a \
                 video job; remove the job without deleting files, or rebuild with --features video",
            ));
        }
        let Some(e) = self.jobs.get_mut(&id) else {
            return Err(RpcError::not_found(format!("job {id}")));
        };
        super::remove::note_removal(&mut e.rec, false);
        e.rec.state = JobState::Removed;
        e.rec.desired = Desired::Stopped;
        e.live = LiveFigures::default();
        self.mark(id, true);
        Ok(())
    }

    pub(super) async fn shutdown_videos(&mut self) {}
}
