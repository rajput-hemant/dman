//! Mapping of dman job options to aria2 options, plus validation.
//!
//! aria2 validates nothing: unknown option names and garbage values come back
//! as `"OK"`. Everything that reaches aria2 from dman goes through here first.
//!
//! # Multi-valued options (`header`, `index-out`)
//!
//! [`Options`] is a string map, so repeated options are encoded as ONE string
//! with the values joined by `\n`. Verified against aria2c 1.37.0 (see the
//! ignored test in `tests/real_aria2.rs`): over JSON-RPC both a JSON array
//! (`["1=a","2=b"]`) and the newline-joined string (`"1=a\n2=b"`) are accepted
//! by `addUri`/`addTorrent`/`changeOption` and `getOption` then shows both
//! values as `"1=a\n2=b\n"`. The newline form fits the string map, so it is
//! what this module produces. Individual values must therefore never contain
//! a newline (enforced).

use std::collections::BTreeSet;

use dman_proto::JobOptions;

use crate::types::Options;

/// Why a [`JobOptions`] could not be mapped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OptionError {
    /// `engine_options` named something that is not an allowed per-download
    /// aria2 option.
    #[error("unknown or not allowed aria2 option `{0}`")]
    Unknown(String),
    /// The option is controlled by a typed `JobOptions` field; set that instead.
    #[error("option `{0}` has a dedicated field and cannot be passed in engine_options")]
    Reserved(String),
    /// A value failed validation.
    #[error("invalid value for `{option}`: {reason}")]
    Invalid {
        /// Option (or `JobOptions` field) name.
        option: String,
        /// Human readable reason.
        reason: String,
    },
}

fn invalid(option: &str, reason: impl Into<String>) -> OptionError {
    OptionError::Invalid {
        option: option.to_owned(),
        reason: reason.into(),
    }
}

/// Value type of an allow-listed option.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Bool,
    Int,
    Size,
    Text,
    OneOf(&'static [&'static str]),
}

const MODE_BOOL_MEM: &[&str] = &["true", "false", "mem"];

/// Per-download ("Input File") aria2 options that may be passed through in
/// `engine_options`, with their value kinds. Options that have a typed
/// `JobOptions` field are in [`RESERVED`] instead. Deliberately absent:
/// `gid`, `pause`, `pause-metadata`, `piece-length`, `force-save`,
/// `dry-run`, `remove-control-file`, `rpc-save-upload-metadata`,
/// `parameterized-uri`.
const ALLOWED: &[(&str, Kind)] = &[
    ("all-proxy-passwd", Kind::Text),
    ("all-proxy-user", Kind::Text),
    ("allow-overwrite", Kind::Bool),
    ("allow-piece-length-change", Kind::Bool),
    ("always-resume", Kind::Bool),
    ("async-dns", Kind::Bool),
    ("auto-file-renaming", Kind::Bool),
    ("bt-enable-hook-after-hash-check", Kind::Bool),
    ("bt-enable-lpd", Kind::Bool),
    ("bt-exclude-tracker", Kind::Text),
    ("bt-external-ip", Kind::Text),
    ("bt-force-encryption", Kind::Bool),
    ("bt-hash-check-seed", Kind::Bool),
    ("bt-load-saved-metadata", Kind::Bool),
    ("bt-max-peers", Kind::Int),
    ("bt-metadata-only", Kind::Bool),
    ("bt-min-crypto-level", Kind::OneOf(&["plain", "arc4"])),
    ("bt-prioritize-piece", Kind::Text),
    ("bt-remove-unselected-file", Kind::Bool),
    ("bt-request-peer-speed-limit", Kind::Size),
    ("bt-require-crypto", Kind::Bool),
    ("bt-save-metadata", Kind::Bool),
    ("bt-seed-unverified", Kind::Bool),
    ("bt-stop-timeout", Kind::Int),
    ("bt-tracker", Kind::Text),
    ("bt-tracker-connect-timeout", Kind::Int),
    ("bt-tracker-interval", Kind::Int),
    ("bt-tracker-timeout", Kind::Int),
    ("check-integrity", Kind::Bool),
    ("conditional-get", Kind::Bool),
    ("connect-timeout", Kind::Int),
    ("content-disposition-default-utf8", Kind::Bool),
    ("enable-http-keep-alive", Kind::Bool),
    ("enable-http-pipelining", Kind::Bool),
    ("enable-mmap", Kind::Bool),
    ("enable-peer-exchange", Kind::Bool),
    (
        "file-allocation",
        Kind::OneOf(&["none", "prealloc", "trunc", "falloc"]),
    ),
    ("follow-metalink", Kind::OneOf(MODE_BOOL_MEM)),
    ("follow-torrent", Kind::OneOf(MODE_BOOL_MEM)),
    ("ftp-passwd", Kind::Text),
    ("ftp-pasv", Kind::Bool),
    ("ftp-proxy", Kind::Text),
    ("ftp-proxy-passwd", Kind::Text),
    ("ftp-proxy-user", Kind::Text),
    ("ftp-reuse-connection", Kind::Bool),
    ("ftp-type", Kind::OneOf(&["binary", "ascii"])),
    ("ftp-user", Kind::Text),
    ("hash-check-only", Kind::Bool),
    ("http-accept-gzip", Kind::Bool),
    ("http-auth-challenge", Kind::Bool),
    ("http-no-cache", Kind::Bool),
    ("http-passwd", Kind::Text),
    ("http-proxy", Kind::Text),
    ("http-proxy-passwd", Kind::Text),
    ("http-proxy-user", Kind::Text),
    ("http-user", Kind::Text),
    ("https-proxy", Kind::Text),
    ("https-proxy-passwd", Kind::Text),
    ("https-proxy-user", Kind::Text),
    ("load-cookies", Kind::Text),
    ("lowest-speed-limit", Kind::Size),
    ("max-file-not-found", Kind::Int),
    ("max-mmap-limit", Kind::Size),
    ("max-resume-failure-tries", Kind::Int),
    ("max-tries", Kind::Int),
    ("metalink-base-uri", Kind::Text),
    ("metalink-enable-unique-protocol", Kind::Bool),
    ("metalink-language", Kind::Text),
    ("metalink-location", Kind::Text),
    ("metalink-os", Kind::Text),
    (
        "metalink-preferred-protocol",
        Kind::OneOf(&["http", "https", "ftp", "bittorrent", "none"]),
    ),
    ("metalink-version", Kind::Text),
    ("min-split-size", Kind::Size),
    ("no-file-allocation-limit", Kind::Size),
    ("no-netrc", Kind::Bool),
    ("no-proxy", Kind::Text),
    ("proxy-method", Kind::OneOf(&["get", "tunnel"])),
    ("realtime-chunk-checksum", Kind::Bool),
    ("remote-time", Kind::Bool),
    ("retry-wait", Kind::Int),
    ("reuse-uri", Kind::Bool),
    ("ssh-host-key-md", Kind::Text),
    (
        "stream-piece-selector",
        Kind::OneOf(&["default", "inorder", "random", "geom"]),
    ),
    ("timeout", Kind::Int),
    (
        "uri-selector",
        Kind::OneOf(&["inorder", "feedback", "adaptive"]),
    ),
    ("use-head", Kind::Bool),
];

/// Options set from typed `JobOptions` fields; refused in `engine_options`.
const RESERVED: &[&str] = &[
    "all-proxy",
    "checksum",
    "dir",
    "header",
    "index-out",
    "max-connection-per-server",
    "max-download-limit",
    "max-upload-limit",
    "out",
    "referer",
    "seed-ratio",
    "seed-time",
    "select-file",
    "split",
    "user-agent",
];

/// Options aria2 applies to an ACTIVE download via `changeOption`
/// (aria2 manual + spike: `max-download-limit`, `max-upload-limit`,
/// `seed-ratio` confirmed on 1.37.0). Everything else must be changed while
/// paused.
pub const CHANGEABLE_WHILE_ACTIVE: &[&str] = &[
    "bt-max-peers",
    "bt-remove-unselected-file",
    "bt-request-peer-speed-limit",
    "max-download-limit",
    "max-upload-limit",
    "seed-ratio",
    "seed-time",
];

/// Whether changing `key` only works reliably on a paused download
/// (`bt-tracker`, `index-out`, `select-file`, `out`, `dir`, ...). True for
/// everything not in [`CHANGEABLE_WHILE_ACTIVE`]; dman then does
/// pause, change, resume.
pub fn requires_pause(key: &str) -> bool {
    !CHANGEABLE_WHILE_ACTIVE.contains(&key)
}

/// Global options for `changeGlobalOption` / initial setup. Limits are bytes
/// per second (`0` = unlimited). `max-concurrent-downloads` is only included
/// when `max_concurrent` is `Some`: dman owns the queue.
pub fn global_limits(download: u64, upload: u64, max_concurrent: Option<u32>) -> Options {
    let mut o = Options::new()
        .with("max-overall-download-limit", download.to_string())
        .with("max-overall-upload-limit", upload.to_string());
    if let Some(n) = max_concurrent {
        o.set("max-concurrent-downloads", n.to_string());
    }
    o
}

fn check_text(option: &str, v: &str) -> Result<(), OptionError> {
    if v.contains(['\n', '\r', '\0']) {
        return Err(invalid(option, "must not contain line breaks or NUL"));
    }
    Ok(())
}

fn check_relative_path(option: &str, v: &str) -> Result<(), OptionError> {
    check_text(option, v)?;
    if v.is_empty() {
        return Err(invalid(option, "must not be empty"));
    }
    if v.starts_with('/') || v.split('/').any(|c| c == "..") {
        return Err(invalid(
            option,
            "must be relative and must not contain `..`",
        ));
    }
    Ok(())
}

fn valid_size(v: &str) -> bool {
    let digits = v.strip_suffix(['K', 'k', 'M', 'm', 'G', 'g']).unwrap_or(v);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) && digits.len() <= 18
}

fn validate_value(name: &str, kind: Kind, v: &str) -> Result<(), OptionError> {
    check_text(name, v)?;
    match kind {
        Kind::Bool => {
            if v != "true" && v != "false" {
                return Err(invalid(name, "expected `true` or `false`"));
            }
        }
        Kind::Int => {
            if v.parse::<u32>().is_err() {
                return Err(invalid(name, "expected a non-negative integer"));
            }
        }
        Kind::Size => {
            if !valid_size(v) {
                return Err(invalid(
                    name,
                    "expected a size like `1048576`, `512K`, `1M` or `1G`",
                ));
            }
        }
        Kind::Text => {}
        Kind::OneOf(allowed) => {
            if !allowed.contains(&v) {
                return Err(invalid(
                    name,
                    format!("expected one of {}", allowed.join(", ")),
                ));
            }
        }
    }
    Ok(())
}

const CHECKSUM_ALGOS: &[(&str, usize)] = &[
    ("sha-1", 40),
    ("sha-224", 56),
    ("sha-256", 64),
    ("sha-384", 96),
    ("sha-512", 128),
    ("md5", 32),
    ("adler32", 8),
];

/// Validate and normalise (lowercase) a `<algo>=<hex>` checksum.
fn normalize_checksum(v: &str) -> Result<String, OptionError> {
    let (algo, hex) = v
        .split_once('=')
        .ok_or_else(|| invalid("checksum", "expected `<algo>=<hex digest>`"))?;
    let algo = algo.trim().to_ascii_lowercase();
    let hex = hex.trim().to_ascii_lowercase();
    let Some((_, len)) = CHECKSUM_ALGOS.iter().find(|(a, _)| *a == algo) else {
        return Err(invalid(
            "checksum",
            format!(
                "unsupported algorithm `{algo}` (use {})",
                CHECKSUM_ALGOS
                    .iter()
                    .map(|(a, _)| *a)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    };
    if hex.len() != *len || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid(
            "checksum",
            format!("{algo} digest must be {len} hex characters"),
        ));
    }
    Ok(format!("{algo}={hex}"))
}

/// Map dman job options to aria2 options, validating everything.
///
/// Typed fields cannot collide with `engine_options` (which refuses their
/// names as [`OptionError::Reserved`]). Multi-valued options are `\n`-joined, see
/// the module docs.
pub fn job_options_to_aria2(opts: &JobOptions) -> Result<Options, OptionError> {
    let mut out = Options::new();

    for (name, value) in &opts.engine_options {
        if RESERVED.contains(&name.as_str()) {
            return Err(OptionError::Reserved(name.clone()));
        }
        let Some((key, kind)) = ALLOWED.iter().find(|(k, _)| k == name) else {
            return Err(OptionError::Unknown(name.clone()));
        };
        validate_value(key, *kind, value)?;
        out.set(*key, value.clone());
    }

    if let Some(dir) = &opts.dir {
        check_text("dir", dir)?;
        if dir.is_empty() {
            return Err(invalid("dir", "must not be empty"));
        }
        out.set("dir", dir.clone());
    }
    if let Some(name) = &opts.out {
        check_relative_path("out", name)?;
        out.set("out", name.clone());
    }
    if let Some(n) = opts.max_download_limit {
        out.set("max-download-limit", n.to_string());
    }
    if let Some(n) = opts.max_upload_limit {
        out.set("max-upload-limit", n.to_string());
    }
    if let Some(files) = &opts.select_files {
        let set: BTreeSet<u32> = files.iter().copied().collect();
        if set.is_empty() {
            return Err(invalid(
                "select_files",
                "at least one file index is required",
            ));
        }
        if set.contains(&0) {
            return Err(invalid("select_files", "file indexes start at 1"));
        }
        let list: Vec<String> = set.iter().map(u32::to_string).collect();
        out.set("select-file", list.join(","));
    }
    if !opts.file_names.is_empty() {
        let mut lines = Vec::with_capacity(opts.file_names.len());
        for (idx, name) in &opts.file_names {
            if *idx == 0 {
                return Err(invalid("file_names", "file indexes start at 1"));
            }
            check_relative_path("file_names", name)?;
            lines.push(format!("{idx}={name}"));
        }
        out.set("index-out", lines.join("\n"));
    }
    if let Some(r) = opts.seed_ratio {
        if !r.is_finite() || r < 0.0 {
            return Err(invalid("seed_ratio", "must be a non-negative number"));
        }
        out.set("seed-ratio", r.to_string());
    }
    if let Some(m) = opts.seed_time {
        out.set("seed-time", m.to_string());
    }
    if opts.sequential == Some(true) {
        out.set("stream-piece-selector", "inorder");
    }
    if !opts.headers.is_empty() {
        for h in &opts.headers {
            check_text("headers", h)?;
            match h.split_once(':') {
                Some((name, _))
                    if !name.trim().is_empty() && !name.contains(char::is_whitespace) => {}
                // The header itself is not echoed: it may carry credentials.
                _ => return Err(invalid("headers", "every header must be `Name: value`")),
            }
        }
        out.set("header", opts.headers.join("\n"));
    }
    if let Some(v) = &opts.user_agent {
        check_text("user_agent", v)?;
        out.set("user-agent", v.clone());
    }
    if let Some(v) = &opts.referer {
        check_text("referer", v)?;
        out.set("referer", v.clone());
    }
    if let Some(v) = &opts.proxy {
        check_text("proxy", v)?;
        out.set("all-proxy", v.clone());
    }
    if let Some(v) = &opts.checksum {
        out.set("checksum", normalize_checksum(v)?);
    }
    if let Some(n) = opts.connections_per_server {
        if !(1..=16).contains(&n) {
            return Err(invalid(
                "connections_per_server",
                "must be between 1 and 16",
            ));
        }
        out.set("max-connection-per-server", n.to_string());
    }
    if let Some(n) = opts.split {
        if n < 1 {
            return Err(invalid("split", "must be at least 1"));
        }
        out.set("split", n.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eo(k: &str, v: &str) -> JobOptions {
        let mut o = JobOptions::default();
        o.engine_options.insert(k.into(), v.into());
        o
    }

    fn map(o: &JobOptions) -> Options {
        job_options_to_aria2(o).expect("valid")
    }

    fn err(o: &JobOptions) -> OptionError {
        job_options_to_aria2(o).expect_err("invalid")
    }

    #[test]
    fn empty_maps_to_empty() {
        assert!(map(&JobOptions::default()).is_empty());
    }

    #[test]
    fn simple_mappings() {
        let o = JobOptions {
            dir: Some("/d".into()),
            out: Some("a/b.bin".into()),
            max_download_limit: Some(1024),
            max_upload_limit: Some(0),
            seed_ratio: Some(1.5),
            seed_time: Some(30),
            user_agent: Some("ua".into()),
            referer: Some("http://r/".into()),
            proxy: Some("http://p:3128".into()),
            connections_per_server: Some(8),
            split: Some(4),
            ..Default::default()
        };
        let m = map(&o);
        assert_eq!(m.get("dir"), Some("/d"));
        assert_eq!(m.get("out"), Some("a/b.bin"));
        assert_eq!(m.get("max-download-limit"), Some("1024"));
        assert_eq!(m.get("max-upload-limit"), Some("0"));
        assert_eq!(m.get("seed-ratio"), Some("1.5"));
        assert_eq!(m.get("seed-time"), Some("30"));
        assert_eq!(m.get("user-agent"), Some("ua"));
        assert_eq!(m.get("referer"), Some("http://r/"));
        assert_eq!(m.get("all-proxy"), Some("http://p:3128"));
        assert_eq!(m.get("max-connection-per-server"), Some("8"));
        assert_eq!(m.get("split"), Some("4"));
    }

    #[test]
    fn select_files_dedupe_sort() {
        let o = JobOptions {
            select_files: Some(vec![5, 1, 3, 3]),
            ..Default::default()
        };
        assert_eq!(map(&o).get("select-file"), Some("1,3,5"));
    }

    #[test]
    fn select_files_errors() {
        let o = JobOptions {
            select_files: Some(vec![]),
            ..Default::default()
        };
        assert!(matches!(err(&o), OptionError::Invalid { .. }));
        let o = JobOptions {
            select_files: Some(vec![0, 2]),
            ..Default::default()
        };
        assert!(matches!(err(&o), OptionError::Invalid { .. }));
    }

    #[test]
    fn file_names_become_index_out() {
        let mut o = JobOptions::default();
        o.file_names.insert(2, "b".into());
        o.file_names.insert(1, "a".into());
        assert_eq!(map(&o).get("index-out"), Some("1=a\n2=b"));
        o.file_names.insert(0, "z".into());
        assert!(matches!(err(&o), OptionError::Invalid { .. }));
    }

    #[test]
    fn file_names_reject_traversal_and_newlines() {
        for bad in ["../x", "/etc/passwd", "a\nb", ""] {
            let mut o = JobOptions::default();
            o.file_names.insert(1, bad.into());
            assert!(matches!(err(&o), OptionError::Invalid { .. }), "{bad:?}");
        }
        let o = JobOptions {
            out: Some("../evil".into()),
            ..Default::default()
        };
        assert!(matches!(err(&o), OptionError::Invalid { .. }));
    }

    #[test]
    fn sequential_maps_only_when_true() {
        let t = JobOptions {
            sequential: Some(true),
            ..Default::default()
        };
        assert_eq!(map(&t).get("stream-piece-selector"), Some("inorder"));
        let f = JobOptions {
            sequential: Some(false),
            ..Default::default()
        };
        assert!(map(&f).is_empty());
    }

    #[test]
    fn headers_joined_and_validated() {
        let o = JobOptions {
            headers: vec!["X-A: 1".into(), "Cookie: a=b".into()],
            ..Default::default()
        };
        assert_eq!(map(&o).get("header"), Some("X-A: 1\nCookie: a=b"));
        for bad in ["nocolon", ": v", "Bad Name: v", "A: b\nC: d"] {
            let o = JobOptions {
                headers: vec![bad.into()],
                ..Default::default()
            };
            assert!(matches!(err(&o), OptionError::Invalid { .. }), "{bad:?}");
        }
    }

    #[test]
    fn checksum_validation() {
        let h = "a".repeat(64);
        let o = JobOptions {
            checksum: Some(format!("SHA-256={}", h.to_uppercase())),
            ..Default::default()
        };
        assert_eq!(
            map(&o).get("checksum"),
            Some(format!("sha-256={h}").as_str())
        );
        for bad in [
            "sha-256".to_owned(),
            format!("sha-3={h}"),
            "sha-256=abc".to_owned(),
            format!("sha-256={}", "g".repeat(64)),
        ] {
            let o = JobOptions {
                checksum: Some(bad.clone()),
                ..Default::default()
            };
            assert!(matches!(err(&o), OptionError::Invalid { .. }), "{bad}");
        }
        for (a, n) in CHECKSUM_ALGOS {
            let o = JobOptions {
                checksum: Some(format!("{a}={}", "0".repeat(*n))),
                ..Default::default()
            };
            map(&o);
        }
    }

    #[test]
    fn numeric_range_errors() {
        for n in [0, 17] {
            let o = JobOptions {
                connections_per_server: Some(n),
                ..Default::default()
            };
            assert!(matches!(err(&o), OptionError::Invalid { .. }));
        }
        let o = JobOptions {
            split: Some(0),
            ..Default::default()
        };
        assert!(matches!(err(&o), OptionError::Invalid { .. }));
        for r in [-1.0, f64::NAN, f64::INFINITY] {
            let o = JobOptions {
                seed_ratio: Some(r),
                ..Default::default()
            };
            assert!(matches!(err(&o), OptionError::Invalid { .. }));
        }
    }

    #[test]
    fn engine_options_allowed_and_rejected() {
        assert_eq!(
            map(&eo("bt-tracker", "udp://t:1/announce")).get("bt-tracker"),
            Some("udp://t:1/announce")
        );
        assert_eq!(map(&eo("max-tries", "3")).get("max-tries"), Some("3"));
        assert_eq!(
            err(&eo("bogus-option", "1")),
            OptionError::Unknown("bogus-option".into())
        );
        // Not changeable / dangerous options are not allowed.
        for k in [
            "piece-length",
            "gid",
            "pause",
            "force-save",
            "rpc-secret",
            "remove-control-file",
        ] {
            assert_eq!(err(&eo(k, "x")), OptionError::Unknown(k.into()));
        }
        assert_eq!(err(&eo("dir", "/x")), OptionError::Reserved("dir".into()));
        assert_eq!(
            err(&eo("header", "A: b")),
            OptionError::Reserved("header".into())
        );
    }

    #[test]
    fn engine_option_value_validation() {
        let bad = [
            ("check-integrity", "yes"),
            ("max-tries", "-1"),
            ("max-tries", "x"),
            ("min-split-size", "1X"),
            ("min-split-size", ""),
            ("file-allocation", "magic"),
            ("follow-torrent", "maybe"),
            ("bt-tracker", "a\nb"),
        ];
        for (k, v) in bad {
            assert!(
                matches!(err(&eo(k, v)), OptionError::Invalid { .. }),
                "{k}={v}"
            );
        }
        for (k, v) in [
            ("check-integrity", "true"),
            ("min-split-size", "1M"),
            ("min-split-size", "20m"),
            ("lowest-speed-limit", "1024"),
            ("file-allocation", "falloc"),
            ("follow-torrent", "mem"),
        ] {
            map(&eo(k, v));
        }
    }

    #[test]
    fn allow_list_is_sane() {
        let mut seen = BTreeSet::new();
        for (k, _) in ALLOWED {
            assert!(seen.insert(*k), "duplicate {k}");
            assert!(!RESERVED.contains(k), "{k} both allowed and reserved");
        }
        for k in [
            "bt-tracker",
            "bt-exclude-tracker",
            "check-integrity",
            "timeout",
            "retry-wait",
        ] {
            assert!(seen.contains(k), "{k}");
        }
    }

    #[test]
    fn global_limits_optional_concurrency() {
        let g = global_limits(100, 50, None);
        assert_eq!(g.get("max-overall-download-limit"), Some("100"));
        assert_eq!(g.get("max-overall-upload-limit"), Some("50"));
        assert_eq!(g.get("max-concurrent-downloads"), None);
        let g = global_limits(0, 0, Some(1000));
        assert_eq!(g.get("max-concurrent-downloads"), Some("1000"));
    }

    #[test]
    fn pause_requirements() {
        for k in [
            "max-download-limit",
            "max-upload-limit",
            "seed-ratio",
            "bt-max-peers",
        ] {
            assert!(!requires_pause(k), "{k}");
        }
        for k in [
            "bt-tracker",
            "index-out",
            "select-file",
            "out",
            "dir",
            "header",
        ] {
            assert!(requires_pause(k), "{k}");
        }
    }

    #[test]
    fn credentials_stay_out_of_debug_and_errors() {
        let o = JobOptions {
            headers: vec!["Bearer-token-without-colon".into()],
            ..JobOptions::default()
        };
        let e = err(&o).to_string();
        assert!(!e.contains("Bearer"), "{e}");
        let opts = Options::new()
            .with("http-passwd", "hunter2")
            .with("all-proxy", "http://u:pw@h")
            .with("header", "Authorization: Bearer abc")
            .with("max-tries", "3");
        let dbg = format!("{opts:?}");
        assert!(!dbg.contains("hunter2") && !dbg.contains("pw@") && !dbg.contains("abc"));
        assert!(dbg.contains("max-tries"));
    }
}
