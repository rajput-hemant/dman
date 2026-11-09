//! Every user-facing operation of the TUI.
//!
//! An [`Action`] is what a key press (via the [`crate::keymap::Keymap`]), a palette
//! entry or a mouse gesture resolves to. Actions carry no data; the target (cursor
//! row, selection) is taken from the state when the action is applied.

use crate::keymap::Context;

/// A user operation. Adding a variant requires adding it to [`Action::ALL`], a
/// default binding in [`crate::keymap::Keymap::defaults`] and metadata below;
/// unit tests enforce all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    /// Move the cursor (or scroll) one row up.
    Up,
    /// Move the cursor (or scroll) one row down.
    Down,
    /// One page up.
    PageUp,
    /// One page down.
    PageDown,
    /// First row.
    Top,
    /// Last row.
    Bottom,
    /// Toggle selection of the row under the cursor.
    ToggleSelect,
    /// Select every visible row.
    SelectAll,
    /// Clear the selection.
    SelectNone,
    /// Select every row between the anchor (last toggled row) and the cursor.
    SelectRange,
    /// Cycle the sort key.
    SortNext,
    /// Reverse the sort direction.
    SortReverse,
    /// Edit the fuzzy name filter.
    Filter,
    /// Remove the filter.
    ClearFilter,
    /// Show the next page of the stopped section.
    StoppedNextPage,
    /// Show the previous page of the stopped section.
    StoppedPrevPage,
    /// Pause the selected downloads.
    Pause,
    /// Resume the selected downloads.
    Resume,
    /// Pause everything.
    PauseAll,
    /// Resume everything.
    ResumeAll,
    /// Remove the selected downloads, keeping their files.
    Remove,
    /// Remove the selected downloads and delete their files (double confirmation).
    RemoveDelete,
    /// Drop stopped downloads from the list (files are kept).
    Purge,
    /// Move the cursor download one place towards the front of the queue.
    MoveUp,
    /// Move the cursor download one place towards the back of the queue.
    MoveDown,
    /// Move the cursor download to the front of the queue.
    MoveTop,
    /// Move the cursor download to the back of the queue.
    MoveBottom,
    /// Raise the global download limit one step.
    LimitUp,
    /// Lower the global download limit one step.
    LimitDown,
    /// Set the maximum number of concurrent downloads.
    SetMaxConcurrent,
    /// Open the detail view of the cursor download.
    OpenDetail,
    /// Open the add view.
    Add,
    /// Add view: preview what will be added, then confirm.
    AddSubmit,
    /// Add view: next field.
    AddNextField,
    /// Add view: previous field.
    AddPrevField,
    /// Add view: browse local files (.torrent/.metalink).
    AddBrowseFiles,
    /// Add view: paste the clipboard.
    AddPasteClipboard,
    /// Add view: group all URI lines into one download (mirrors).
    AddToggleMirror,
    /// Add view: choose the download directory on the daemon host.
    AddPickDir,
    /// Open the command palette.
    Palette,
    /// Show the help screen.
    Help,
    /// Close the current view and go back.
    Back,
    /// Leave the TUI (the daemon keeps running).
    Quit,
    /// Shut the daemon and its engines down.
    Shutdown,
}

impl Action {
    /// Every action, in palette / help order.
    pub const ALL: &'static [Action] = &[
        Action::Up,
        Action::Down,
        Action::PageUp,
        Action::PageDown,
        Action::Top,
        Action::Bottom,
        Action::ToggleSelect,
        Action::SelectAll,
        Action::SelectNone,
        Action::SelectRange,
        Action::SortNext,
        Action::SortReverse,
        Action::Filter,
        Action::ClearFilter,
        Action::StoppedNextPage,
        Action::StoppedPrevPage,
        Action::Pause,
        Action::Resume,
        Action::PauseAll,
        Action::ResumeAll,
        Action::Remove,
        Action::RemoveDelete,
        Action::Purge,
        Action::MoveUp,
        Action::MoveDown,
        Action::MoveTop,
        Action::MoveBottom,
        Action::LimitUp,
        Action::LimitDown,
        Action::SetMaxConcurrent,
        Action::OpenDetail,
        Action::Add,
        Action::AddSubmit,
        Action::AddNextField,
        Action::AddPrevField,
        Action::AddBrowseFiles,
        Action::AddPasteClipboard,
        Action::AddToggleMirror,
        Action::AddPickDir,
        Action::Palette,
        Action::Help,
        Action::Back,
        Action::Quit,
        Action::Shutdown,
    ];

    /// Stable identifier used in `config.toml` (`[keys] pause = ["p"]`).
    pub const fn id(self) -> &'static str {
        match self {
            Action::Up => "up",
            Action::Down => "down",
            Action::PageUp => "page_up",
            Action::PageDown => "page_down",
            Action::Top => "top",
            Action::Bottom => "bottom",
            Action::ToggleSelect => "toggle_select",
            Action::SelectAll => "select_all",
            Action::SelectNone => "select_none",
            Action::SelectRange => "select_range",
            Action::SortNext => "sort",
            Action::SortReverse => "sort_reverse",
            Action::Filter => "filter",
            Action::ClearFilter => "clear_filter",
            Action::StoppedNextPage => "stopped_next_page",
            Action::StoppedPrevPage => "stopped_prev_page",
            Action::Pause => "pause",
            Action::Resume => "resume",
            Action::PauseAll => "pause_all",
            Action::ResumeAll => "resume_all",
            Action::Remove => "remove",
            Action::RemoveDelete => "remove_delete",
            Action::Purge => "purge",
            Action::MoveUp => "move_up",
            Action::MoveDown => "move_down",
            Action::MoveTop => "move_top",
            Action::MoveBottom => "move_bottom",
            Action::LimitUp => "limit_up",
            Action::LimitDown => "limit_down",
            Action::SetMaxConcurrent => "max_concurrent",
            Action::OpenDetail => "detail",
            Action::Add => "add",
            Action::AddSubmit => "add_submit",
            Action::AddNextField => "add_next_field",
            Action::AddPrevField => "add_prev_field",
            Action::AddBrowseFiles => "add_browse_files",
            Action::AddPasteClipboard => "add_paste_clipboard",
            Action::AddToggleMirror => "add_toggle_mirror",
            Action::AddPickDir => "add_pick_dir",
            Action::Palette => "palette",
            Action::Help => "help",
            Action::Back => "back",
            Action::Quit => "quit",
            Action::Shutdown => "shutdown",
        }
    }

    /// Look an action up by its [`Action::id`].
    pub fn from_id(id: &str) -> Option<Action> {
        Action::ALL.iter().copied().find(|a| a.id() == id)
    }

    /// Description shown in the palette and on the help screen.
    pub const fn title(self) -> &'static str {
        match self {
            Action::Up => "Move up",
            Action::Down => "Move down",
            Action::PageUp => "Page up",
            Action::PageDown => "Page down",
            Action::Top => "Go to top",
            Action::Bottom => "Go to bottom",
            Action::ToggleSelect => "Toggle selection",
            Action::SelectAll => "Select all",
            Action::SelectNone => "Clear selection",
            Action::SelectRange => "Select range (anchor to cursor)",
            Action::SortNext => "Cycle sort key",
            Action::SortReverse => "Reverse sort order",
            Action::Filter => "Filter by name (fuzzy)",
            Action::ClearFilter => "Clear filter",
            Action::StoppedNextPage => "Next page of stopped downloads",
            Action::StoppedPrevPage => "Previous page of stopped downloads",
            Action::Pause => "Pause",
            Action::Resume => "Resume",
            Action::PauseAll => "Pause all",
            Action::ResumeAll => "Resume all",
            Action::Remove => "Remove (keep files)",
            Action::RemoveDelete => "Remove and delete files",
            Action::Purge => "Purge stopped downloads (keep files)",
            Action::MoveUp => "Move up in queue",
            Action::MoveDown => "Move down in queue",
            Action::MoveTop => "Move to top of queue",
            Action::MoveBottom => "Move to bottom of queue",
            Action::LimitUp => "Raise global download limit",
            Action::LimitDown => "Lower global download limit",
            Action::SetMaxConcurrent => "Set max concurrent downloads",
            Action::OpenDetail => "Open detail",
            Action::Add => "Add downloads",
            Action::AddSubmit => "Add view: preview and confirm",
            Action::AddNextField => "Add view: next field",
            Action::AddPrevField => "Add view: previous field",
            Action::AddBrowseFiles => "Add view: browse local .torrent/.metalink files",
            Action::AddPasteClipboard => "Add view: paste from clipboard",
            Action::AddToggleMirror => "Add view: toggle mirrors (one download from all URLs)",
            Action::AddPickDir => "Add view: choose download directory (daemon)",
            Action::Palette => "Command palette",
            Action::Help => "Help (key bindings)",
            Action::Back => "Back / close",
            Action::Quit => "Quit the TUI (daemon keeps running)",
            Action::Shutdown => "Shut down the daemon and engines",
        }
    }

    /// One-word label for the footer.
    pub const fn short(self) -> &'static str {
        match self {
            Action::Up => "up",
            Action::Down => "down",
            Action::PageUp => "pgup",
            Action::PageDown => "pgdn",
            Action::Top => "top",
            Action::Bottom => "bottom",
            Action::ToggleSelect => "select",
            Action::SelectAll => "all",
            Action::SelectNone => "none",
            Action::SelectRange => "range",
            Action::SortNext => "sort",
            Action::SortReverse => "reverse",
            Action::Filter => "filter",
            Action::ClearFilter => "unfilter",
            Action::StoppedNextPage => "next page",
            Action::StoppedPrevPage => "prev page",
            Action::Pause => "pause",
            Action::Resume => "resume",
            Action::PauseAll => "pause all",
            Action::ResumeAll => "resume all",
            Action::Remove => "remove",
            Action::RemoveDelete => "remove+files",
            Action::Purge => "purge",
            Action::MoveUp => "queue up",
            Action::MoveDown => "queue down",
            Action::MoveTop => "queue top",
            Action::MoveBottom => "queue bottom",
            Action::LimitUp => "limit+",
            Action::LimitDown => "limit-",
            Action::SetMaxConcurrent => "max conc.",
            Action::OpenDetail => "detail",
            Action::Add => "add",
            Action::AddSubmit => "preview",
            Action::AddNextField => "next field",
            Action::AddPrevField => "prev field",
            Action::AddBrowseFiles => "files",
            Action::AddPasteClipboard => "clipboard",
            Action::AddToggleMirror => "mirror",
            Action::AddPickDir => "folder",
            Action::Palette => "palette",
            Action::Help => "help",
            Action::Back => "back",
            Action::Quit => "quit",
            Action::Shutdown => "shutdown",
        }
    }

    /// The context whose default keymap holds this action; used to show its key
    /// in the palette.
    pub const fn home_context(self) -> Context {
        match self {
            Action::Palette | Action::Help | Action::Quit | Action::Shutdown => Context::Global,
            Action::Back => Context::Detail,
            Action::AddSubmit
            | Action::AddNextField
            | Action::AddPrevField
            | Action::AddBrowseFiles
            | Action::AddPasteClipboard
            | Action::AddToggleMirror
            | Action::AddPickDir => Context::Add,
            _ => Context::Downloads,
        }
    }

    /// Whether the action talks to the daemon and is therefore disabled while
    /// disconnected (decision Q4: actions are never queued).
    pub const fn needs_connection(self) -> bool {
        matches!(
            self,
            Action::Pause
                | Action::Resume
                | Action::PauseAll
                | Action::ResumeAll
                | Action::Remove
                | Action::RemoveDelete
                | Action::Purge
                | Action::MoveUp
                | Action::MoveDown
                | Action::MoveTop
                | Action::MoveBottom
                | Action::LimitUp
                | Action::LimitDown
                | Action::SetMaxConcurrent
                | Action::Shutdown
                | Action::AddPickDir
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for a in Action::ALL {
            assert!(seen.insert(a.id()), "duplicate id {}", a.id());
            assert_eq!(Action::from_id(a.id()), Some(*a));
        }
    }

    #[test]
    fn all_is_complete() {
        // `Shutdown` is the last variant; the discriminant count must match.
        assert_eq!(Action::ALL.len(), Action::Shutdown as usize + 1);
    }
}
