//! The per-add options form: fields, local validation, conversion to
//! [`JobOptions`]. Empty fields inherit the profile defaults.

use dman_proto::JobOptions;

use crate::widgets::editor::Editor;

/// A form field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Field {
    /// Group all URI lines into one download.
    Mirror,
    /// Download directory (on the daemon host).
    Dir,
    /// Output file name.
    Out,
    /// Add without starting.
    Paused,
    /// Download limit (bytes/s, sizes accepted).
    DownLimit,
    /// Upload limit.
    UpLimit,
    /// Connections per server.
    Connections,
    /// Split count.
    Split,
    /// Seed ratio.
    SeedRatio,
    /// Seed time in minutes.
    SeedTime,
    /// Sequential download.
    Sequential,
    /// File indexes to download.
    SelectFiles,
    /// User agent.
    UserAgent,
    /// Referer.
    Referer,
    /// HTTP headers, one per line.
    Headers,
    /// Proxy URL.
    Proxy,
    /// Checksum `algo=hex`.
    Checksum,
    /// Engine options, `key=value` per line.
    Engine,
}

impl Field {
    /// Every field in form order.
    pub const ALL: [Field; 18] = [
        Field::Mirror,
        Field::Dir,
        Field::Out,
        Field::Paused,
        Field::DownLimit,
        Field::UpLimit,
        Field::Connections,
        Field::Split,
        Field::SeedRatio,
        Field::SeedTime,
        Field::Sequential,
        Field::SelectFiles,
        Field::UserAgent,
        Field::Referer,
        Field::Headers,
        Field::Proxy,
        Field::Checksum,
        Field::Engine,
    ];

    /// Position in [`Field::ALL`].
    pub fn index(self) -> usize {
        Field::ALL.iter().position(|f| *f == self).unwrap_or(0)
    }

    /// Form label.
    pub const fn label(self) -> &'static str {
        match self {
            Field::Mirror => "Mirror",
            Field::Dir => "Directory",
            Field::Out => "Output name",
            Field::Paused => "Start paused",
            Field::DownLimit => "Down limit",
            Field::UpLimit => "Up limit",
            Field::Connections => "Conn/server",
            Field::Split => "Split",
            Field::SeedRatio => "Seed ratio",
            Field::SeedTime => "Seed minutes",
            Field::Sequential => "Sequential",
            Field::SelectFiles => "Select files",
            Field::UserAgent => "User agent",
            Field::Referer => "Referer",
            Field::Headers => "Headers",
            Field::Proxy => "Proxy",
            Field::Checksum => "Checksum",
            Field::Engine => "Engine opts",
        }
    }

    /// What an empty field means, shown dimmed.
    pub const fn hint(self) -> &'static str {
        match self {
            Field::Mirror => "all URI lines are mirrors of one download",
            Field::Dir => "profile default (Ctrl-f browses the daemon)",
            Field::Out => "from the source (single source only)",
            Field::Paused => "add without starting",
            Field::DownLimit | Field::UpLimit => "inherit (e.g. 2MiB; 0 = unlimited)",
            Field::Connections => "inherit (1-16)",
            Field::Split => "inherit",
            Field::SeedRatio => "inherit (e.g. 1.5)",
            Field::SeedTime => "inherit",
            Field::Sequential => "download pieces in order",
            Field::SelectFiles => "all (e.g. 1,3-5)",
            Field::UserAgent | Field::Referer | Field::Proxy => "inherit",
            Field::Headers => "Name: value, one per line",
            Field::Checksum => "e.g. sha-256=<hex>",
            Field::Engine => "key=value, one per line",
        }
    }

    /// A checkbox.
    pub const fn is_check(self) -> bool {
        matches!(self, Field::Mirror | Field::Paused | Field::Sequential)
    }

    /// Text that may span lines.
    pub const fn is_multiline(self) -> bool {
        matches!(self, Field::Headers | Field::Engine)
    }

    /// The next field, wrapping.
    pub fn next(self) -> Field {
        Field::ALL[(self.index() + 1) % Field::ALL.len()]
    }

    /// The previous field, wrapping.
    pub fn prev(self) -> Field {
        Field::ALL[(self.index() + Field::ALL.len() - 1) % Field::ALL.len()]
    }
}

/// The value of a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// Text.
    Text(Editor),
    /// Checkbox.
    Check(bool),
}

/// All field values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionsForm {
    values: Vec<Value>,
}

impl Default for OptionsForm {
    fn default() -> Self {
        Self {
            values: Field::ALL
                .iter()
                .map(|f| {
                    if f.is_check() {
                        Value::Check(false)
                    } else {
                        Value::Text(Editor::new(!f.is_multiline()))
                    }
                })
                .collect(),
        }
    }
}

/// The options form converted to wire types.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    /// Per-job options (only the fields the user filled in).
    pub options: JobOptions,
    /// Start paused.
    pub paused: bool,
    /// Group URI lines into one download.
    pub mirror: bool,
    /// Problems, in form order.
    pub errors: Vec<(Field, String)>,
}

impl OptionsForm {
    /// The editor of a text field.
    pub fn editor(&self, f: Field) -> Option<&Editor> {
        match &self.values[f.index()] {
            Value::Text(e) => Some(e),
            Value::Check(_) => None,
        }
    }

    /// The editor of a text field, mutable.
    pub fn editor_mut(&mut self, f: Field) -> Option<&mut Editor> {
        match &mut self.values[f.index()] {
            Value::Text(e) => Some(e),
            Value::Check(_) => None,
        }
    }

    /// Whether every field is empty and every checkbox unchecked.
    pub fn is_pristine(&self) -> bool {
        self.values.iter().all(|v| match v {
            Value::Text(e) => e.is_empty(),
            Value::Check(c) => !c,
        })
    }

    /// State of a checkbox (false for text fields).
    pub fn checked(&self, f: Field) -> bool {
        matches!(self.values[f.index()], Value::Check(true))
    }

    /// Flip a checkbox.
    pub fn toggle(&mut self, f: Field) {
        if let Value::Check(c) = &mut self.values[f.index()] {
            *c = !*c;
        }
    }

    /// Set the text of a field.
    pub fn set_text(&mut self, f: Field, text: &str) {
        if let Some(e) = self.editor_mut(f) {
            e.set_text(text);
        }
    }

    fn text(&self, f: Field) -> String {
        self.editor(f).map(Editor::text).unwrap_or_default()
    }

    /// Validate and convert. Never fails as a whole: see [`Parsed::errors`].
    pub fn parse(&self) -> Parsed {
        let mut p = Parsed {
            paused: self.checked(Field::Paused),
            mirror: self.checked(Field::Mirror),
            ..Parsed::default()
        };
        let o = &mut p.options;
        let mut errors = Vec::new();
        let mut put = |f: Field, r: Result<(), String>| {
            if let Err(e) = r {
                errors.push((f, e));
            }
        };
        let single = |f: Field| {
            let t = self.text(f);
            let t = t.trim();
            (!t.is_empty()).then(|| t.to_owned())
        };
        o.dir = single(Field::Dir);
        o.out = single(Field::Out);
        put(
            Field::Out,
            match &o.out {
                Some(n) if n.contains('/') || n.contains('\\') => {
                    Err("a file name, not a path (use Directory for the folder)".into())
                }
                _ => Ok(()),
            },
        );
        for (f, slot) in [
            (Field::DownLimit, &mut o.max_download_limit),
            (Field::UpLimit, &mut o.max_upload_limit),
        ] {
            if let Some(t) = single(f) {
                put(f, parse_limit(&t).map(|v| *slot = Some(v)));
            }
        }
        if let Some(t) = single(Field::Connections) {
            put(
                Field::Connections,
                parse_range(&t, 1, 16).map(|v| o.connections_per_server = Some(v)),
            );
        }
        if let Some(t) = single(Field::Split) {
            put(
                Field::Split,
                parse_range(&t, 1, 1024).map(|v| o.split = Some(v)),
            );
        }
        if let Some(t) = single(Field::SeedRatio) {
            put(
                Field::SeedRatio,
                match t.parse::<f64>() {
                    Ok(v) if v.is_finite() && v >= 0.0 => {
                        o.seed_ratio = Some(v);
                        Ok(())
                    }
                    _ => Err(format!("`{t}` is not a ratio (0 or more)")),
                },
            );
        }
        if let Some(t) = single(Field::SeedTime) {
            put(
                Field::SeedTime,
                t.parse::<u64>()
                    .map(|v| o.seed_time = Some(v))
                    .map_err(|_| format!("`{t}` is not a number of minutes")),
            );
        }
        if self.checked(Field::Sequential) {
            o.sequential = Some(true);
        }
        if let Some(t) = single(Field::SelectFiles) {
            put(
                Field::SelectFiles,
                parse_selection(&t).map(|v| o.select_files = Some(v)),
            );
        }
        o.user_agent = single(Field::UserAgent);
        o.referer = single(Field::Referer);
        if let Some(t) = single(Field::Proxy) {
            put(
                Field::Proxy,
                if t.contains(char::is_whitespace) {
                    Err("a proxy URL has no spaces".into())
                } else {
                    o.proxy = Some(t);
                    Ok(())
                },
            );
        }
        if let Some(t) = single(Field::Checksum) {
            put(
                Field::Checksum,
                parse_checksum(&t).map(|v| o.checksum = Some(v)),
            );
        }
        let headers = self.text(Field::Headers);
        for line in headers.lines().map(str::trim).filter(|l| !l.is_empty()) {
            match line.split_once(':') {
                Some((n, _)) if !n.trim().is_empty() && !n.contains(char::is_whitespace) => {
                    o.headers.push(line.to_owned());
                }
                _ => {
                    put(
                        Field::Headers,
                        Err(format!("`{line}` is not `Name: value`")),
                    );
                }
            }
        }
        let engine = self.text(Field::Engine);
        for line in engine.lines().map(str::trim).filter(|l| !l.is_empty()) {
            match line.split_once('=') {
                Some((k, v)) if !k.trim().is_empty() && !k.trim().contains(char::is_whitespace) => {
                    o.engine_options
                        .insert(k.trim().to_owned(), v.trim().to_owned());
                }
                _ => {
                    put(Field::Engine, Err(format!("`{line}` is not `key=value`")));
                }
            }
        }
        errors.sort_by_key(|(f, _)| f.index());
        p.errors = errors;
        p
    }
}

fn parse_limit(t: &str) -> Result<u64, String> {
    dman_config::parse_bytes(t).map_err(|_| format!("`{t}` is not a size (e.g. 500KiB, 2MiB, 0)"))
}

fn parse_range(t: &str, min: u32, max: u32) -> Result<u32, String> {
    match t.parse::<u32>() {
        Ok(v) if (min..=max).contains(&v) => Ok(v),
        _ => Err(format!("`{t}` is not a number from {min} to {max}")),
    }
}

/// `1,3,5-7` → sorted unique 1-based indexes.
pub fn parse_selection(t: &str) -> Result<Vec<u32>, String> {
    let bad = || format!("`{t}` is not a list like 1,3,5-7");
    let mut out = Vec::new();
    for part in t.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (part, part),
        };
        let (a, b): (u32, u32) = (a.parse().map_err(|_| bad())?, b.parse().map_err(|_| bad())?);
        if a == 0 || b < a || b - a > 100_000 {
            return Err(bad());
        }
        out.extend(a..=b);
    }
    out.sort_unstable();
    out.dedup();
    if out.is_empty() { Err(bad()) } else { Ok(out) }
}

/// `algo=hex` with the right digest length.
fn parse_checksum(t: &str) -> Result<String, String> {
    let (algo, hex) = t
        .split_once('=')
        .ok_or("expected `<algo>=<hex digest>`, e.g. sha-256=…")?;
    let algo = algo.trim().to_ascii_lowercase();
    let want = match algo.as_str() {
        "md5" => 32,
        "sha-1" => 40,
        "sha-224" => 56,
        "sha-256" => 64,
        "sha-384" => 96,
        "sha-512" => 128,
        "adler32" => 8,
        _ => return Err(format!("unknown algorithm `{algo}`")),
    };
    let hex = hex.trim();
    if hex.len() != want || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("{algo} needs {want} hex digits"));
    }
    Ok(format!("{algo}={}", hex.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(pairs: &[(Field, &str)]) -> OptionsForm {
        let mut f = OptionsForm::default();
        for (field, text) in pairs {
            f.set_text(*field, text);
        }
        f
    }

    #[test]
    fn empty_form_inherits_everything() {
        let p = OptionsForm::default().parse();
        assert_eq!(p.options, JobOptions::default());
        assert!(p.errors.is_empty() && !p.paused && !p.mirror);
    }

    #[test]
    fn valid_values_convert() {
        let mut f = form(&[
            (Field::Dir, " /data/x "),
            (Field::Out, "a.iso"),
            (Field::DownLimit, "2MiB"),
            (Field::UpLimit, "0"),
            (Field::Connections, "8"),
            (Field::Split, "16"),
            (Field::SeedRatio, "1.5"),
            (Field::SeedTime, "90"),
            (Field::SelectFiles, "5,1-3,2"),
            (Field::UserAgent, "UA/1"),
            (Field::Proxy, "http://p:3128"),
            (
                Field::Checksum,
                "SHA-1=DA39A3EE5E6B4B0D3255BFEF95601890AFD80709",
            ),
            (Field::Headers, "Cookie: a=b\n\nX-Y: z"),
            (Field::Engine, "max-tries = 3\nretry-wait=5"),
        ]);
        f.toggle(Field::Paused);
        f.toggle(Field::Sequential);
        f.toggle(Field::Mirror);
        let p = f.parse();
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        let o = p.options;
        assert_eq!(o.dir.as_deref(), Some("/data/x"));
        assert_eq!(o.max_download_limit, Some(2 << 20));
        assert_eq!(o.max_upload_limit, Some(0));
        assert_eq!(o.connections_per_server, Some(8));
        assert_eq!(o.split, Some(16));
        assert_eq!(o.seed_ratio, Some(1.5));
        assert_eq!(o.seed_time, Some(90));
        assert_eq!(o.select_files, Some(vec![1, 2, 3, 5]));
        assert_eq!(o.sequential, Some(true));
        assert_eq!(o.headers, ["Cookie: a=b", "X-Y: z"]);
        assert_eq!(o.engine_options["max-tries"], "3");
        assert_eq!(
            o.checksum.as_deref(),
            Some("sha-1=da39a3ee5e6b4b0d3255bfef95601890afd80709")
        );
        assert!(p.paused && p.mirror);
    }

    #[test]
    fn invalid_values_report_per_field() {
        let p = form(&[
            (Field::Out, "a/b"),
            (Field::DownLimit, "fast"),
            (Field::Connections, "99"),
            (Field::Split, "0"),
            (Field::SeedRatio, "-1"),
            (Field::SeedTime, "x"),
            (Field::SelectFiles, "0"),
            (Field::Proxy, "a b"),
            (Field::Checksum, "sha-256=abc"),
            (Field::Headers, "novalue"),
            (Field::Engine, "=x"),
        ])
        .parse();
        let fields: Vec<Field> = p.errors.iter().map(|(f, _)| *f).collect();
        assert_eq!(
            fields,
            [
                Field::Out,
                Field::DownLimit,
                Field::Connections,
                Field::Split,
                Field::SeedRatio,
                Field::SeedTime,
                Field::SelectFiles,
                Field::Headers,
                Field::Proxy,
                Field::Checksum,
                Field::Engine,
            ]
        );
    }

    #[test]
    fn selection_and_checksum_edges() {
        assert_eq!(parse_selection("3-1").map_err(|_| ()), Err(()));
        assert_eq!(parse_selection(" 2 , 4-5 "), Ok(vec![2, 4, 5]));
        assert!(parse_selection(",").is_err());
        assert!(parse_checksum("md5=d41d8cd98f00b204e9800998ecf8427e").is_ok());
        assert!(parse_checksum("crc=1").is_err());
        assert!(parse_checksum("md5").is_err());
    }

    #[test]
    fn field_navigation_wraps() {
        assert_eq!(Field::Engine.next(), Field::Mirror);
        assert_eq!(Field::Mirror.prev(), Field::Engine);
        assert_eq!(Field::Dir.next(), Field::Out);
    }
}
