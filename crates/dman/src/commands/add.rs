//! `dman add`.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use dman_proto::{
    AddOutcome, AddParams, AddSource, JobId, JobOptions, VideoOptions, VideoSubtitles,
};

use std::io::IsTerminal;

use dman_client::Client;

use crate::cli::{AddArgs, VideoArgs};
use crate::commands::progress::Renderer;
use crate::commands::quality;
use crate::commands::wait::{conclude, track};
use crate::connect::Ctx;
use crate::error::{CliError, EXIT_DUPLICATE, EXIT_TIMEOUT, batch_exit};

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
        _ => Err(CliError::invalid(format!("expected KEY=VALUE, got '{s}'"))
            .with_hint("write the option as -o KEY=VALUE, e.g. -o max-tries=5")),
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
        let text = std::fs::read_to_string(file).map_err(|e| {
            CliError::invalid(format!("cannot read {file}: {e}"))
                .with_hint("check the path of the -i/--input-file list")
        })?;
        out.extend(parse_lines(&text));
    }
    Ok(out)
}

fn read_file_source(path: &str, kind: &Kind) -> Result<AddSource, CliError> {
    let bytes = std::fs::read(path).map_err(|e| {
        CliError::invalid(format!("cannot read {path}: {e}"))
            .with_hint("check that the file exists and is readable")
    })?;
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
    /// Known video sites (offline heuristic).
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
pub type Built = (Vec<AddSource>, Vec<String>, Vec<String>);

/// Turn raw inputs into sources plus a display label for each. Video URLs
/// (forced or detected) become `AddSource::Video`; the second list holds the
/// inputs that were detected as video without `--video`.
pub fn build_sources(
    inputs: &[String],
    mirror: bool,
    video: &VideoArgs,
) -> Result<Built, CliError> {
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
            return Err(
                CliError::invalid(format!("--video needs http(s) URLs, got '{input}'")).with_hint(
                    "drop --video to download it with aria2, or give the page's http(s) URL",
                ),
            );
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

/// What happened to the sources of one `add`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// Sources that failed, duplicates included.
    pub failed: usize,
    /// Duplicates that were refused (a subset of `failed`).
    pub duplicates: usize,
    /// Duplicate torrents that received new trackers (not failures).
    pub merged: usize,
    /// The first failure: its code, and whether it was a duplicate.
    pub first: Option<(dman_proto::ErrorCode, bool)>,
}

impl Tally {
    pub fn of(outcomes: &[AddOutcome]) -> Self {
        let mut t = Self::default();
        for o in outcomes {
            let AddOutcome::Failed { error } = o else {
                continue;
            };
            match dman_proto::DuplicateInfo::from_error(error) {
                Some(d) if d.trackers_added > 0 => t.merged += 1,
                dup => {
                    t.failed += 1;
                    t.duplicates += usize::from(dup.is_some());
                    t.first.get_or_insert((error.code, dup.is_some()));
                }
            }
        }
        t
    }

    /// Process exit code: 0 when nothing failed, the duplicate code when every
    /// source failed and the first failure was a duplicate, otherwise as for
    /// any batch.
    pub fn exit(&self, total: usize) -> i32 {
        match self.first {
            Some((_, true)) if self.failed >= total => EXIT_DUPLICATE,
            first => batch_exit(total, self.failed, first.map(|f| f.0)),
        }
    }
}

/// Prints one line per outcome (or the JSON array) and returns the exit code.
/// With `print_doc` false the JSON is left to the caller (`add --wait` prints
/// one combined document).
pub fn report(
    labels: &[String],
    outcomes: &[AddOutcome],
    json: bool,
    print_doc: bool,
) -> Result<i32, CliError> {
    let tally = Tally::of(outcomes);
    if json {
        if print_doc {
            super::print_json(&outcomes)?;
        }
        return Ok(tally.exit(outcomes.len()));
    }
    for (label, outcome) in labels.iter().zip(outcomes) {
        match outcome {
            AddOutcome::Added { .. } => {
                let ids: Vec<String> = outcome.ids().iter().map(ToString::to_string).collect();
                println!("added {}", ids.join(" "));
            }
            AddOutcome::Failed { error } => match dman_proto::DuplicateInfo::from_error(error) {
                Some(d) if d.trackers_added > 0 => {
                    println!("merged into {}: {}", d.id, error.message)
                }
                Some(_) => eprintln!("dman: skipped {label}: {}", error.message),
                None => eprintln!("dman: failed {label}: {}", error.message),
            },
        }
    }
    if tally.duplicates > 0 {
        eprintln!("hint: use --force to add anyway");
    }
    Ok(tally.exit(outcomes.len()))
}

/// Asks whether to add the links found in the clipboard; Enter means yes.
fn offer_clipboard(links: &[String]) -> Result<bool, CliError> {
    use std::io::{BufRead as _, Write as _};
    let shown: Vec<String> = links.iter().map(|l| super::clipboard::shown(l)).collect();
    let what = match shown.as_slice() {
        [one] => one.clone(),
        many => format!("{} links ({})", many.len(), many.join(", ")),
    };
    eprint!("Add {what} from the clipboard? [Y/n] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "" | "y" | "yes"
    ))
}

/// The sources of this `add`: arguments, `-i` file and stdin, plus the
/// clipboard with `--clipboard` (or, without any source, when a terminal can
/// confirm it).
async fn gather_inputs(ctx: &Ctx, args: &AddArgs) -> Result<Vec<String>, CliError> {
    let mut inputs = collect_inputs(args)?;
    if args.clipboard {
        let links = super::clipboard::read_links().await?;
        if !ctx.json {
            let shown: Vec<String> = links.iter().map(|l| super::clipboard::shown(l)).collect();
            eprintln!("from the clipboard: {}", shown.join(", "));
        }
        inputs.extend(links);
    } else if inputs.is_empty()
        && !ctx.json
        && super::can_prompt()
        && let Ok(links) = super::clipboard::read_links().await
        && offer_clipboard(&links)?
    {
        inputs = links;
    }
    Ok(inputs)
}

pub async fn run(ctx: &Ctx, args: &AddArgs) -> Result<i32, CliError> {
    let inputs = gather_inputs(ctx, args).await?;
    if inputs.is_empty() {
        return Err(CliError::invalid(
            "nothing to add: give a URL, magnet, file or `-` (or --clipboard)",
        )
        .with_hint("e.g. `dman add https://example.org/file.iso`; see `dman add --help`"));
    }
    if args.video.any() && args.video.no_video {
        return Err(
            CliError::invalid("video options cannot be combined with --no-video")
                .with_hint("drop --no-video, or drop the video options"),
        );
    }
    let (sources, labels, detected) = build_sources(&inputs, args.mirror, &args.video)?;
    if args.video.any() && !sources.iter().any(|s| matches!(s, AddSource::Video { .. })) {
        return Err(CliError::invalid("video options need a video URL")
            .with_hint("add --video to download the URL with yt-dlp"));
    }
    for d in &detected {
        eprintln!("detected video URL {d}; use --no-video to download the page itself");
    }
    let params = AddParams {
        sources,
        options: build_options(args)?,
        paused: args.paused,
        force: args.force,
    };
    let client = ctx.connect().await?;
    let reads_stdin = args.sources.iter().any(|s| s == "-");
    let mut params = params;
    if quality::picker_allowed(
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        ctx.json,
        args.yes,
        reads_stdin,
        &args.video,
    ) {
        choose_qualities(&client, &mut params).await?;
    }
    let outcomes = client.add(params).await?;
    if !args.wait {
        return report(&labels, &outcomes, ctx.json, true);
    }
    let added = report(&labels, &outcomes, ctx.json, false)?;
    let ids: Vec<JobId> = outcomes.iter().flat_map(AddOutcome::ids).collect();
    if ids.is_empty() {
        if ctx.json {
            super::print_json(&serde_json::json!({ "added": outcomes, "jobs": [] }))?;
        }
        return Ok(added);
    }
    let renderer = (!ctx.json).then(|| Renderer::new(true));
    let tracked = track(&client, &ids, args.timeout, renderer).await?;
    let waited = if ctx.json {
        let code = tracked.exit_code();
        super::print_json(&serde_json::json!({
            "added": outcomes,
            "jobs": tracked.jobs,
            "timed_out": tracked.timed_out,
        }))?;
        code
    } else {
        conclude(ctx, &tracked, args.timeout)?
    };
    // A timeout is reported as such; otherwise a failure to add some sources wins
    // over the outcome of the jobs that were added.
    Ok(if waited == EXIT_TIMEOUT || added == 0 {
        waited
    } else {
        added
    })
}

/// The quality flow for every video source without an explicit choice:
/// preferences, probe, picker, saved defaults. Playlists and sources that need
/// no choice keep the daemon's defaults.
async fn choose_qualities(client: &Client, params: &mut AddParams) -> Result<(), CliError> {
    let wanted = |s: &AddSource| {
        matches!(s, AddSource::Video { video, .. }
            if video.quality.is_none() && video.format_id.is_none() && video.audio_only.is_none()
                && !video.playlist && video.playlist_items.is_none())
    };
    if !params.sources.iter().any(wanted) {
        return Ok(());
    }
    if !client.video_preferences().await?.ask_quality {
        return Ok(());
    }
    let mut never_ask = false;
    for source in &mut params.sources {
        if never_ask || !wanted(source) {
            continue;
        }
        let AddSource::Video { url, video } = source else {
            continue;
        };
        eprintln!("checking the available qualities of {url} ...");
        let probe = client
            .video_probe(dman_proto::VideoProbeParams {
                url: url.clone(),
                playlist: false,
                cookies_from_browser: video.cookies_from_browser.clone(),
            })
            .await?;
        if probe.playlist {
            eprintln!(
                "{url} is a playlist: using your defaults (choose items with --playlist-items, a quality with --quality)"
            );
            continue;
        }
        if !probe.needs_choice {
            continue;
        }
        let answers =
            quality::run_picker(&probe, &mut std::io::stdin().lock(), &mut std::io::stderr())?;
        video.format_id = Some(answers.picked.format_id.clone());
        if let Some(update) = answers.update {
            let saved = client.video_preferences_set(update).await?;
            if answers.never_ask {
                never_ask = true;
                eprintln!("{}", quality::NEVER_ASK_HINT);
            } else {
                eprintln!(
                    "saved defaults: quality {}, audio {}",
                    saved.default_quality, saved.default_audio
                );
            }
        }
    }
    Ok(())
}

/// The video options `dman probe --plan` was given (unset = the saved defaults).
pub fn plan_options(
    a: &crate::cli::PlanArgs,
    playlist: bool,
    cookies_from_browser: Option<String>,
) -> VideoOptions {
    video_options(&VideoArgs {
        quality: a.quality.clone(),
        format_id: a.format_id.clone(),
        container: a.container.clone(),
        playlist,
        playlist_items: a.playlist_items.clone(),
        subs: a.subs.clone(),
        embed_subs: a.embed_subs,
        auto_subs: a.auto_subs,
        thumbnail: a.thumbnail,
        no_metadata: a.no_metadata,
        sponsorblock_remove: a.sponsorblock_remove.clone(),
        cookies_from_browser,
        allow_separate_streams: a.allow_separate_streams,
        ..VideoArgs::default()
    })
}

/// The human text of `dman probe --plan`.
pub fn plan_text(p: &dman_proto::VideoDownloadPlan) -> String {
    let source = match p.source.as_str() {
        "defaults" => "saved defaults",
        "format_id" => "--format-id",
        "audio_only" => "audio only",
        _ => "--quality",
    };
    let mut kv: Vec<(&str, String)> = vec![("url", p.url.clone())];
    let format = match &p.quality {
        Some(q) => format!("{q} ({source})"),
        None => format!("exact format id ({source})"),
    };
    kv.push(("format", format));
    kv.push(("selector", p.selector.clone()));
    kv.push((
        "ffmpeg",
        if p.ffmpeg {
            "available".into()
        } else {
            "missing (single-file formats only, nothing is merged or converted)".into()
        },
    ));
    if let Some(c) = &p.container {
        kv.push(("container", c.clone()));
    }
    if let Some(a) = &p.audio_extract {
        kv.push(("audio", format!("converted to {a}")));
    }
    if p.playlist {
        let items = p.playlist_items.as_deref().unwrap_or("all");
        kv.push(("playlist", format!("yes (items: {items})")));
    }
    kv.push(("output", p.output_template.clone()));
    kv.push((
        "quality picker",
        if p.asks_quality {
            format!("would ask: {}", p.ask_reason)
        } else {
            format!("would not ask: {}", p.ask_reason)
        },
    ));
    let mut out = crate::format::key_values(&kv);
    out.push_str("\npost-processing:\n");
    if p.post_processing.is_empty() {
        out.push_str("  none\n");
    }
    for step in &p.post_processing {
        out.push_str(&format!("  - {step}\n"));
    }
    if !p.skipped.is_empty() {
        out.push_str("\nskipped (needs ffmpeg):\n");
        for step in &p.skipped {
            out.push_str(&format!("  - {step}\n"));
        }
    }
    for n in &p.notes {
        out.push_str(&format!("\nnote: {n}\n"));
    }
    out
}

/// `dman probe <url> --plan`: what a video download would use, without probing it.
pub async fn plan(ctx: &Ctx, url: &str, options: &VideoOptions) -> Result<i32, CliError> {
    let p = ctx
        .connect()
        .await?
        .video_plan(dman_proto::VideoPlanParams {
            url: url.to_owned(),
            options: options.clone(),
        })
        .await?;
    if ctx.json {
        super::print_json(&p)?;
    } else {
        print!("{}", plan_text(&p));
    }
    Ok(0)
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
    fn plan_options_carry_the_flags() {
        let a = crate::cli::PlanArgs {
            quality: Some("720p".into()),
            container: Some("mkv".into()),
            subs: vec!["en".into()],
            thumbnail: true,
            ..Default::default()
        };
        let o = plan_options(&a, true, Some("firefox".into()));
        assert_eq!(o.quality.as_deref(), Some("720p"));
        assert_eq!(o.container.as_deref(), Some("mkv"));
        assert!(o.playlist && o.embed_thumbnail == Some(true));
        assert_eq!(o.subtitles.unwrap().langs, ["en"]);
        assert_eq!(o.cookies_from_browser.as_deref(), Some("firefox"));
        assert_eq!(
            plan_options(&Default::default(), false, None),
            VideoOptions::default()
        );
    }

    #[test]
    fn plan_text_lists_selector_steps_and_fallback() {
        let p = dman_proto::VideoDownloadPlan {
            url: "https://e.com/v".into(),
            source: "defaults".into(),
            quality: Some("1080p".into()),
            selector: "b[height<=1080]/b".into(),
            ffmpeg: false,
            output_template: "%(title)s.%(ext)s".into(),
            post_processing: vec!["write subtitles (en) next to the video".into()],
            skipped: vec!["merge into a mkv container: yt-dlp keeps its own container".into()],
            asks_quality: true,
            ask_reason: "in a terminal".into(),
            notes: vec!["ffmpeg is missing: lower quality".into()],
            ..Default::default()
        };
        let t = plan_text(&p);
        assert!(
            t.contains("format:") && t.contains("1080p (saved defaults)"),
            "{t}"
        );
        assert!(
            t.contains("selector:") && t.contains("b[height<=1080]/b"),
            "{t}"
        );
        assert!(t.contains("ffmpeg:") && t.contains("missing"), "{t}");
        assert!(t.contains("would ask: in a terminal"), "{t}");
        assert!(t.contains("post-processing:\n  - write subtitles"), "{t}");
        assert!(
            t.contains("skipped (needs ffmpeg):\n  - merge into a mkv"),
            "{t}"
        );
        assert!(t.contains("note: ffmpeg is missing"), "{t}");
        let none = dman_proto::VideoDownloadPlan::default();
        assert!(plan_text(&none).contains("post-processing:\n  none"));
    }

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

    fn dup(trackers_added: u32) -> AddOutcome {
        let mut error =
            dman_proto::RpcError::refused("already in your downloads as job 4 (paused)");
        error.data = Some(
            dman_proto::DuplicateInfo {
                id: dman_proto::JobId(4),
                state: dman_proto::JobState::Paused,
                trackers_added,
            }
            .to_data(),
        );
        AddOutcome::Failed { error }
    }

    #[test]
    fn duplicates_have_their_own_exit_code() {
        let ok = AddOutcome::Added {
            id: dman_proto::JobId(1),
            extra_ids: vec![],
        };
        let bad = AddOutcome::Failed {
            error: dman_proto::RpcError::not_found("x"),
        };
        let exit = |o: &[AddOutcome]| Tally::of(o).exit(o.len());
        assert_eq!(exit(std::slice::from_ref(&ok)), 0);
        assert_eq!(exit(&[dup(0)]), EXIT_DUPLICATE);
        assert_eq!(exit(&[dup(0), dup(0)]), EXIT_DUPLICATE);
        // A torrent that only got new trackers is not a failure.
        assert_eq!(exit(&[dup(3)]), 0);
        assert_eq!(exit(&[ok.clone(), dup(0)]), 8);
        assert_eq!(exit(std::slice::from_ref(&bad)), 5);
        assert_eq!(exit(&[bad, dup(0)]), 5);
        let t = Tally::of(&[ok, dup(0), dup(2)]);
        assert_eq!((t.failed, t.duplicates, t.merged), (1, 1, 1));
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
