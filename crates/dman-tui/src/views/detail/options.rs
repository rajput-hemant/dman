//! The options tab: which `JobOptions` fields are shown and how they are edited.

use dman_proto::JobOptions;

use crate::format;

/// An editable option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptField {
    /// Download directory.
    Dir,
    /// Output file name.
    Out,
    /// Per-download download limit.
    MaxDownloadLimit,
    /// Per-download upload limit.
    MaxUploadLimit,
    /// Seed ratio.
    SeedRatio,
    /// Seed time in minutes.
    SeedTime,
    /// Sequential download (toggled, no prompt).
    Sequential,
    /// User agent.
    UserAgent,
    /// Referer.
    Referer,
    /// Proxy URL.
    Proxy,
    /// Checksum `<algo>=<digest>`.
    Checksum,
    /// Connections per server.
    ConnectionsPerServer,
    /// Split count.
    Split,
}

impl OptField {
    /// Name as in the API.
    pub const fn label(self) -> &'static str {
        match self {
            OptField::Dir => "dir",
            OptField::Out => "out",
            OptField::MaxDownloadLimit => "max_download_limit",
            OptField::MaxUploadLimit => "max_upload_limit",
            OptField::SeedRatio => "seed_ratio",
            OptField::SeedTime => "seed_time",
            OptField::Sequential => "sequential",
            OptField::UserAgent => "user_agent",
            OptField::Referer => "referer",
            OptField::Proxy => "proxy",
            OptField::Checksum => "checksum",
            OptField::ConnectionsPerServer => "connections_per_server",
            OptField::Split => "split",
        }
    }

    /// One-line explanation shown in the prompt.
    pub const fn help(self) -> &'static str {
        match self {
            OptField::Dir => "Download directory on the daemon host:",
            OptField::Out => "Output file name (relative):",
            OptField::MaxDownloadLimit | OptField::MaxUploadLimit => {
                "Bytes/s, optional K/M/G suffix; 0 = unlimited:"
            }
            OptField::SeedRatio => "Stop seeding at this ratio (0 = never by ratio):",
            OptField::SeedTime => "Stop seeding after this many minutes:",
            OptField::Sequential => "true / false:",
            OptField::UserAgent => "User agent string:",
            OptField::Referer => "Referer URL:",
            OptField::Proxy => "Proxy URL:",
            OptField::Checksum => "<algo>=<hex digest>, e.g. sha-256=…:",
            OptField::ConnectionsPerServer => "Connections per server (1-16):",
            OptField::Split => "Pieces to split into (1-64):",
        }
    }

    /// Turn the prompt text into the changed-fields-only options of the call.
    ///
    /// # Errors
    /// What is wrong with `value`.
    pub fn build(self, value: &str) -> Result<JobOptions, String> {
        let v = value.trim();
        let mut o = JobOptions::default();
        let text = || {
            if v.is_empty() {
                Err("value must not be empty".to_owned())
            } else {
                Ok(v.to_owned())
            }
        };
        let number = || {
            v.parse::<u32>()
                .map_err(|_| format!("'{v}' is not a whole number"))
        };
        match self {
            OptField::Dir => o.dir = Some(text()?),
            OptField::Out => o.out = Some(text()?),
            OptField::MaxDownloadLimit => o.max_download_limit = Some(parse_size(v)?),
            OptField::MaxUploadLimit => o.max_upload_limit = Some(parse_size(v)?),
            OptField::SeedRatio => {
                let r: f64 = v.parse().map_err(|_| format!("'{v}' is not a number"))?;
                if !r.is_finite() || r < 0.0 {
                    return Err("must be 0 or more".into());
                }
                o.seed_ratio = Some(r);
            }
            OptField::SeedTime => {
                o.seed_time = Some(
                    v.parse()
                        .map_err(|_| format!("'{v}' is not a whole number"))?,
                );
            }
            OptField::Sequential => {
                o.sequential = Some(match v.to_ascii_lowercase().as_str() {
                    "true" | "yes" | "on" | "1" => true,
                    "false" | "no" | "off" | "0" => false,
                    _ => return Err("expected true or false".into()),
                });
            }
            OptField::UserAgent => o.user_agent = Some(text()?),
            OptField::Referer => o.referer = Some(text()?),
            OptField::Proxy => o.proxy = Some(text()?),
            OptField::Checksum => o.checksum = Some(text()?),
            OptField::ConnectionsPerServer => o.connections_per_server = Some(number()?),
            OptField::Split => o.split = Some(number()?),
        }
        Ok(o)
    }
}

/// `1.5M`, `512K`, `2048`, `0` → bytes.
fn parse_size(v: &str) -> Result<u64, String> {
    let lower = v.to_ascii_lowercase();
    let (digits, mult) = match lower.chars().last() {
        Some('k') => (&lower[..lower.len() - 1], 1u64 << 10),
        Some('m') => (&lower[..lower.len() - 1], 1 << 20),
        Some('g') => (&lower[..lower.len() - 1], 1 << 30),
        _ => (lower.as_str(), 1),
    };
    let n: f64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("'{v}' is not a size (try 500K, 2M or 0)"))?;
    if !n.is_finite() || n < 0.0 {
        return Err("must be 0 or more".into());
    }
    // Rounding to whole bytes is intended.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok((n * mult as f64).round() as u64)
}

/// Bytes/s as text to edit: `2M`, `512K`, or plain bytes.
fn size_input(n: u64) -> String {
    if n != 0 && n.is_multiple_of(1 << 30) {
        format!("{}G", n >> 30)
    } else if n != 0 && n.is_multiple_of(1 << 20) {
        format!("{}M", n >> 20)
    } else if n != 0 && n.is_multiple_of(1 << 10) {
        format!("{}K", n >> 10)
    } else {
        n.to_string()
    }
}

/// One row of the options tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptRow {
    /// Field name.
    pub label: &'static str,
    /// Value as shown (`(default)` when unset).
    pub value: String,
    /// Whether the value is unset.
    pub unset: bool,
    /// Text to pre-fill the editor with.
    pub edit_value: String,
    /// The editable field, `None` for read-only rows.
    pub field: Option<OptField>,
    /// Where else to change a read-only row.
    pub note: &'static str,
}

fn editable<T>(
    label: &'static str,
    field: OptField,
    v: Option<T>,
    show: impl Fn(&T) -> String,
    edit: impl Fn(&T) -> String,
) -> OptRow {
    OptRow {
        label,
        unset: v.is_none(),
        value: v.as_ref().map_or_else(|| "(default)".into(), &show),
        edit_value: v.as_ref().map(edit).unwrap_or_default(),
        field: Some(field),
        note: "",
    }
}

fn readonly(label: &'static str, value: Option<String>, note: &'static str) -> OptRow {
    OptRow {
        label,
        unset: value.is_none(),
        value: value.unwrap_or_else(|| "(default)".into()),
        edit_value: String::new(),
        field: None,
        note,
    }
}

/// All rows of the options tab for `o`.
pub fn option_rows(o: &JobOptions) -> Vec<OptRow> {
    let s = |x: &String| x.clone();
    let n = |x: &u32| x.to_string();
    let count = |len: usize| (len > 0).then(|| format!("{len} set"));
    vec![
        editable("dir", OptField::Dir, o.dir.clone(), s, s),
        editable("out", OptField::Out, o.out.clone(), s, s),
        editable(
            "max_download_limit",
            OptField::MaxDownloadLimit,
            o.max_download_limit,
            |b| format::limit(*b),
            |b| size_input(*b),
        ),
        editable(
            "max_upload_limit",
            OptField::MaxUploadLimit,
            o.max_upload_limit,
            |b| format::limit(*b),
            |b| size_input(*b),
        ),
        readonly(
            "select_files",
            o.select_files.as_ref().map(|f| {
                let list: Vec<String> = f.iter().map(u32::to_string).collect();
                list.join(",")
            }),
            "use Space on the Files tab",
        ),
        readonly(
            "file_names",
            count(o.file_names.len()),
            "set when adding (rename comes later)",
        ),
        editable(
            "seed_ratio",
            OptField::SeedRatio,
            o.seed_ratio,
            |r| r.to_string(),
            |r| r.to_string(),
        ),
        editable(
            "seed_time",
            OptField::SeedTime,
            o.seed_time,
            |m| format!("{m} min"),
            |m| m.to_string(),
        ),
        editable(
            "sequential",
            OptField::Sequential,
            o.sequential,
            |b| b.to_string(),
            |b| b.to_string(),
        ),
        readonly("headers", count(o.headers.len()), "set when adding"),
        editable(
            "user_agent",
            OptField::UserAgent,
            o.user_agent.clone(),
            s,
            s,
        ),
        editable("referer", OptField::Referer, o.referer.clone(), s, s),
        editable("proxy", OptField::Proxy, o.proxy.clone(), s, s),
        editable("checksum", OptField::Checksum, o.checksum.clone(), s, s),
        editable(
            "connections_per_server",
            OptField::ConnectionsPerServer,
            o.connections_per_server,
            n,
            n,
        ),
        editable("split", OptField::Split, o.split, n, n),
        readonly(
            "engine_options",
            count(o.engine_options.len()),
            "set when adding",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("2M"), Ok(2 << 20));
        assert_eq!(parse_size("1.5k"), Ok(1536));
        assert_eq!(parse_size("0"), Ok(0));
        assert!(parse_size("fast").is_err());
        assert_eq!(size_input(2 << 20), "2M");
        assert_eq!(size_input(1500), "1500");
    }

    #[test]
    fn build_sets_only_the_edited_field() {
        let o = OptField::MaxDownloadLimit.build(" 500K ").unwrap();
        assert_eq!(
            o,
            JobOptions {
                max_download_limit: Some(500 << 10),
                ..JobOptions::default()
            }
        );
        assert!(OptField::Split.build("x").is_err());
        assert!(OptField::Dir.build("").is_err());
    }
}
