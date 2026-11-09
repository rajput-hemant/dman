//! Turning the form into a plan, an [`AddParams`] and a preview of what will
//! be added (spec: preview before confirming).

use std::collections::HashMap;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use crossterm::event::{KeyCode, KeyEvent};
use dman_proto::{AddParams, AddSource};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::options::{Field, Parsed};
use super::source::{Kind, Token};
use super::torrent::{TorrentMeta, parse_torrent};
use crate::format::{plural, size};
use crate::theme::Theme;
use crate::widgets::centered;

/// One download that will be requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Planned {
    /// One or more URIs of one download (several = mirrors).
    Uris(Vec<Token>),
    /// A local file to upload.
    File(Token),
}

impl Planned {
    /// The source texts this entry came from.
    pub fn tokens(&self) -> Vec<String> {
        match self {
            Planned::Uris(t) => t.iter().map(|t| t.text.clone()).collect(),
            Planned::File(t) => vec![t.text.clone()],
        }
    }
}

/// Group sources into downloads. With `mirror`, every http/https/ftp/sftp URI
/// becomes one download (placed first); magnets and files always stand alone.
pub fn plan(tokens: &[Token], mirror: bool) -> Vec<Planned> {
    let mut out = Vec::new();
    if mirror {
        let uris: Vec<Token> = tokens
            .iter()
            .filter(|t| t.kind.is_mirrorable())
            .cloned()
            .collect();
        if !uris.is_empty() {
            out.push(Planned::Uris(uris));
        }
    }
    for t in tokens {
        if t.kind.is_file() {
            out.push(Planned::File(t.clone()));
        } else if t.kind.is_uri() && !(mirror && t.kind.is_mirrorable()) {
            out.push(Planned::Uris(vec![t.clone()]));
        }
    }
    out
}

/// Checks that need the plan (options meaningful only for some sources).
pub fn plan_errors(plan: &[Planned], parsed: &Parsed) -> Vec<(Field, String)> {
    let mut e = Vec::new();
    if parsed.options.out.is_some() && plan.len() > 1 {
        e.push((
            Field::Out,
            "output name only applies to a single download".into(),
        ));
    }
    if parsed.options.select_files.is_some() {
        let bt = |p: &Planned| match p {
            Planned::File(_) => true,
            Planned::Uris(t) => t.iter().any(|t| t.kind == Kind::Magnet),
        };
        if plan.len() > 1 {
            e.push((
                Field::SelectFiles,
                "file selection applies to a single torrent".into(),
            ));
        } else if !plan.iter().all(bt) {
            e.push((
                Field::SelectFiles,
                "only torrents, magnets and metalinks have files to select".into(),
            ));
        }
    }
    e
}

/// Loaded contents of local files by source text.
pub type Loaded = HashMap<String, Result<Vec<u8>, String>>;

/// Build the request. `Err` names a file that could not be read.
///
/// # Errors
/// A local source that failed to load.
pub fn build_params(
    plan: &[Planned],
    files: &Loaded,
    parsed: &Parsed,
) -> Result<AddParams, String> {
    let mut sources = Vec::new();
    for p in plan {
        sources.push(match p {
            Planned::Uris(t) => AddSource::Uris {
                uris: t.iter().map(|t| t.text.clone()).collect(),
            },
            Planned::File(t) => {
                let data = match files.get(&t.text) {
                    Some(Ok(d)) => d,
                    Some(Err(e)) => return Err(e.clone()),
                    None => return Err(format!("{} was not read", t.text)),
                };
                let name = Path::new(&t.text)
                    .file_name()
                    .map_or_else(|| t.text.clone(), |n| n.to_string_lossy().into_owned());
                let data_base64 = STANDARD.encode(data);
                if t.kind == Kind::TorrentFile {
                    AddSource::Torrent { name, data_base64 }
                } else {
                    AddSource::Metalink { name, data_base64 }
                }
            }
        });
    }
    Ok(AddParams {
        sources,
        options: parsed.options.clone(),
        paused: parsed.paused,
    })
}

/// One line group of the preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewItem {
    /// `torrent`, `magnet`, `http`, `mirrors (3)`, …
    pub kind: String,
    /// Display name.
    pub name: String,
    /// Total size when known.
    pub size: Option<u64>,
    /// Extra lines (info-hash, file list, notes).
    pub details: Vec<String>,
    /// A problem; it blocks confirming when `fatal`.
    pub problem: Option<(String, bool)>,
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3]).ok();
                if let Some(v) = h.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A readable name for a URI (magnet `dn`, last path segment, or host).
pub fn uri_name(uri: &str) -> String {
    if uri.to_ascii_lowercase().starts_with("magnet:") {
        let query = uri.split_once('?').map_or("", |(_, q)| q);
        let param = |key: &str| {
            query
                .split('&')
                .find_map(|kv| kv.strip_prefix(key))
                .map(percent_decode)
        };
        if let Some(dn) = param("dn=").filter(|d| !d.is_empty()) {
            return dn;
        }
        return param("xt=urn:btih:").map_or_else(|| "magnet".into(), |h| format!("btih:{h}"));
    }
    let rest = uri.split_once("://").map_or(uri, |(_, r)| r);
    let no_query = rest.split(['?', '#']).next().unwrap_or(rest);
    let (host, path) = no_query.split_once('/').unwrap_or((no_query, ""));
    match path.trim_end_matches('/').rsplit('/').next() {
        Some(seg) if !seg.is_empty() => percent_decode(seg),
        _ => host.to_owned(),
    }
}

fn torrent_item(token: &Token, meta: &TorrentMeta, select: Option<&[u32]>) -> PreviewItem {
    let mut details = vec![format!("info-hash {}", meta.info_hash)];
    let chosen = |i: usize| select.is_none_or(|s| s.contains(&(u32::try_from(i + 1).unwrap_or(0))));
    let mut shown = 0;
    let mut selected_bytes = 0;
    let mut selected = 0;
    for (i, f) in meta.files.iter().enumerate() {
        let on = chosen(i);
        if on {
            selected += 1;
            selected_bytes += f.length;
        }
        if shown < 12 && (on || select.is_some()) {
            shown += 1;
            details.push(format!(
                "[{}] {:>3}  {}  ({})",
                if on { 'x' } else { ' ' },
                i + 1,
                f.path,
                size(f.length)
            ));
        }
    }
    if meta.files.len() > shown && (select.is_none() || shown == 12) {
        details.push(format!("… and {} more files", meta.files.len() - shown));
    }
    let mut item = PreviewItem {
        kind: "torrent".into(),
        name: meta.name.clone(),
        size: Some(if select.is_some() {
            selected_bytes
        } else {
            meta.total
        }),
        details,
        problem: None,
    };
    if select.is_some() {
        item.details.insert(
            1,
            format!(
                "{} of {} selected",
                selected,
                plural(meta.files.len(), "file")
            ),
        );
        if selected == 0 {
            item.problem = Some(("the selection matches no file".into(), true));
        }
    }
    let _ = token;
    item
}

/// Preview of a plan, given the loaded files.
pub fn build_items(plan: &[Planned], files: &Loaded, parsed: &Parsed) -> Vec<PreviewItem> {
    plan.iter()
        .map(|p| match p {
            Planned::Uris(t) if t.len() == 1 && t[0].kind == Kind::Magnet => PreviewItem {
                kind: "magnet".into(),
                name: uri_name(&t[0].text),
                size: None,
                details: vec!["metadata will be fetched after adding".into()],
                problem: None,
            },
            Planned::Uris(t) if t.len() == 1 => PreviewItem {
                kind: t[0].kind.label(),
                name: uri_name(&t[0].text),
                size: None,
                details: vec![t[0].text.clone()],
                problem: None,
            },
            Planned::Uris(t) => PreviewItem {
                kind: format!("mirrors ({})", t.len()),
                name: uri_name(&t[0].text),
                size: None,
                details: t.iter().map(|t| t.text.clone()).collect(),
                problem: None,
            },
            Planned::File(t) => match files.get(&t.text) {
                Some(Err(e)) => PreviewItem {
                    kind: t.kind.label(),
                    name: t.text.clone(),
                    size: None,
                    details: Vec::new(),
                    problem: Some((e.clone(), true)),
                },
                None => PreviewItem {
                    kind: t.kind.label(),
                    name: t.text.clone(),
                    size: None,
                    details: Vec::new(),
                    problem: Some(("not read".into(), true)),
                },
                Some(Ok(data)) if t.kind == Kind::TorrentFile => match parse_torrent(data) {
                    Ok(meta) => torrent_item(t, &meta, parsed.options.select_files.as_deref()),
                    Err(e) => PreviewItem {
                        kind: t.kind.label(),
                        name: t.text.clone(),
                        size: Some(data.len() as u64),
                        details: Vec::new(),
                        problem: Some((format!("cannot read torrent: {e}"), false)),
                    },
                },
                Some(Ok(data)) => PreviewItem {
                    kind: t.kind.label(),
                    name: Path::new(&t.text)
                        .file_name()
                        .map_or_else(|| t.text.clone(), |n| n.to_string_lossy().into_owned()),
                    size: None,
                    details: vec![format!(
                        "{} file; downloads are listed after adding",
                        size(data.len() as u64)
                    )],
                    problem: None,
                },
            },
        })
        .collect()
}

/// What a key did to the preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewOutcome {
    /// Keep showing it.
    Stay,
    /// Back to the form.
    Back,
    /// Send the request.
    Confirm,
}

/// The preview modal.
#[derive(Debug, Clone, PartialEq)]
pub struct Preview {
    /// What will be added.
    pub items: Vec<PreviewItem>,
    /// The request sent on confirm.
    pub params: AddParams,
    /// Source texts behind each `params.sources` entry (for pruning after a
    /// partial failure).
    pub origins: Vec<Vec<String>>,
    /// First scrolled-off line.
    pub scroll: usize,
}

impl Preview {
    /// Whether confirming is blocked, and why.
    pub fn blocker(&self) -> Option<&str> {
        self.items
            .iter()
            .find_map(|i| i.problem.as_ref().filter(|p| p.1).map(|p| p.0.as_str()))
    }

    /// Handle a key.
    pub fn key(&mut self, key: KeyEvent) -> PreviewOutcome {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n' | 'q') | KeyCode::Backspace => PreviewOutcome::Back,
            KeyCode::Enter | KeyCode::Char('y') if self.blocker().is_none() => {
                PreviewOutcome::Confirm
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll += 1;
                PreviewOutcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll = self.scroll.saturating_sub(1);
                PreviewOutcome::Stay
            }
            KeyCode::PageDown => {
                self.scroll += 10;
                PreviewOutcome::Stay
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(10);
                PreviewOutcome::Stay
            }
            _ => PreviewOutcome::Stay,
        }
    }

    /// Footer hints.
    pub fn hints(&self) -> Vec<(String, String)> {
        let mut h = Vec::new();
        if self.blocker().is_none() {
            h.push(("Enter".to_owned(), "add".to_owned()));
        }
        h.push(("Esc".into(), "back to form".into()));
        h.push(("↑↓".into(), "scroll".into()));
        h
    }

    fn lines(&self, theme: &Theme) -> Vec<Line<'static>> {
        let o = &self.params.options;
        let mut out = vec![Line::from(vec![
            Span::raw(format!("Add {} to ", plural(self.items.len(), "download"))),
            Span::styled(
                o.dir
                    .clone()
                    .unwrap_or_else(|| "the profile's directory".into()),
                theme.accent,
            ),
            Span::raw(if self.params.paused { " (paused)" } else { "" }),
        ])];
        out.push(Line::default());
        for (i, it) in self.items.iter().enumerate() {
            let mut head = vec![
                Span::styled(format!("{}. ", i + 1), theme.dim),
                Span::styled(format!("[{}] ", it.kind), theme.key),
                Span::raw(it.name.clone()),
            ];
            head.push(Span::styled(
                match it.size {
                    Some(s) => format!("  {}", size(s)),
                    None => "  size unknown".to_owned(),
                },
                theme.dim,
            ));
            out.push(Line::from(head));
            for d in &it.details {
                out.push(Line::styled(format!("     {d}"), theme.dim));
            }
            if let Some((p, fatal)) = &it.problem {
                out.push(Line::styled(
                    format!("     ! {p}"),
                    if *fatal { theme.error } else { theme.warn },
                ));
            }
        }
        out
    }

    /// Draw the modal.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let rect = centered(
            area,
            area.width.saturating_sub(4).min(100),
            area.height.saturating_sub(2).min(34),
        );
        frame.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border)
            .title(Span::styled(" Preview ", theme.accent));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let mut lines = self.lines(theme);
        let h = usize::from(inner.height).saturating_sub(2);
        let scroll = self.scroll.min(lines.len().saturating_sub(h));
        lines = lines.into_iter().skip(scroll).take(h).collect();
        lines.push(Line::default());
        lines.push(match self.blocker() {
            Some(b) => Line::styled(format!("Cannot add: {b}"), theme.error),
            None => Line::from(vec![
                Span::styled("Enter", theme.key),
                Span::raw(" add   "),
                Span::styled("Esc", theme.key),
                Span::raw(" back"),
            ]),
        });
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::add::options::OptionsForm;
    use crate::views::add::source::tokens;

    fn toks(lines: &[&str]) -> Vec<Token> {
        lines.iter().flat_map(|l| tokens(l)).collect()
    }

    fn uris(p: &Planned) -> Vec<String> {
        p.tokens()
    }

    #[test]
    fn separate_vs_mirror_params() {
        let t = toks(&[
            "http://a/x",
            "https://b/x",
            "magnet:?xt=urn:btih:abc&dn=Foo+Bar",
            "/tmp/a.torrent",
        ]);
        let sep = plan(&t, false);
        assert_eq!(sep.len(), 4);
        let mir = plan(&t, true);
        assert_eq!(mir.len(), 3);
        assert_eq!(uris(&mir[0]), ["http://a/x", "https://b/x"]);
        assert!(matches!(&mir[1], Planned::Uris(m) if m[0].kind == Kind::Magnet));
        assert!(matches!(&mir[2], Planned::File(_)));

        let mut files = Loaded::new();
        files.insert(
            "/tmp/a.torrent".into(),
            Ok(b"d4:infod4:name1:a6:lengthi1eee".to_vec()),
        );
        let parsed = OptionsForm::default().parse();
        let params = build_params(&mir, &files, &parsed).unwrap();
        assert_eq!(params.sources.len(), 3);
        assert_eq!(
            params.sources[0],
            AddSource::Uris {
                uris: vec!["http://a/x".into(), "https://b/x".into()]
            }
        );
        assert!(matches!(
            &params.sources[2],
            AddSource::Torrent { name, data_base64 }
                if name == "a.torrent" && STANDARD.decode(data_base64).unwrap().starts_with(b"d4:info")
        ));
        let params = build_params(&sep, &files, &parsed).unwrap();
        assert_eq!(params.sources.len(), 4);
        assert!(
            params
                .sources
                .iter()
                .take(3)
                .all(|s| matches!(s, AddSource::Uris { uris } if uris.len() == 1))
        );
    }

    #[test]
    fn missing_file_fails_build() {
        let t = toks(&["/x/a.metalink"]);
        let p = plan(&t, false);
        let mut files = Loaded::new();
        files.insert("/x/a.metalink".into(), Err("gone".into()));
        let parsed = OptionsForm::default().parse();
        assert_eq!(build_params(&p, &files, &parsed).unwrap_err(), "gone");
        let items = build_items(&p, &files, &parsed);
        assert!(items[0].problem.as_ref().is_some_and(|p| p.1));
    }

    #[test]
    fn out_and_select_need_a_single_suitable_source() {
        let mut form = OptionsForm::default();
        form.set_text(Field::Out, "x.bin");
        let parsed = form.parse();
        let two = plan(&toks(&["http://a/1", "http://a/2"]), false);
        assert_eq!(plan_errors(&two, &parsed)[0].0, Field::Out);
        assert!(plan_errors(&two[..1], &parsed).is_empty());
        // Mirrors count as one download.
        assert!(plan_errors(&plan(&toks(&["http://a/1", "http://a/2"]), true), &parsed).is_empty());
        let mut form = OptionsForm::default();
        form.set_text(Field::SelectFiles, "1");
        let parsed = form.parse();
        assert_eq!(plan_errors(&two[..1], &parsed)[0].0, Field::SelectFiles);
        let mag = plan(&toks(&["magnet:?xt=urn:btih:abc"]), false);
        assert!(plan_errors(&mag, &parsed).is_empty());
    }

    #[test]
    fn names_from_uris() {
        assert_eq!(
            uri_name("http://h.org/dir/My%20File.iso?x=1"),
            "My File.iso"
        );
        assert_eq!(uri_name("https://h.org/"), "h.org");
        assert_eq!(
            uri_name("magnet:?xt=urn:btih:abc&dn=Foo+Bar%21"),
            "Foo Bar!"
        );
        assert_eq!(uri_name("magnet:?xt=urn:btih:abc"), "btih:abc");
        assert_eq!(uri_name("ftp://h/a%2"), "a%2");
    }

    #[test]
    fn torrent_preview_applies_selection() {
        let data = b"d4:infod5:filesld6:lengthi3e4:pathl1:a1:xeed6:lengthi4e4:pathl1:beee4:name3:dir12:piece lengthi16384e6:pieces40:AAAAAAAAAAAAAAAAAAAABBBBBBBBBBBBBBBBBBBBee".to_vec();
        let t = toks(&["/t/d.torrent"]);
        let p = plan(&t, false);
        let mut files = Loaded::new();
        files.insert("/t/d.torrent".into(), Ok(data));
        let mut form = OptionsForm::default();
        form.set_text(Field::SelectFiles, "2");
        let items = build_items(&p, &files, &form.parse());
        let it = &items[0];
        assert_eq!(it.name, "dir");
        assert_eq!(it.size, Some(4));
        assert!(
            it.details
                .iter()
                .any(|d| d.contains("1 of 2 files selected"))
        );
        assert!(it.details.iter().any(|d| d.starts_with("[ ]   1  a/x")));
        assert!(it.details.iter().any(|d| d.starts_with("[x]   2  b")));
        let items = build_items(&p, &files, &OptionsForm::default().parse());
        assert_eq!(items[0].size, Some(7));
        assert!(items[0].details[0].contains("21e7f7434d48f160bd729e70246fa7758a8bbd6f"));
    }
}
