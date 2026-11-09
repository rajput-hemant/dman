//! `dman add`.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use dman_proto::{AddOutcome, AddParams, AddSource, JobOptions, VideoOptions, VideoSubtitles};

use crate::cli::{AddArgs, VideoArgs};
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

/// How URLs are routed to the video engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoMode {
    /// `--video`: every http(s) URL.
    Always,
    /// `--no-video`: never.
    Never,
    /// Known video sites (offline heuristic, D10).
    Detect,
}

fn video_mode(v: &VideoArgs) -> VideoMode {
    if v.video {
        VideoMode::Always
    } else if v.no_video {
        VideoMode::Never
    } else {
        VideoMode::Detect
    }
}

/// Whether `input` goes to yt-dlp; `Some(true)` = detected (not forced).
fn wants_video(input: &str, mode: VideoMode) -> Option<bool> {
    let url = url::Url::parse(input).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    match mode {
        VideoMode::Always => Some(false),
        VideoMode::Never => None,
        VideoMode::Detect => dman_ytdlp::is_probably_video_url(&url).then_some(true),
    }
}

/// The video options given on the command line (unset = daemon defaults).
fn video_options(v: &VideoArgs) -> VideoOptions {
    VideoOptions {
        // `audio:mp3` is a quality preset; the daemon resolves it.
        quality: v.quality.clone(),
        format_id: v.format_id.clone(),
        container: v.container.clone(),
        audio_only: None,
        playlist: v.playlist || v.playlist_items.is_some(),
        playlist_items: v.playlist_items.clone(),
        subtitles: (!v.subs.is_empty()).then(|| VideoSubtitles {
            langs: v.subs.clone(),
            embed: v.embed_subs,
            auto: v.auto_subs,
        }),
        embed_thumbnail: v.thumbnail.then_some(true),
        embed_metadata: v.no_metadata.then_some(false),
        output_template: v.template.clone(),
        cookies_from_browser: v.cookies_from_browser.clone(),
        sponsorblock_remove: (!v.sponsorblock_remove.is_empty())
            .then(|| v.sponsorblock_remove.clone()),
        allow_separate_streams: v.allow_separate_streams,
        extra_args: v.ytdlp_args.clone(),
    }
}

/// Sources, their display labels, and the inputs detected as video.
type Built = (Vec<AddSource>, Vec<String>, Vec<String>);

/// Turn raw inputs into sources plus a display label for each. Video URLs
/// (forced or detected) become `AddSource::Video`; the second list holds the
/// inputs that were detected as video without `--video`.
fn build_sources(inputs: &[String], mirror: bool, video: &VideoArgs) -> Result<Built, CliError> {
    let mode = if mirror && !video.video {
        VideoMode::Never
    } else {
        video_mode(video)
    };
    let opts = video_options(video);
    let mut sources = Vec::new();
    let mut labels = Vec::new();
    let mut mirrors = Vec::new();
    let mut detected = Vec::new();
    for input in inputs {
        let kind = classify(input);
        if kind == Kind::Uri
            && let Some(auto) = wants_video(input, mode)
        {
            if auto {
                detected.push(input.clone());
            }
            sources.push(AddSource::Video {
                url: input.clone(),
                video: Box::new(opts.clone()),
            });
            labels.push(input.clone());
            continue;
        }
        if mode == VideoMode::Always && kind == Kind::Uri {
            return Err(CliError::invalid(format!(
                "--video needs http(s) URLs, got '{input}'"
            )));
        }
        match kind {
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
    Ok((sources, labels, detected))
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
            AddOutcome::Added { .. } if !json => {
                let ids: Vec<String> = outcome.ids().iter().map(ToString::to_string).collect();
                println!("added {}", ids.join(" "));
            }
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
    if args.video.any() && args.video.no_video {
        return Err(CliError::invalid(
            "video options cannot be combined with --no-video",
        ));
    }
    let (sources, labels, detected) = build_sources(&inputs, args.mirror, &args.video)?;
    if args.video.any() && !sources.iter().any(|s| matches!(s, AddSource::Video { .. })) {
        return Err(CliError::invalid(
            "video options need a video URL (add --video to download a URL with yt-dlp)",
        ));
    }
    for d in &detected {
        eprintln!("detected video URL {d}; use --no-video to download the page itself");
    }
    let params = AddParams {
        sources,
        options: build_options(args)?,
        paused: args.paused,
    };
    let outcomes = ctx.connect().await?.add(params).await?;
    report(&labels, &outcomes, ctx.json)
}

/// `dman probe <url>`: formats, subtitles, playlist entries and quality plans.
pub async fn probe(
    ctx: &Ctx,
    url: &str,
    playlist: bool,
    cookies_from_browser: Option<String>,
) -> Result<i32, CliError> {
    let client = ctx.connect().await?;
    if !ctx.json {
        eprintln!("probing with yt-dlp on the daemon host...");
    }
    let p = client
        .video_probe(dman_proto::VideoProbeParams {
            url: url.to_owned(),
            playlist,
            cookies_from_browser,
        })
        .await?;
    if ctx.json {
        super::print_json(&p)?;
        return Ok(0);
    }
    use crate::format as f;
    let mut kv: Vec<(&str, String)> = Vec::new();
    if let Some(t) = &p.title {
        kv.push(("title", t.clone()));
    }
    if let Some(u) = &p.uploader {
        kv.push(("uploader", u.clone()));
    }
    if let Some(d) = p.duration {
        kv.push(("duration", f::duration(d)));
    }
    if let Some(id) = &p.id {
        kv.push(("id", id.clone()));
    }
    if !p.subtitles.is_empty() {
        kv.push(("subtitles", p.subtitles.join(", ")));
    }
    if !p.automatic_captions.is_empty() {
        let mut a = p.automatic_captions.clone();
        let n = a.len();
        a.truncate(12);
        let more = if n > 12 {
            format!(" (+{} more)", n - 12)
        } else {
            String::new()
        };
        kv.push(("auto captions", format!("{}{more}", a.join(", "))));
    }
    kv.push((
        "ffmpeg",
        if p.ffmpeg {
            "available (merging, conversion, embedding)".into()
        } else {
            "missing (no merging; see the plans)".into()
        },
    ));
    print!("{}", f::key_values(&kv));
    if p.playlist {
        println!(
            "\nplaylist: {} entries (add with --playlist or --playlist-items)",
            p.entries.len()
        );
        print!("{}", f::playlist_table(&p.entries));
    }
    if !p.formats.is_empty() {
        println!();
        print!("{}", f::video_formats_table(&p.formats));
    }
    println!();
    print!("{}", f::video_plans_table(&p.plans));
    Ok(0)
}

/// `dman preview <magnet>`: the files of a magnet link, without downloading.
pub async fn preview(ctx: &Ctx, magnet: &str, timeout: Option<u32>) -> Result<i32, CliError> {
    let client = ctx.connect().await?;
    if !ctx.json {
        eprintln!("fetching metadata (this needs peers; Ctrl-C to cancel)...");
    }
    let p = client.preview_magnet(magnet, timeout).await?;
    if ctx.json {
        super::print_json(&p)?;
        return Ok(0);
    }
    println!("name:       {}", p.name);
    println!("info hash:  {}", p.info_hash);
    println!(
        "size:       {} in {} file(s)",
        crate::format::size(p.total_bytes),
        p.files.len()
    );
    println!();
    print!("{}", crate::format::preview_table(&p.files));
    Ok(0)
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
        let v = VideoArgs::default();
        let (s, l, _) = build_sources(&inputs, true, &v).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(l.len(), 1);
        let (s, _, _) = build_sources(&inputs, false, &v).unwrap();
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn video_detection_and_flags() {
        let yt = "https://www.youtube.com/watch?v=abc".to_owned();
        let file = "https://example.com/file.iso".to_owned();
        let inputs = vec![yt.clone(), file.clone()];
        let (s, _, detected) = build_sources(&inputs, false, &VideoArgs::default()).unwrap();
        assert!(matches!(&s[0], AddSource::Video { url, .. } if *url == yt));
        assert!(matches!(&s[1], AddSource::Uris { .. }));
        assert_eq!(detected, vec![yt.clone()]);

        let no = VideoArgs {
            no_video: true,
            ..Default::default()
        };
        let (s, _, detected) = build_sources(&inputs, false, &no).unwrap();
        assert!(s.iter().all(|s| matches!(s, AddSource::Uris { .. })));
        assert!(detected.is_empty());

        let forced = VideoArgs {
            video: true,
            quality: Some("720p".into()),
            subs: vec!["en".into(), "de".into()],
            embed_subs: true,
            playlist_items: Some("1-3".into()),
            sponsorblock_remove: vec!["sponsor".into()],
            ..Default::default()
        };
        let (s, _, detected) = build_sources(&inputs, false, &forced).unwrap();
        assert!(detected.is_empty());
        let AddSource::Video { video, .. } = &s[1] else {
            panic!("{s:?}")
        };
        assert_eq!(video.quality.as_deref(), Some("720p"));
        assert!(video.playlist);
        assert_eq!(video.subtitles.as_ref().unwrap().langs, ["en", "de"]);
        assert!(video.subtitles.as_ref().unwrap().embed);
        assert_eq!(video.embed_metadata, None);
        assert!(build_sources(&["magnet:?xt=x".into()], false, &forced).is_err());
    }

    #[test]
    fn torrent_file_is_base64() {
        let dir = std::env::temp_dir().join(format!("dman-add-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.torrent");
        std::fs::write(&f, b"abc").unwrap();
        let (s, _, _) = build_sources(
            &[f.to_string_lossy().into_owned()],
            false,
            &VideoArgs::default(),
        )
        .unwrap();
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
