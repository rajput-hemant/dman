//! `dman add`.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use dman_proto::{AddOutcome, AddParams, AddSource, JobOptions};

use crate::cli::AddArgs;
use crate::connect::Ctx;
use crate::error::{CliError, batch_exit};

/// What a source string refers to.
#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Uri,
    Torrent,
    Metalink,
}

/// Parse `1,3-5` into sorted, de-duplicated 1-based file indexes.
pub fn parse_select_files(s: &str) -> Result<Vec<u32>, String> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let num = |t: &str| {
            t.trim()
                .parse::<u32>()
                .ok()
                .filter(|n| *n >= 1)
                .ok_or_else(|| format!("invalid file index '{t}' in '{s}'"))
        };
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b) = (num(a)?, num(b)?);
                if b < a {
                    return Err(format!("invalid range '{part}'"));
                }
                out.extend(a..=b);
            }
            None => out.push(num(part)?),
        }
    }
    if out.is_empty() {
        return Err("empty file selection".into());
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Split `key=value`.
pub fn parse_key_value(s: &str) -> Result<(String, String), CliError> {
    match s.split_once('=') {
        Some((k, v)) if !k.is_empty() => Ok((k.to_owned(), v.to_owned())),
        _ => Err(CliError::invalid(format!("expected KEY=VALUE, got '{s}'"))),
    }
}

/// Non-blank, non-comment lines.
pub fn parse_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

fn classify(src: &str) -> Kind {
    if src.contains("://") || src.starts_with("magnet:") {
        return Kind::Uri;
    }
    let ext = Path::new(src)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("torrent") => Kind::Torrent,
        Some("metalink" | "meta4") => Kind::Metalink,
        _ => Kind::Uri,
    }
}

fn collect_inputs(args: &AddArgs) -> Result<Vec<String>, CliError> {
    let mut out = Vec::new();
    for s in &args.sources {
        if s == "-" {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
            out.extend(parse_lines(&text));
        } else {
            out.push(s.clone());
        }
    }
    if let Some(file) = &args.input_file {
        let text = std::fs::read_to_string(file)
            .map_err(|e| CliError::invalid(format!("cannot read {file}: {e}")))?;
        out.extend(parse_lines(&text));
    }
    Ok(out)
}

fn read_file_source(path: &str, kind: &Kind) -> Result<AddSource, CliError> {
    let bytes =
        std::fs::read(path).map_err(|e| CliError::invalid(format!("cannot read {path}: {e}")))?;
    let name = Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned());
    let data_base64 = STANDARD.encode(bytes);
    Ok(if *kind == Kind::Torrent {
        AddSource::Torrent { name, data_base64 }
    } else {
        AddSource::Metalink { name, data_base64 }
    })
}

/// Turn raw inputs into sources plus a display label for each.
fn build_sources(
    inputs: &[String],
    mirror: bool,
) -> Result<(Vec<AddSource>, Vec<String>), CliError> {
    let mut sources = Vec::new();
    let mut labels = Vec::new();
    let mut mirrors = Vec::new();
    for input in inputs {
        match classify(input) {
            Kind::Uri if mirror => mirrors.push(input.clone()),
            Kind::Uri => {
                sources.push(AddSource::Uris {
                    uris: vec![input.clone()],
                });
                labels.push(input.clone());
            }
            kind => {
                sources.push(read_file_source(input, &kind)?);
                labels.push(input.clone());
            }
        }
    }
    if !mirrors.is_empty() {
        labels.push(mirrors.join(", "));
        sources.push(AddSource::Uris { uris: mirrors });
    }
    Ok((sources, labels))
}

fn build_options(args: &AddArgs) -> Result<JobOptions, CliError> {
    let mut o = JobOptions {
        dir: args.dir.clone(),
        out: args.out.clone(),
        max_download_limit: args.max_download_limit,
        max_upload_limit: args.max_upload_limit,
        select_files: args.select_files.clone(),
        seed_ratio: args.seed_ratio,
        seed_time: args.seed_time,
        sequential: args.sequential.then_some(true),
        headers: args.header.clone(),
        user_agent: args.user_agent.clone(),
        referer: args.referer.clone(),
        proxy: args.proxy.clone(),
        checksum: args.checksum.clone(),
        connections_per_server: args.connections,
        split: args.split,
        ..JobOptions::default()
    };
    for kv in &args.options {
        let (k, v) = parse_key_value(kv)?;
        o.engine_options.insert(k, v);
    }
    Ok(o)
}

fn report(labels: &[String], outcomes: &[AddOutcome], json: bool) -> Result<i32, CliError> {
    let mut first_failure = None;
    let mut failed = 0;
    for (label, outcome) in labels.iter().zip(outcomes) {
        match outcome {
            AddOutcome::Added { id } if !json => println!("added {id}"),
            AddOutcome::Failed { error } => {
                failed += 1;
                first_failure.get_or_insert(error.code);
                if !json {
                    println!("failed {label}: {}", error.message);
                }
            }
            AddOutcome::Added { .. } => {}
        }
    }
    if json {
        super::print_json(&outcomes)?;
    }
    Ok(batch_exit(outcomes.len(), failed, first_failure))
}

pub async fn run(ctx: &Ctx, args: &AddArgs) -> Result<i32, CliError> {
    let inputs = collect_inputs(args)?;
    if inputs.is_empty() {
        return Err(CliError::invalid(
            "nothing to add: give a URL, magnet, file or `-`",
        ));
    }
    let (sources, labels) = build_sources(&inputs, args.mirror)?;
    let params = AddParams {
        sources,
        options: build_options(args)?,
        paused: args.paused,
    };
    let outcomes = ctx.connect().await?.add(params).await?;
    report(&labels, &outcomes, ctx.json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_files() {
        assert_eq!(parse_select_files("1,3-5").unwrap(), vec![1, 3, 4, 5]);
        assert_eq!(parse_select_files("5, 2,2").unwrap(), vec![2, 5]);
        assert!(parse_select_files("0").is_err());
        assert!(parse_select_files("4-2").is_err());
        assert!(parse_select_files("a").is_err());
        assert!(parse_select_files("").is_err());
    }

    #[test]
    fn key_values() {
        assert_eq!(
            parse_key_value("a=b=c").unwrap(),
            ("a".to_owned(), "b=c".to_owned())
        );
        assert!(parse_key_value("nope").is_err());
        assert!(parse_key_value("=x").is_err());
    }

    #[test]
    fn lines_skip_comments_and_blanks() {
        let l = parse_lines("# c\n\n  http://a  \nmagnet:?x\n");
        assert_eq!(l, vec!["http://a", "magnet:?x"]);
    }

    #[test]
    fn classification() {
        assert_eq!(classify("http://x/a.torrent"), Kind::Uri);
        assert_eq!(classify("magnet:?xt=a"), Kind::Uri);
        assert_eq!(classify("/tmp/a.TORRENT"), Kind::Torrent);
        assert_eq!(classify("x.meta4"), Kind::Metalink);
        assert_eq!(classify("file.iso"), Kind::Uri);
    }

    #[test]
    fn mirror_groups_uris() {
        let inputs = vec!["http://a/f".to_owned(), "http://b/f".to_owned()];
        let (s, l) = build_sources(&inputs, true).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(l.len(), 1);
        let (s, _) = build_sources(&inputs, false).unwrap();
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn torrent_file_is_base64() {
        let dir = std::env::temp_dir().join(format!("dman-add-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.torrent");
        std::fs::write(&f, b"abc").unwrap();
        let (s, _) = build_sources(&[f.to_string_lossy().into_owned()], false).unwrap();
        assert_eq!(
            s,
            vec![AddSource::Torrent {
                name: "a.torrent".into(),
                data_base64: "YWJj".into()
            }]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
