//! The two pickers of the add view: a browser for LOCAL `.torrent`/`.metalink`
//! files and a picker for DAEMON directories (decision Q6).
//!
//! Both are pure state machines. Keys return an outcome that asks the caller
//! for a directory listing (a runtime command for the local one, `fs.list` for
//! the daemon one), so nothing here touches the filesystem except
//! [`read_local_dir`], which the runtime calls off the UI thread.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dman_proto::method::{FsEntry, FsListing};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::format::{size, truncate};
use crate::theme::Theme;
use crate::widgets::centered;

/// Most entries kept per local directory.
pub const LOCAL_CAP: usize = 5000;
const PAGE: usize = 10;

/// An entry of a local directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseEntry {
    /// File or directory name.
    pub name: String,
    /// A directory (symlinks are followed).
    pub is_dir: bool,
    /// Bytes of a file.
    pub size: Option<u64>,
}

/// A local directory listing (answer to `Cmd::ReadDir`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalListing {
    /// Canonical directory.
    pub dir: PathBuf,
    /// Directories first, then files, case-insensitively by name.
    pub entries: Vec<BrowseEntry>,
    /// The directory had more than [`LOCAL_CAP`] entries.
    pub truncated: bool,
}

/// List a local directory (`None` = the current working directory). Blocking.
///
/// # Errors
/// The I/O error as text.
pub fn read_local_dir(path: Option<&Path>) -> Result<LocalListing, String> {
    let start = match path {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?,
    };
    let dir = std::fs::canonicalize(&start).map_err(|e| format!("{}: {e}", start.display()))?;
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for item in rd.flatten() {
        let Ok(meta) = std::fs::metadata(item.path()).or_else(|_| item.metadata()) else {
            continue;
        };
        if entries.len() >= LOCAL_CAP {
            truncated = true;
            break;
        }
        entries.push(BrowseEntry {
            name: item.file_name().to_string_lossy().into_owned(),
            is_dir: meta.is_dir(),
            size: meta.is_file().then_some(meta.len()),
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(LocalListing {
        dir,
        entries,
        truncated,
    })
}

/// Whether a file name is a source file the browser shows by default.
pub fn is_source_file(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    l.ends_with(".torrent") || l.ends_with(".metalink") || l.ends_with(".meta4")
}

/// Cursor navigation shared by both pickers; `true` = key handled.
fn navigate(key: KeyEvent, cursor: &mut usize, len: usize) -> bool {
    let last = len.saturating_sub(1);
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => *cursor = (*cursor + 1).min(last),
        KeyCode::Char('k') | KeyCode::Up => *cursor = cursor.saturating_sub(1),
        KeyCode::PageDown => *cursor = (*cursor + PAGE).min(last),
        KeyCode::PageUp => *cursor = cursor.saturating_sub(PAGE),
        KeyCode::Char('g') | KeyCode::Home => *cursor = 0,
        KeyCode::Char('G') | KeyCode::End => *cursor = last,
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            *cursor = (*cursor + PAGE).min(last);
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            *cursor = cursor.saturating_sub(PAGE);
        }
        _ => return false,
    }
    true
}

/// What a key did to the local file browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOutcome {
    /// Nothing for the caller to do.
    Stay,
    /// Close the browser.
    Close,
    /// List this directory (`None` = working directory); answer with
    /// [`FileBrowser::on_listing`].
    List(Option<PathBuf>),
    /// Close and append these paths as source lines.
    Pick(Vec<String>),
}

/// Browser for local source files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileBrowser {
    /// Directory shown (empty until the first listing arrives).
    pub dir: PathBuf,
    entries: Vec<BrowseEntry>,
    /// Cursor index into [`FileBrowser::visible`].
    pub cursor: usize,
    /// Show every file, not just `.torrent`/`.metalink`/`.meta4`.
    pub show_all: bool,
    /// Files marked with Space (full paths, across directories).
    pub marked: BTreeSet<PathBuf>,
    /// A listing was requested and has not arrived.
    pub loading: bool,
    /// The last listing failed.
    pub error: Option<String>,
    /// The listing was cut at [`LOCAL_CAP`].
    pub truncated: bool,
    /// Name to put the cursor on after the next listing (the directory we left).
    select_after: Option<String>,
}

impl FileBrowser {
    /// A browser waiting for the working directory listing.
    pub fn new() -> Self {
        Self {
            loading: true,
            ..Self::default()
        }
    }

    /// Entries shown with the current filter.
    pub fn visible(&self) -> Vec<&BrowseEntry> {
        self.entries
            .iter()
            .filter(|e| e.is_dir || self.show_all || is_source_file(&e.name))
            .collect()
    }

    fn path_of(&self, e: &BrowseEntry) -> PathBuf {
        self.dir.join(&e.name)
    }

    /// Feed a listing (or its error).
    pub fn on_listing(&mut self, result: Result<LocalListing, String>) {
        self.loading = false;
        match result {
            Ok(l) => {
                self.error = None;
                self.truncated = l.truncated;
                self.dir = l.dir;
                self.entries = l.entries;
                self.cursor = self
                    .select_after
                    .take()
                    .and_then(|n| self.visible().iter().position(|e| e.name == n))
                    .unwrap_or(0);
            }
            Err(e) => {
                self.select_after = None;
                self.error = Some(e);
            }
        }
    }

    /// Handle a key.
    pub fn key(&mut self, key: KeyEvent) -> FileOutcome {
        let len = self.visible().len();
        if navigate(key, &mut self.cursor, len) {
            return FileOutcome::Stay;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => FileOutcome::Close,
            KeyCode::Char('.') => {
                let name = self.visible().get(self.cursor).map(|e| e.name.clone());
                self.show_all = !self.show_all;
                self.cursor = name
                    .and_then(|n| self.visible().iter().position(|e| e.name == n))
                    .unwrap_or(0);
                FileOutcome::Stay
            }
            KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Left => self.go_up(),
            KeyCode::Char('l') | KeyCode::Right => match self.cursor_entry() {
                Some(e) if e.is_dir => self.descend(&e),
                _ => FileOutcome::Stay,
            },
            KeyCode::Char(' ') => {
                if let Some(e) = self.cursor_entry()
                    && !e.is_dir
                {
                    let p = self.path_of(&e);
                    if !self.marked.remove(&p) {
                        self.marked.insert(p);
                    }
                    self.cursor = (self.cursor + 1).min(len.saturating_sub(1));
                }
                FileOutcome::Stay
            }
            KeyCode::Enter => match self.cursor_entry() {
                Some(e) if e.is_dir => self.descend(&e),
                Some(e) => {
                    let paths: Vec<PathBuf> = if self.marked.is_empty() {
                        vec![self.path_of(&e)]
                    } else {
                        self.marked.iter().cloned().collect()
                    };
                    FileOutcome::Pick(paths.iter().map(|p| p.display().to_string()).collect())
                }
                // Enter on an empty directory with marks picks the marks.
                None if !self.marked.is_empty() => FileOutcome::Pick(
                    self.marked
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect(),
                ),
                None => FileOutcome::Stay,
            },
            _ => FileOutcome::Stay,
        }
    }

    fn cursor_entry(&self) -> Option<BrowseEntry> {
        self.visible().get(self.cursor).map(|e| (*e).clone())
    }

    fn descend(&mut self, e: &BrowseEntry) -> FileOutcome {
        self.loading = true;
        FileOutcome::List(Some(self.path_of(e)))
    }

    fn go_up(&mut self) -> FileOutcome {
        match self.dir.parent() {
            Some(parent) => {
                self.select_after = self
                    .dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned());
                self.loading = true;
                FileOutcome::List(Some(parent.to_path_buf()))
            }
            None => FileOutcome::Stay,
        }
    }

    /// Footer hints.
    pub fn hints() -> Vec<(String, String)> {
        hints(&[
            ("Enter", "open/pick"),
            ("Space", "mark"),
            ("⌫/h", "up"),
            (".", "all files"),
            ("Esc", "close"),
        ])
    }

    /// Draw the browser.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let rows: Vec<Row> = self
            .visible()
            .iter()
            .map(|e| {
                let marked = !e.is_dir && self.marked.contains(&self.path_of(e));
                Row {
                    mark: if marked { "[x]" } else { "   " },
                    name: if e.is_dir {
                        format!("{}/", e.name)
                    } else {
                        e.name.clone()
                    },
                    right: e.size.map(size).unwrap_or_default(),
                    dir: e.is_dir,
                }
            })
            .collect();
        let empty = if self.show_all {
            "(empty)"
        } else {
            "(no .torrent / .metalink / .meta4 files here; `.` shows all files)"
        };
        let mut head = format!("{}", self.dir.display());
        if self.truncated {
            head.push_str(" (list cut)");
        }
        let marks = if self.marked.is_empty() {
            String::new()
        } else {
            format!(" · {} marked", self.marked.len())
        };
        render_popup(
            frame,
            area,
            theme,
            &format!(" Add local file{marks} "),
            &head,
            &rows,
            self.cursor,
            self.error.as_deref(),
            self.loading,
            empty,
        );
    }
}

/// What a key did to the daemon directory picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirOutcome {
    /// Nothing for the caller to do.
    Stay,
    /// Close the picker.
    Close,
    /// Ask the daemon for this directory (`None` = profile default); answer
    /// with [`DirPicker::on_listing`].
    List(Option<String>),
    /// Close and use this directory.
    Choose(String),
}

/// Picker for a directory on the daemon host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirPicker {
    /// Canonical directory shown.
    pub path: String,
    /// Its parent, if any.
    pub parent: Option<String>,
    /// Subdirectories.
    pub entries: Vec<FsEntry>,
    /// Cursor index.
    pub cursor: usize,
    /// A listing was requested and has not arrived.
    pub loading: bool,
    /// The last listing failed.
    pub error: Option<String>,
    /// The daemon cut the list.
    pub truncated: bool,
    select_after: Option<String>,
}

/// Join a directory and a name in the style of the daemon's paths.
pub fn join_remote(base: &str, name: &str) -> String {
    if base.ends_with('/') || base.ends_with('\\') {
        format!("{base}{name}")
    } else if base.contains('\\') && !base.contains('/') {
        format!("{base}\\{name}")
    } else {
        format!("{base}/{name}")
    }
}

impl DirPicker {
    /// A picker waiting for its first listing.
    pub fn new() -> Self {
        Self {
            loading: true,
            ..Self::default()
        }
    }

    /// Feed a listing (or the daemon's error).
    pub fn on_listing(&mut self, result: Result<FsListing, String>) {
        self.loading = false;
        match result {
            Ok(l) => {
                self.error = None;
                self.truncated = l.truncated;
                self.path = l.path;
                self.parent = l.parent;
                self.entries = l.entries.into_iter().filter(|e| e.is_dir).collect();
                self.cursor = self
                    .select_after
                    .take()
                    .and_then(|n| self.entries.iter().position(|e| e.name == n))
                    .unwrap_or(0);
            }
            Err(e) => {
                self.select_after = None;
                self.error = Some(e);
            }
        }
    }

    /// Handle a key.
    pub fn key(&mut self, key: KeyEvent) -> DirOutcome {
        if navigate(key, &mut self.cursor, self.entries.len()) {
            return DirOutcome::Stay;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => DirOutcome::Close,
            KeyCode::Char(' ') | KeyCode::Char('c') => self.choose(),
            KeyCode::Char('s') if ctrl => self.choose(),
            KeyCode::Char('~') => {
                self.loading = true;
                DirOutcome::List(None)
            }
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                match self.entries.get(self.cursor) {
                    Some(e) if !self.path.is_empty() => {
                        let target = join_remote(&self.path, &e.name);
                        self.loading = true;
                        DirOutcome::List(Some(target))
                    }
                    _ => DirOutcome::Stay,
                }
            }
            KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Left => match &self.parent {
                Some(p) => {
                    let p = p.clone();
                    self.select_after = self
                        .path
                        .trim_end_matches(['/', '\\'])
                        .rsplit(['/', '\\'])
                        .next()
                        .map(str::to_owned);
                    self.loading = true;
                    DirOutcome::List(Some(p))
                }
                None => DirOutcome::Stay,
            },
            _ => DirOutcome::Stay,
        }
    }

    fn choose(&self) -> DirOutcome {
        if self.path.is_empty() {
            DirOutcome::Stay
        } else {
            DirOutcome::Choose(self.path.clone())
        }
    }

    /// Footer hints.
    pub fn hints() -> Vec<(String, String)> {
        hints(&[
            ("Space", "use this dir"),
            ("Enter", "open"),
            ("⌫/h", "up"),
            ("~", "default"),
            ("Esc", "close"),
        ])
    }

    /// Draw the picker.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let rows: Vec<Row> = self
            .entries
            .iter()
            .map(|e| Row {
                mark: "   ",
                name: format!("{}/", e.name),
                right: String::new(),
                dir: true,
            })
            .collect();
        let mut head = self.path.clone();
        if self.truncated {
            head.push_str(" (list cut)");
        }
        render_popup(
            frame,
            area,
            theme,
            " Download directory (on the daemon host) ",
            &head,
            &rows,
            self.cursor,
            self.error.as_deref(),
            self.loading,
            "(no subdirectories)",
        );
    }
}

fn hints(h: &[(&str, &str)]) -> Vec<(String, String)> {
    h.iter()
        .map(|(k, l)| ((*k).to_owned(), (*l).to_owned()))
        .collect()
}

struct Row {
    mark: &'static str,
    name: String,
    right: String,
    dir: bool,
}

#[allow(clippy::too_many_arguments)]
fn render_popup(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    title: &str,
    head: &str,
    rows: &[Row],
    cursor: usize,
    error: Option<&str>,
    loading: bool,
    empty: &str,
) {
    let rect = centered(
        area,
        area.width.saturating_sub(6).min(90),
        area.height.saturating_sub(2).min(30),
    );
    let inner = crate::widgets::modal_frame(
        frame,
        rect,
        theme,
        Line::from(Span::styled(
            title.to_owned(),
            theme.accent.patch(theme.surface_bg()),
        )),
        theme.border_focus,
    );
    let width = usize::from(inner.width);
    let mut lines = vec![Line::styled(truncate_left(head, width), theme.accent)];
    if let Some(e) = error {
        lines.push(Line::styled(truncate(e, width), theme.error));
    } else if loading {
        lines.push(Line::styled("loading…", theme.dim));
    } else if rows.is_empty() {
        lines.push(Line::styled(truncate(empty, width), theme.dim));
    } else {
        lines.push(Line::default());
    }
    let list_h = usize::from(inner.height).saturating_sub(lines.len());
    let top = (cursor + 1).saturating_sub(list_h);
    for (i, r) in rows.iter().enumerate().skip(top).take(list_h) {
        let right_w = r.right.chars().count();
        let name_w = width.saturating_sub(4 + right_w + 1);
        let text = format!(
            "{} {:<name_w$} {}",
            r.mark,
            truncate(&r.name, name_w),
            r.right
        );
        let style = if i == cursor {
            theme.cursor
        } else if r.dir {
            theme.accent
        } else {
            theme.text
        };
        lines.push(Line::styled(text, style));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Keep the end of a long path.
fn truncate_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_owned()
    } else {
        let tail: String = s.chars().skip(n - max.saturating_sub(1)).collect();
        format!("…{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        k(KeyCode::Char(c))
    }

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::create_dir(d.path().join("Another")).unwrap();
        std::fs::write(d.path().join("b.torrent"), b"x").unwrap();
        std::fs::write(d.path().join("a.META4"), b"x").unwrap();
        std::fs::write(d.path().join("notes.txt"), b"x").unwrap();
        std::fs::write(d.path().join("sub/inner.metalink"), b"x").unwrap();
        d
    }

    fn open(path: &Path) -> FileBrowser {
        let mut b = FileBrowser::new();
        b.on_listing(read_local_dir(Some(path)));
        b
    }

    fn names(b: &FileBrowser) -> Vec<String> {
        b.visible().iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn lists_dirs_first_and_filters_files() {
        let d = tree();
        let mut b = open(d.path());
        assert_eq!(names(&b), ["Another", "sub", "a.META4", "b.torrent"]);
        b.key(ch('.'));
        assert_eq!(
            names(&b),
            ["Another", "sub", "a.META4", "b.torrent", "notes.txt"]
        );
        b.key(ch('.'));
        assert_eq!(b.visible().len(), 4);
    }

    #[test]
    fn descend_and_go_up_restores_cursor() {
        let d = tree();
        let mut b = open(d.path());
        b.key(ch('j')); // sub
        let out = b.key(k(KeyCode::Enter));
        let FileOutcome::List(Some(p)) = out else {
            panic!("{out:?}")
        };
        assert!(b.loading);
        b.on_listing(read_local_dir(Some(&p)));
        assert_eq!(names(&b), ["inner.metalink"]);
        let out = b.key(k(KeyCode::Backspace));
        let FileOutcome::List(Some(p)) = out else {
            panic!("{out:?}")
        };
        b.on_listing(read_local_dir(Some(&p)));
        assert_eq!(b.cursor, 1, "cursor returns to `sub`");
        assert_eq!(b.dir, std::fs::canonicalize(d.path()).unwrap());
    }

    #[test]
    fn enter_on_file_picks_it_and_space_marks_many() {
        let d = tree();
        let mut b = open(d.path());
        b.key(ch('G'));
        let FileOutcome::Pick(p) = b.key(k(KeyCode::Enter)) else {
            panic!()
        };
        assert_eq!(p.len(), 1);
        assert!(p[0].ends_with("b.torrent"));
        // Mark two files; Enter picks the marks.
        let mut b = open(d.path());
        b.key(ch('j'));
        b.key(ch('j')); // a.META4
        b.key(ch(' '));
        b.key(ch(' '));
        assert_eq!(b.marked.len(), 2);
        b.key(ch('k'));
        b.key(ch(' ')); // unmark b.torrent
        assert_eq!(b.marked.len(), 1);
        b.key(ch('k'));
        b.key(ch(' '));
        let FileOutcome::Pick(p) = b.key(k(KeyCode::Enter)) else {
            panic!()
        };
        assert_eq!(p.len(), 2);
        // Space on a directory does nothing.
        let mut b = open(d.path());
        b.key(ch(' '));
        assert!(b.marked.is_empty());
    }

    #[test]
    fn errors_and_close() {
        let mut b = FileBrowser::new();
        b.on_listing(read_local_dir(Some(Path::new("/definitely/not/here"))));
        assert!(b.error.is_some() && !b.loading);
        assert_eq!(b.key(k(KeyCode::Esc)), FileOutcome::Close);
    }

    #[test]
    fn dir_picker_transitions() {
        let listing = |path: &str, parent: Option<&str>, dirs: &[&str]| FsListing {
            path: path.into(),
            parent: parent.map(Into::into),
            entries: dirs
                .iter()
                .map(|n| FsEntry {
                    name: (*n).into(),
                    is_dir: true,
                    size: None,
                })
                .chain([FsEntry {
                    name: "f".into(),
                    is_dir: false,
                    size: Some(1),
                }])
                .collect(),
            truncated: false,
        };
        let mut p = DirPicker::new();
        p.on_listing(Ok(listing("/data", Some("/"), &["a", "b"])));
        assert_eq!(p.entries.len(), 2, "files are not offered");
        p.key(ch('j'));
        assert_eq!(
            p.key(k(KeyCode::Enter)),
            DirOutcome::List(Some("/data/b".into()))
        );
        p.on_listing(Ok(listing("/data/b", Some("/data"), &[])));
        assert_eq!(p.key(ch(' ')), DirOutcome::Choose("/data/b".into()));
        assert_eq!(
            p.key(k(KeyCode::Backspace)),
            DirOutcome::List(Some("/data".into()))
        );
        p.on_listing(Ok(listing("/data", Some("/"), &["a", "b"])));
        assert_eq!(p.cursor, 1);
        assert_eq!(p.key(ch('~')), DirOutcome::List(None));
        p.on_listing(Err("nope".into()));
        assert_eq!(p.error.as_deref(), Some("nope"));
        assert_eq!(p.key(k(KeyCode::Esc)), DirOutcome::Close);
    }

    #[test]
    fn join_styles() {
        assert_eq!(join_remote("/a", "b"), "/a/b");
        assert_eq!(join_remote("/", "b"), "/b");
        assert_eq!(join_remote(r"C:\a", "b"), r"C:\a\b");
    }
}
