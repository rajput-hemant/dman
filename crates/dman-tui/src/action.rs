//! Every user-facing operation of the TUI.
//!
//! An [`Action`] is what a key press (via the [`crate::keymap::Keymap`]), a palette
//! entry or a mouse gesture resolves to. Actions carry no data; the target (cursor
//! row, selection) is taken from the state when the action is applied.

use crate::keymap::Context;

/// A tab of the command palette. Every action belongs to exactly one group
/// ([`Action::group`] is an exhaustive match).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    /// Act on downloads: pause, resume, remove, rename, files.
    Downloads,
    /// Queue order and speed limits.
    Queue,
    /// Adding downloads and video quality preferences.
    Add,
    /// Torrent search and its providers.
    Search,
    /// What the list and the detail view show: sorting, filters, tabs.
    View,
    /// Moving the cursor and marking rows.
    Navigate,
    /// The app itself: help, settings, quitting.
    App,
}

impl Group {
    /// Every group, in tab order.
    pub const ALL: &'static [Group] = &[
        Group::Downloads,
        Group::Queue,
        Group::Add,
        Group::Search,
        Group::View,
        Group::Navigate,
        Group::App,
    ];

    /// Tab label.
    pub const fn label(self) -> &'static str {
        match self {
            Group::Downloads => "Downloads",
            Group::Queue => "Queue & Speed",
            Group::Add => "Add & Video",
            Group::Search => "Search",
            Group::View => "View",
            Group::Navigate => "Navigate",
            Group::App => "App",
        }
    }

    /// A shorter tab label for narrow terminals.
    pub const fn short(self) -> &'static str {
        match self {
            Group::Downloads => "Downloads",
            Group::Queue => "Queue",
            Group::Add => "Add",
            Group::Search => "Search",
            Group::View => "View",
            Group::Navigate => "Move",
            Group::App => "App",
        }
    }

    /// The actions of this group, in palette order.
    pub fn actions(self) -> impl Iterator<Item = Action> {
        Action::ALL
            .iter()
            .copied()
            .filter(move |a| a.group() == self)
    }
}

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
    /// Rename the cursor download (or, in the detail Files tab, the file under the cursor).
    Rename,
    /// Delete the partial files of the cursor torrent's deselected files (asks first).
    CleanupUnselected,
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
    /// Show or hide the global speed graph.
    ToggleGraph,
    /// Speed charts: show a shorter window of history.
    SpeedWindowShorter,
    /// Speed charts: show a longer window of history.
    SpeedWindowLonger,
    /// Show or hide removed downloads (history) in the list.
    ToggleRemoved,
    /// Open the detail view of the cursor download.
    OpenDetail,
    /// Detail: next tab.
    DetailNextTab,
    /// Detail: previous tab.
    DetailPrevTab,
    /// Detail: Files tab.
    DetailFiles,
    /// Detail: Peers tab.
    DetailPeers,
    /// Detail: Trackers tab.
    DetailTrackers,
    /// Detail: Pieces tab.
    DetailPieces,
    /// Detail: Speed tab.
    DetailSpeed,
    /// Detail: Options tab.
    DetailOptions,
    /// Detail: toggle the file under the cursor (waiting/paused only).
    DetailToggleFile,
    /// Detail: select / deselect all files (waiting/paused only).
    DetailToggleAllFiles,
    /// Detail: edit the option under the cursor.
    DetailEditOption,
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
    /// Add view: try the URL under the cursor as video (or back as plain http).
    AddToggleVideo,
    /// Add view: show or hide the advanced options.
    AddToggleAdvanced,
    /// Open the search view.
    Search,
    /// Search view: run the query.
    SearchRun,
    /// Search view: edit the query.
    SearchEdit,
    /// Search view: next field.
    SearchNextField,
    /// Search view: previous field.
    SearchPrevField,
    /// Search view: cancel the running search, close the history, or leave.
    SearchCancel,
    /// Search view: open the Add view with the result(s) (history: re-run).
    SearchAdd,
    /// Search view: add the result(s) with the profile defaults.
    SearchAddDirect,
    /// Search view: show or hide the search history.
    SearchHistory,
    /// Search view: clear the search history (confirmed).
    SearchHistoryClear,
    /// Open the search providers settings.
    SearchProviders,
    /// Search view: set the minimum seeders.
    SearchFilterSeeders,
    /// Search view: set the size range.
    SearchFilterSize,
    /// Search view: filter by source provider.
    SearchFilterSource,
    /// Providers view: enable or disable the provider.
    ProviderToggle,
    /// Providers view: test the provider.
    ProviderTest,
    /// Providers view: import custom providers from a file.
    ProviderImport,
    /// Providers view: export custom providers to a file.
    ProviderExport,
    /// Providers view: remove a custom provider (confirmed).
    ProviderRemove,
    /// Open the command palette.
    Palette,
    /// Show the help screen.
    Help,
    /// Close the current view and go back.
    Back,
    /// Leave the TUI (the daemon keeps running).
    Quit,
    /// Switch the video quality picker between "ask every time" and "use defaults".
    VideoQualityToggle,
    /// Set the default video quality (used when the picker is off or skipped).
    VideoDefaultQuality,
    /// Set the default video audio.
    VideoDefaultAudio,
    /// Re-read config.toml in the daemon (limits, schedule, notifications, search, ...).
    ReloadConfig,
    /// Switch mouse support on or off (saved in `[ui] mouse`).
    ToggleMouse,
    /// Undo the last remove-from-list while its toast shows (else unmark).
    Undo,
    /// Show every download.
    ShowAll,
    /// Show only active (downloading / seeding) downloads.
    ShowActive,
    /// Show only waiting / paused downloads.
    ShowWaiting,
    /// Show only finished downloads.
    ShowDone,
    /// Show only failed downloads.
    ShowFailed,
    /// Detail: Info tab (folder, source, hash, comment).
    DetailInfo,
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
        Action::Rename,
        Action::CleanupUnselected,
        Action::MoveUp,
        Action::MoveDown,
        Action::MoveTop,
        Action::MoveBottom,
        Action::LimitUp,
        Action::LimitDown,
        Action::SetMaxConcurrent,
        Action::ToggleGraph,
        Action::SpeedWindowShorter,
        Action::SpeedWindowLonger,
        Action::ToggleRemoved,
        Action::OpenDetail,
        Action::DetailNextTab,
        Action::DetailPrevTab,
        Action::DetailFiles,
        Action::DetailPeers,
        Action::DetailTrackers,
        Action::DetailPieces,
        Action::DetailSpeed,
        Action::DetailOptions,
        Action::DetailToggleFile,
        Action::DetailToggleAllFiles,
        Action::DetailEditOption,
        Action::Add,
        Action::AddSubmit,
        Action::AddNextField,
        Action::AddPrevField,
        Action::AddBrowseFiles,
        Action::AddPasteClipboard,
        Action::AddToggleMirror,
        Action::AddPickDir,
        Action::AddToggleVideo,
        Action::AddToggleAdvanced,
        Action::Search,
        Action::SearchRun,
        Action::SearchEdit,
        Action::SearchNextField,
        Action::SearchPrevField,
        Action::SearchCancel,
        Action::SearchAdd,
        Action::SearchAddDirect,
        Action::SearchHistory,
        Action::SearchHistoryClear,
        Action::SearchProviders,
        Action::SearchFilterSeeders,
        Action::SearchFilterSize,
        Action::SearchFilterSource,
        Action::ProviderToggle,
        Action::ProviderTest,
        Action::ProviderImport,
        Action::ProviderExport,
        Action::ProviderRemove,
        Action::Palette,
        Action::Help,
        Action::Back,
        Action::Quit,
        Action::VideoQualityToggle,
        Action::VideoDefaultQuality,
        Action::VideoDefaultAudio,
        Action::ReloadConfig,
        Action::ToggleMouse,
        Action::Undo,
        Action::ShowAll,
        Action::ShowActive,
        Action::ShowWaiting,
        Action::ShowDone,
        Action::ShowFailed,
        Action::DetailInfo,
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
            Action::Rename => "rename",
            Action::CleanupUnselected => "cleanup_unselected",
            Action::MoveUp => "move_up",
            Action::MoveDown => "move_down",
            Action::MoveTop => "move_top",
            Action::MoveBottom => "move_bottom",
            Action::LimitUp => "limit_up",
            Action::LimitDown => "limit_down",
            Action::SetMaxConcurrent => "max_concurrent",
            Action::ToggleGraph => "toggle_graph",
            Action::SpeedWindowShorter => "speed_window_shorter",
            Action::SpeedWindowLonger => "speed_window_longer",
            Action::ToggleRemoved => "toggle_removed",
            Action::OpenDetail => "detail",
            Action::DetailNextTab => "detail_next_tab",
            Action::DetailPrevTab => "detail_prev_tab",
            Action::DetailFiles => "detail_files",
            Action::DetailPeers => "detail_peers",
            Action::DetailTrackers => "detail_trackers",
            Action::DetailPieces => "detail_pieces",
            Action::DetailSpeed => "detail_speed",
            Action::DetailOptions => "detail_options",
            Action::DetailToggleFile => "detail_toggle_file",
            Action::DetailToggleAllFiles => "detail_toggle_all_files",
            Action::DetailEditOption => "detail_edit_option",
            Action::Add => "add",
            Action::AddSubmit => "add_submit",
            Action::AddNextField => "add_next_field",
            Action::AddPrevField => "add_prev_field",
            Action::AddBrowseFiles => "add_browse_files",
            Action::AddPasteClipboard => "add_paste_clipboard",
            Action::AddToggleMirror => "add_toggle_mirror",
            Action::AddPickDir => "add_pick_dir",
            Action::AddToggleVideo => "add_toggle_video",
            Action::AddToggleAdvanced => "add_toggle_advanced",
            Action::Search => "search",
            Action::SearchRun => "search_run",
            Action::SearchEdit => "search_edit",
            Action::SearchNextField => "search_next_field",
            Action::SearchPrevField => "search_prev_field",
            Action::SearchCancel => "search_cancel",
            Action::SearchAdd => "search_add",
            Action::SearchAddDirect => "search_add_direct",
            Action::SearchHistory => "search_history",
            Action::SearchHistoryClear => "search_history_clear",
            Action::SearchProviders => "search_providers",
            Action::SearchFilterSeeders => "search_filter_seeders",
            Action::SearchFilterSize => "search_filter_size",
            Action::SearchFilterSource => "search_filter_source",
            Action::ProviderToggle => "provider_toggle",
            Action::ProviderTest => "provider_test",
            Action::ProviderImport => "provider_import",
            Action::ProviderExport => "provider_export",
            Action::ProviderRemove => "provider_remove",
            Action::Palette => "palette",
            Action::Help => "help",
            Action::Back => "back",
            Action::Quit => "quit",
            Action::VideoQualityToggle => "video_quality_toggle",
            Action::VideoDefaultQuality => "video_default_quality",
            Action::VideoDefaultAudio => "video_default_audio",
            Action::ReloadConfig => "reload_config",
            Action::ToggleMouse => "toggle_mouse",
            Action::Undo => "undo",
            Action::ShowAll => "show_all",
            Action::ShowActive => "show_active",
            Action::ShowWaiting => "show_waiting",
            Action::ShowDone => "show_done",
            Action::ShowFailed => "show_failed",
            Action::DetailInfo => "detail_info",
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
            Action::Remove => "Remove from the list (keep files)",
            Action::RemoveDelete => "Remove and DELETE the files from disk (asks first)",
            Action::Purge => "Clear finished/failed downloads from the list (keep files)",
            Action::Rename => "Rename (not while active; file under cursor in Detail)",
            Action::CleanupUnselected => "Delete unselected partial files of a finished torrent",
            Action::MoveUp => "Move up in queue",
            Action::MoveDown => "Move down in queue",
            Action::MoveTop => "Move to top of queue",
            Action::MoveBottom => "Move to bottom of queue",
            Action::LimitUp => "Raise global download limit",
            Action::LimitDown => "Lower global download limit",
            Action::SetMaxConcurrent => "Set how many downloads run at the same time",
            Action::ToggleGraph => "Show / hide the global speed graph",
            Action::SpeedWindowShorter => "Speed charts: show a shorter time window",
            Action::SpeedWindowLonger => "Speed charts: show a longer time window",
            Action::ToggleRemoved => "Show / hide removed downloads (history)",
            Action::OpenDetail => "Open details",
            Action::DetailNextTab => "Detail: next tab",
            Action::DetailPrevTab => "Detail: previous tab",
            Action::DetailFiles => "Detail: Files tab",
            Action::DetailPeers => "Detail: Peers tab",
            Action::DetailTrackers => "Detail: Trackers tab",
            Action::DetailPieces => "Detail: Pieces tab",
            Action::DetailSpeed => "Detail: Speed tab",
            Action::DetailOptions => "Detail: Options tab",
            Action::DetailToggleFile => {
                "Detail: toggle the file under the cursor (waiting/paused only)"
            }
            Action::DetailToggleAllFiles => {
                "Detail: select / deselect all files (waiting/paused only)"
            }
            Action::DetailEditOption => "Detail: edit the option under the cursor",
            Action::Add => "Add downloads",
            Action::AddSubmit => "Add view: review, then add and start (Enter again)",
            Action::AddNextField => "Add view: next field",
            Action::AddPrevField => "Add view: previous field",
            Action::AddBrowseFiles => "Add view: browse local .torrent/.metalink files",
            Action::AddPasteClipboard => "Add view: paste from clipboard",
            Action::AddToggleMirror => {
                "Add view: mirrors on/off (all URLs are copies of one file; fetch from all)"
            }
            Action::AddPickDir => "Add view: choose the download folder (on the daemon's machine)",
            Action::AddToggleVideo => "Add view: try this URL as video (yt-dlp) / as plain http",
            Action::AddToggleAdvanced => "Add view: show / hide advanced options",
            Action::Search => "Search torrents",
            Action::SearchRun => "Search: run the query",
            Action::SearchEdit => "Search: new search (edit the query)",
            Action::SearchNextField => "Search: next field",
            Action::SearchPrevField => "Search: previous field",
            Action::SearchCancel => "Search: cancel the running search (leave when idle)",
            Action::SearchAdd => "Search: open result(s) in the Add view to change options first",
            Action::SearchAddDirect => "Search: download result(s) now with default options",
            Action::SearchHistory => "Search: history",
            Action::SearchHistoryClear => "Search: clear the history",
            Action::SearchProviders => "Search providers",
            Action::SearchFilterSeeders => "Search: minimum seeders",
            Action::SearchFilterSize => "Search: size range",
            Action::SearchFilterSource => "Search: filter by source provider",
            Action::ProviderToggle => "Providers: enable / disable",
            Action::ProviderTest => "Providers: test the provider",
            Action::ProviderImport => "Providers: import from a file",
            Action::ProviderExport => "Providers: export to a file",
            Action::ProviderRemove => "Providers: remove a custom provider",
            Action::Palette => "Command palette",
            Action::Help => "Help (key bindings)",
            Action::Back => "Back / close",
            Action::Quit => "Quit the TUI (daemon keeps running)",
            Action::VideoQualityToggle => "Video quality picker: ask every time / use defaults",
            Action::VideoDefaultQuality => {
                "Video default quality (best, 1080p, 720p, audio:m4a, …)"
            }
            Action::VideoDefaultAudio => "Video default audio (best, m4a, opus, mp3)",
            Action::ReloadConfig => "Reload config.toml",
            Action::ToggleMouse => "Mouse on / off (click rows, tabs, buttons; saved)",
            Action::Undo => "Undo the last remove from the list (while its notice shows)",
            Action::ShowAll => "Show all downloads",
            Action::ShowActive => "Show only active downloads",
            Action::ShowWaiting => "Show only waiting and paused downloads",
            Action::ShowDone => "Show only finished downloads",
            Action::ShowFailed => "Show only failed downloads",
            Action::DetailInfo => "Detail: Info tab (folder, source, hash)",
            Action::Shutdown => "Shut down the daemon and engines",
        }
    }

    /// The palette group of this action.
    pub const fn group(self) -> Group {
        match self {
            Action::Pause
            | Action::Resume
            | Action::PauseAll
            | Action::ResumeAll
            | Action::Remove
            | Action::RemoveDelete
            | Action::Purge
            | Action::Rename
            | Action::CleanupUnselected
            | Action::OpenDetail
            | Action::Undo
            | Action::DetailToggleFile
            | Action::DetailToggleAllFiles
            | Action::DetailEditOption => Group::Downloads,
            Action::MoveUp
            | Action::MoveDown
            | Action::MoveTop
            | Action::MoveBottom
            | Action::LimitUp
            | Action::LimitDown
            | Action::SetMaxConcurrent => Group::Queue,
            Action::Add
            | Action::AddSubmit
            | Action::AddNextField
            | Action::AddPrevField
            | Action::AddBrowseFiles
            | Action::AddPasteClipboard
            | Action::AddToggleMirror
            | Action::AddPickDir
            | Action::AddToggleVideo
            | Action::AddToggleAdvanced
            | Action::VideoQualityToggle
            | Action::VideoDefaultQuality
            | Action::VideoDefaultAudio => Group::Add,
            Action::Search
            | Action::SearchRun
            | Action::SearchEdit
            | Action::SearchNextField
            | Action::SearchPrevField
            | Action::SearchCancel
            | Action::SearchAdd
            | Action::SearchAddDirect
            | Action::SearchHistory
            | Action::SearchHistoryClear
            | Action::SearchProviders
            | Action::SearchFilterSeeders
            | Action::SearchFilterSize
            | Action::SearchFilterSource
            | Action::ProviderToggle
            | Action::ProviderTest
            | Action::ProviderImport
            | Action::ProviderExport
            | Action::ProviderRemove => Group::Search,
            Action::SortNext
            | Action::SortReverse
            | Action::Filter
            | Action::ClearFilter
            | Action::StoppedNextPage
            | Action::StoppedPrevPage
            | Action::ToggleGraph
            | Action::SpeedWindowShorter
            | Action::SpeedWindowLonger
            | Action::ToggleRemoved
            | Action::ShowAll
            | Action::ShowActive
            | Action::ShowWaiting
            | Action::ShowDone
            | Action::ShowFailed
            | Action::DetailNextTab
            | Action::DetailPrevTab
            | Action::DetailFiles
            | Action::DetailPeers
            | Action::DetailTrackers
            | Action::DetailPieces
            | Action::DetailSpeed
            | Action::DetailOptions
            | Action::DetailInfo => Group::View,
            Action::Up
            | Action::Down
            | Action::PageUp
            | Action::PageDown
            | Action::Top
            | Action::Bottom
            | Action::ToggleSelect
            | Action::SelectAll
            | Action::SelectNone
            | Action::SelectRange
            | Action::Back => Group::Navigate,
            Action::Palette
            | Action::Help
            | Action::Quit
            | Action::ReloadConfig
            | Action::ToggleMouse
            | Action::Shutdown => Group::App,
        }
    }

    /// The palette's wording: a short name and, where the full title says
    /// more, a description line. Derived from [`Action::title`]: the view
    /// prefix ("Detail: ") goes (the group tab says where it belongs) and a
    /// trailing parenthesis becomes the description.
    pub fn palette_text(self) -> (String, Option<String>) {
        let own = |name: &str, desc: &str| (name.to_owned(), Some(desc.to_owned()));
        match self {
            Action::Purge => {
                return own(
                    "Clear finished downloads",
                    "Drops finished and failed downloads from the list; files are kept",
                );
            }
            Action::RemoveDelete => {
                return own(
                    "Remove and DELETE files",
                    "Deletes the files from disk; asks first",
                );
            }
            Action::CleanupUnselected => {
                return own(
                    "Delete unselected partial files",
                    "Of a finished torrent whose files you deselected; asks first",
                );
            }
            Action::SearchAdd => {
                return own(
                    "Open results in the Add view",
                    "Change options before adding",
                );
            }
            Action::SearchAddDirect => {
                return own("Download results now", "With your default options");
            }
            Action::VideoQualityToggle => {
                return own(
                    "Video quality picker on/off",
                    "Ask every time, or use the defaults",
                );
            }
            Action::AddToggleVideo => {
                return own(
                    "Try as video / as plain http",
                    "Downloads the URL with yt-dlp, or back",
                );
            }
            Action::SetMaxConcurrent => {
                return own(
                    "Max downloads at once",
                    "How many downloads run at the same time",
                );
            }
            _ => {}
        }
        let mut t = self.title();
        for p in ["Detail: ", "Add view: ", "Search: ", "Providers: "] {
            if let Some(rest) = t.strip_prefix(p) {
                t = rest;
            }
        }
        let (name, desc) = match t.split_once(" (") {
            Some((name, rest)) if rest.ends_with(')') => (name, Some(&rest[..rest.len() - 1])),
            _ => (t, None),
        };
        (capitalize(name), desc.map(capitalize))
    }

    /// One-word label for the footer.
    pub const fn short(self) -> &'static str {
        match self {
            Action::Up => "up",
            Action::Down => "down",
            Action::PageUp => "page up",
            Action::PageDown => "page down",
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
            Action::RemoveDelete => "delete files",
            Action::Purge => "purge",
            Action::Rename => "rename",
            Action::CleanupUnselected => "cleanup",
            Action::MoveUp => "queue up",
            Action::MoveDown => "queue down",
            Action::MoveTop => "queue top",
            Action::MoveBottom => "queue bottom",
            Action::LimitUp => "limit+",
            Action::LimitDown => "limit-",
            Action::SetMaxConcurrent => "max at once",
            Action::ToggleGraph => "graph",
            Action::SpeedWindowShorter => "shorter window",
            Action::SpeedWindowLonger => "longer window",
            Action::ToggleRemoved => "history",
            Action::OpenDetail => "details",
            Action::DetailNextTab => "next tab",
            Action::DetailPrevTab => "prev tab",
            Action::DetailFiles => "files",
            Action::DetailPeers => "peers",
            Action::DetailTrackers => "trackers",
            Action::DetailPieces => "pieces",
            Action::DetailSpeed => "speed",
            Action::DetailOptions => "options",
            Action::DetailToggleFile => "toggle file",
            Action::DetailToggleAllFiles => "all files",
            Action::DetailEditOption => "edit",
            Action::Add => "add",
            Action::AddSubmit => "review & add",
            Action::AddNextField => "next field",
            Action::AddPrevField => "prev field",
            Action::AddBrowseFiles => "files",
            Action::AddPasteClipboard => "clipboard",
            Action::AddToggleMirror => "mirror",
            Action::AddPickDir => "folder",
            Action::AddToggleVideo => "video",
            Action::AddToggleAdvanced => "more options",
            Action::Search => "search",
            Action::SearchRun => "search",
            Action::SearchEdit => "new search",
            Action::SearchNextField => "next field",
            Action::SearchPrevField => "prev field",
            Action::SearchCancel => "cancel",
            Action::SearchAdd => "edit first",
            Action::SearchAddDirect => "download",
            Action::SearchHistory => "history",
            Action::SearchHistoryClear => "clear history",
            Action::SearchProviders => "providers",
            Action::SearchFilterSeeders => "min seeders",
            Action::SearchFilterSize => "size",
            Action::SearchFilterSource => "source",
            Action::ProviderToggle => "toggle",
            Action::ProviderTest => "test",
            Action::ProviderImport => "import",
            Action::ProviderExport => "export",
            Action::ProviderRemove => "remove",
            Action::Palette => "palette",
            Action::Help => "help",
            Action::Back => "back",
            Action::Quit => "quit",
            Action::VideoQualityToggle => "quality picker",
            Action::VideoDefaultQuality => "default quality",
            Action::VideoDefaultAudio => "default audio",
            Action::ReloadConfig => "reload",
            Action::ToggleMouse => "mouse",
            Action::Undo => "undo",
            Action::ShowAll => "all",
            Action::ShowActive => "active",
            Action::ShowWaiting => "waiting",
            Action::ShowDone => "done",
            Action::ShowFailed => "failed",
            Action::DetailInfo => "info",
            Action::Shutdown => "shutdown",
        }
    }

    /// The context whose default keymap holds this action; used to show its key
    /// in the palette.
    pub const fn home_context(self) -> Context {
        match self {
            Action::Palette
            | Action::Help
            | Action::Quit
            | Action::VideoQualityToggle
            | Action::VideoDefaultQuality
            | Action::VideoDefaultAudio
            | Action::ReloadConfig
            | Action::ToggleMouse
            | Action::Shutdown => Context::Global,
            Action::Back
            | Action::DetailNextTab
            | Action::DetailPrevTab
            | Action::DetailFiles
            | Action::DetailPeers
            | Action::DetailTrackers
            | Action::DetailPieces
            | Action::DetailSpeed
            | Action::DetailOptions
            | Action::DetailInfo
            | Action::DetailToggleFile
            | Action::DetailToggleAllFiles
            | Action::DetailEditOption => Context::Detail,
            Action::AddSubmit
            | Action::AddNextField
            | Action::AddPrevField
            | Action::AddBrowseFiles
            | Action::AddPasteClipboard
            | Action::AddToggleMirror
            | Action::AddToggleVideo
            | Action::AddToggleAdvanced
            | Action::AddPickDir => Context::Add,
            Action::SearchRun
            | Action::SearchEdit
            | Action::SearchNextField
            | Action::SearchPrevField
            | Action::SearchCancel
            | Action::SearchAdd
            | Action::SearchAddDirect
            | Action::SearchHistory
            | Action::SearchHistoryClear
            | Action::SearchProviders
            | Action::SearchFilterSeeders
            | Action::SearchFilterSize
            | Action::SearchFilterSource => Context::Search,
            Action::ProviderToggle
            | Action::ProviderTest
            | Action::ProviderImport
            | Action::ProviderExport
            | Action::ProviderRemove => Context::Providers,
            _ => Context::Downloads,
        }
    }

    /// Whether the action talks to the daemon and is therefore disabled while
    /// disconnected (actions are never queued).
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
                | Action::Rename
                | Action::CleanupUnselected
                | Action::ReloadConfig
                | Action::VideoQualityToggle
                | Action::VideoDefaultQuality
                | Action::VideoDefaultAudio
                | Action::Shutdown
                | Action::DetailToggleFile
                | Action::DetailToggleAllFiles
                | Action::DetailEditOption
                | Action::AddPickDir
                | Action::SearchRun
                | Action::SearchAddDirect
                | Action::SearchHistoryClear
                | Action::ProviderToggle
                | Action::ProviderTest
                | Action::ProviderImport
                | Action::ProviderExport
                | Action::ProviderRemove
        )
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_belongs_to_exactly_one_group() {
        let mut seen = std::collections::HashSet::new();
        for g in Group::ALL {
            for a in g.actions() {
                assert!(seen.insert(a), "{a:?} is in two groups");
            }
        }
        assert_eq!(seen.len(), Action::ALL.len(), "an action has no group");
        assert!(
            Group::ALL.len() < 8,
            "the palette has at most 8 tabs with All"
        );
        for g in Group::ALL {
            assert!(g.actions().count() > 0, "{g:?} is empty");
        }
    }

    #[test]
    fn palette_text_splits_names_and_descriptions() {
        assert_eq!(
            Action::Rename.palette_text(),
            (
                "Rename".to_owned(),
                Some("Not while active; file under cursor in Detail".to_owned())
            )
        );
        assert_eq!(
            Action::DetailNextTab.palette_text(),
            ("Next tab".to_owned(), None)
        );
        assert_eq!(
            Action::PauseAll.palette_text(),
            ("Pause all".to_owned(), None)
        );
        for a in Action::ALL {
            let (name, _) = a.palette_text();
            assert!(!name.is_empty() && name.len() <= 40, "{a:?}: {name}");
        }
    }

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
