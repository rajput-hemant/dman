//! The rename dialog, shared by the downloads list and the detail view.
//!
//! Renaming is offered for waiting, paused and completed downloads. Active
//! transfers are refused with the reason on the status line (spec). Completed
//! downloads are moved on disk by the daemon, which the prompt says.

use dman_proto::{JobId, JobState};

use crate::api::ApiCall;
use crate::app::{Cmd, State};
use crate::widgets::Modal;
use crate::widgets::prompt::{PromptKind, PromptState};

/// Why an active transfer cannot be renamed.
pub const ACTIVE_REASON: &str = "cannot rename an active transfer — pause it first";

/// Why a download in `state` cannot be renamed, or `None` when it can.
pub fn refusal(state: JobState) -> Option<&'static str> {
    match state {
        JobState::Waiting | JobState::Paused | JobState::Complete => None,
        JobState::Active | JobState::Seeding => Some(ACTIVE_REASON),
        JobState::Error | JobState::Removed => {
            Some("only waiting, paused or completed downloads can be renamed")
        }
    }
}

/// Whether job `id` may be renamed now; says why not on the status line.
pub fn allowed(state: &mut State, id: JobId) -> bool {
    match state.jobs.get(&id).map(|j| j.state) {
        None => {
            state.status.info("The download is gone");
            false
        }
        Some(s) => match refusal(s) {
            Some(reason) => {
                state.status.warn(reason);
                false
            }
            None => true,
        },
    }
}

/// Open the rename prompt for `id`, pre-filled with `current`. With
/// `file_index` the file of a multi-file download is renamed.
pub fn open(state: &mut State, id: JobId, file_index: Option<u32>, current: &str) -> Vec<Cmd> {
    if !allowed(state, id) {
        return Vec::new();
    }
    let complete = state
        .jobs
        .get(&id)
        .is_some_and(|j| j.state == JobState::Complete);
    let (title, label) = if complete {
        (
            "Rename and move on disk",
            "The file is moved on disk. New name:",
        )
    } else {
        ("Rename", "New name (applied when the download runs):")
    };
    state.modal = Some(Modal::Prompt(PromptState::new(
        PromptKind::Rename { id, file_index },
        title,
        label,
        current,
    )));
    Vec::new()
}

/// The prompt was submitted with `value`.
pub fn submit(state: &mut State, id: JobId, file_index: Option<u32>, value: &str) -> Vec<Cmd> {
    if value.is_empty() {
        state.status.error("Rename: the name is empty");
        return Vec::new();
    }
    if value.contains(['/', '\\']) {
        state
            .status
            .error("Rename: give a file name, not a path (no '/')");
        return Vec::new();
    }
    state.call(ApiCall::Rename {
        id,
        name: value.to_owned(),
        file_index,
    })
}
