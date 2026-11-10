//! Human-readable formatting: sizes, speeds, ETAs and the job table.

use bytesize::ByteSize;
use dman_proto::search::{ProviderInfo, SearchHistoryEntry, SearchResultDto};
use dman_proto::{FileInfo, Job, JobState, PeerInfo};

mod schedule;

pub use schedule::{limit_lines, reload_text, schedule_text};

const DEFAULT_WIDTH: usize = 120;
const MIN_NAME_WIDTH: usize = 10;
const COLUMN_GAP: &str = "  ";

/// Terminal width from `COLUMNS`, default 120.
pub fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.trim().parse().ok())
        .filter(|w| *w > 0)
        .unwrap_or(DEFAULT_WIDTH)
}

/// `1.2 MiB`.
pub fn size(bytes: u64) -> String {
    ByteSize::b(bytes).display().iec().to_string()
}

/// `1.2 MiB/s`.
pub fn speed(bytes_per_sec: u64) -> String {
    format!("{}/s", size(bytes_per_sec))
}

/// A speed limit where 0 means unlimited.
pub fn limit(bytes_per_sec: u64) -> String {
    if bytes_per_sec == 0 {
        "unlimited".into()
    } else {
        speed(bytes_per_sec)
    }
}

/// `45s`, `5m03s`, `1h02m`; `-` when unknown.
pub fn eta(secs: Option<u64>) -> String {
    let Some(s) = secs else { return "-".into() };
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// Lower-case state label.
pub fn state(s: JobState) -> &'static str {
    match s {
        JobState::Waiting => "waiting",
        JobState::Active => "active",
        JobState::Seeding => "seeding",
        JobState::Paused => "paused",
        JobState::Complete => "complete",
        JobState::Error => "error",
        JobState::Removed => "removed",
    }
}

/// `45.2%`, or `-` while the size is unknown.
pub fn progress(job: &Job) -> String {
    if job.total_bytes == 0 {
        "-".into()
    } else {
        format!("{:.1}%", job.progress() * 100.0)
    }
}

/// Shorten `s` to at most `max` characters, ending in `…` when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

fn rate_or_dash(v: u64) -> String {
    if v == 0 { "-".into() } else { speed(v) }
}

fn job_row(j: &Job) -> [String; 7] {
    [
        j.id.to_string(),
        state(j.state).to_owned(),
        progress(j),
        rate_or_dash(j.download_speed),
        rate_or_dash(j.upload_speed),
        eta(j.eta_secs()),
        if j.total_bytes == 0 {
            "-".into()
        } else {
            size(j.total_bytes)
        },
    ]
}

/// Render the job table (ID STATE PROGRESS DOWN UP ETA SIZE NAME) for a terminal
/// of `width` columns; NAME is truncated to the remaining space.
pub fn job_table(jobs: &[Job], width: usize) -> String {
    const HEADERS: [&str; 7] = ["ID", "STATE", "PROGRESS", "DOWN", "UP", "ETA", "SIZE"];
    let rows: Vec<[String; 7]> = jobs.iter().map(job_row).collect();
    let mut widths = HEADERS.map(str::len);
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let used: usize = widths.iter().map(|w| w + COLUMN_GAP.len()).sum();
    let name_width = width.saturating_sub(used).max(MIN_NAME_WIDTH);
    let rows: Vec<[String; 8]> = jobs
        .iter()
        .zip(rows)
        .map(|(j, [id, st, pr, dn, up, eta, sz])| {
            [id, st, pr, dn, up, eta, sz, truncate(&j.name, name_width)]
        })
        .collect();
    // STATE (index 1) is left-aligned; the other columns are right-aligned.
    table(
        [
            "ID", "STATE", "PROGRESS", "DOWN", "UP", "ETA", "SIZE", "NAME",
        ],
        &rows,
        &[1],
    )
}

/// Generic table: `left` lists the columns that are left-aligned (the last
/// column is never padded).
fn table<const N: usize>(headers: [&str; N], rows: &[[String; N]], left: &[usize]) -> String {
    let mut widths = headers.map(|h| h.chars().count());
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let mut out = String::new();
        for (i, cell) in cells.iter().enumerate() {
            let w = widths[i];
            if i + 1 == N {
                out.push_str(cell);
            } else {
                if left.contains(&i) {
                    out.push_str(&format!("{cell:<w$}"));
                } else {
                    out.push_str(&format!("{cell:>w$}"));
                }
                out.push_str(COLUMN_GAP);
            }
        }
        out.push('\n');
        out
    };
    let mut out = line(headers.to_vec());
    for row in rows {
        out.push_str(&line(row.iter().map(String::as_str).collect()));
    }
    out
}

/// `video.probe` format table: `ID  EXT  KIND  RES  FPS  SIZE  CODECS  NOTE`.
pub fn video_formats_table(formats: &[dman_proto::video::VideoFormat]) -> String {
    use dman_proto::video::VideoStreamKind as K;
    let rows: Vec<[String; 8]> = formats
        .iter()
        .map(|f| {
            let kind = match f.kind {
                K::Combined => "video+audio",
                K::VideoOnly => "video only",
                K::AudioOnly => "audio only",
                K::Other => "other",
            };
            let res = match (f.width, f.height) {
                (Some(w), Some(h)) => format!("{w}x{h}"),
                (None, Some(h)) => format!("{h}p"),
                _ => "-".into(),
            };
            let codec = |c: &Option<String>| c.clone().filter(|c| c != "none");
            let codecs = [codec(&f.vcodec), codec(&f.acodec)]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("/");
            [
                f.id.clone(),
                f.ext.clone(),
                kind.to_owned(),
                res,
                f.fps.map_or("-".into(), |x| format!("{x:.0}")),
                f.size.map_or("-".into(), size),
                if codecs.is_empty() {
                    "-".into()
                } else {
                    codecs
                },
                f.note.clone().unwrap_or_default(),
            ]
        })
        .collect();
    table(
        ["ID", "EXT", "KIND", "RES", "FPS", "SIZE", "CODECS", "NOTE"],
        &rows,
        &[0, 1, 2, 6],
    )
}

/// Quality plans of a probe: `QUALITY  PLAN  EXPLANATION`.
pub fn video_plans_table(plans: &[dman_proto::video::VideoPlan]) -> String {
    use dman_proto::video::VideoPlanKind as K;
    let kind = |k: K| match k {
        K::Merged => "merged",
        K::Single => "single file",
        K::Separate => "separate files",
        K::Audio => "audio",
    };
    let mut rows: Vec<[String; 3]> = Vec::new();
    for p in plans {
        rows.push([p.preset.clone(), kind(p.kind).into(), p.explanation.clone()]);
        if let Some(a) = &p.alternative {
            rows.push([
                String::new(),
                format!("or {}", kind(a.kind)),
                format!("{} (--allow-separate-streams)", a.explanation),
            ]);
        }
    }
    table(["QUALITY", "PLAN", "EXPLANATION"], &rows, &[0, 1])
}

/// Playlist entries of a probe: `#  DURATION  TITLE`.
pub fn playlist_table(entries: &[dman_proto::video::PlaylistEntry]) -> String {
    let rows: Vec<[String; 3]> = entries
        .iter()
        .map(|e| {
            [
                e.index.to_string(),
                e.duration.map_or("-".into(), duration),
                e.title.clone().unwrap_or_default(),
            ]
        })
        .collect();
    table(["#", "DURATION", "TITLE"], &rows, &[])
}

/// `h:mm:ss` or `m:ss`.
pub fn duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// File table: `#  DONE  SIZE  SEL  PATH`.
pub fn file_table(files: &[FileInfo]) -> String {
    let rows: Vec<[String; 5]> = files
        .iter()
        .map(|f| {
            [
                f.index.to_string(),
                if f.length == 0 {
                    "-".into()
                } else {
                    format!("{:.1}%", f.completed as f64 / f.length as f64 * 100.0)
                },
                if f.length == 0 {
                    "-".into()
                } else {
                    size(f.length)
                },
                if f.selected { "yes" } else { "no" }.into(),
                f.path.clone(),
            ]
        })
        .collect();
    table(["#", "DONE", "SIZE", "SEL", "PATH"], &rows, &[3])
}

/// File list of a magnet preview: `#  SIZE  PATH`.
pub fn preview_table(files: &[FileInfo]) -> String {
    let rows: Vec<[String; 3]> = files
        .iter()
        .map(|f| [f.index.to_string(), size(f.length), f.path.clone()])
        .collect();
    table(["#", "SIZE", "PATH"], &rows, &[])
}

/// Peer table: `ADDRESS  PROGRESS  DOWN  UP  FLAGS`.
pub fn peer_table(peers: &[PeerInfo]) -> String {
    let rows: Vec<[String; 5]> = peers
        .iter()
        .map(|p| {
            [
                if p.ip.contains(':') {
                    format!("[{}]:{}", p.ip, p.port)
                } else {
                    format!("{}:{}", p.ip, p.port)
                },
                format!("{:.1}%", p.progress * 100.0),
                rate_or_dash(p.download_speed),
                rate_or_dash(p.upload_speed),
                if p.seeder { "seeder" } else { "" }.into(),
            ]
        })
        .collect();
    table(["ADDRESS", "PROGRESS", "DOWN", "UP", "FLAGS"], &rows, &[0])
}

/// `2024-05-01` (UTC), or `-` when unknown.
pub fn date(secs: Option<i64>) -> String {
    secs.and_then(|s| jiff::Timestamp::from_second(s).ok())
        .map_or_else(
            || "-".into(),
            |t| t.to_zoned(jiff::tz::TimeZone::UTC).date().to_string(),
        )
}

/// `12/3` (seeders/leechers), `-` when neither is known.
pub fn seeds(s: Option<u32>, l: Option<u32>) -> String {
    if s.is_none() && l.is_none() {
        return "-".into();
    }
    format!(
        "{}/{}",
        s.map_or_else(|| "?".into(), |v| v.to_string()),
        l.map_or_else(|| "?".into(), |v| v.to_string())
    )
}

/// Ranked search results: `#  TITLE  SIZE  S/L  SOURCES  DATE`. `first` is
/// the number of the first row (1-based); TITLE takes the remaining width.
pub fn search_table(results: &[SearchResultDto], first: usize, width: usize) -> String {
    let rows: Vec<[String; 6]> = results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            [
                (first + i).to_string(),
                r.title.clone(),
                r.size.map_or_else(|| "-".into(), size),
                seeds(r.seeders, r.leechers),
                r.sources.join(","),
                date(r.date),
            ]
        })
        .collect();
    let headers = ["#", "TITLE", "SIZE", "S/L", "SOURCES", "DATE"];
    let mut widths = headers.map(str::len);
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let others: usize = widths
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 1)
        .map(|(_, w)| w + COLUMN_GAP.len())
        .sum();
    let title_w = width
        .saturating_sub(others + COLUMN_GAP.len())
        .max(MIN_NAME_WIDTH);
    let rows: Vec<[String; 6]> = rows
        .into_iter()
        .map(|mut r| {
            r[1] = truncate(&r[1], title_w);
            r
        })
        .collect();
    // # right, TITLE left, SIZE right, S/L right, SOURCES left, DATE last.
    table(headers, &rows, &[1, 4])
}

/// Providers: `NAME KIND ENABLED ORDER TIMEOUT VERIFIED BUILTIN URL`.
pub fn providers_table(list: &[ProviderInfo]) -> String {
    let yn = |b: bool| if b { "yes" } else { "no" }.to_owned();
    let rows: Vec<[String; 8]> = list
        .iter()
        .map(|p| {
            [
                p.name.clone(),
                p.kind.clone(),
                yn(p.enabled),
                p.order.to_string(),
                format!("{}s", p.timeout_secs),
                yn(p.verified),
                if p.builtin { "built-in" } else { "custom" }.to_owned(),
                p.url.clone(),
            ]
        })
        .collect();
    table(
        [
            "NAME", "KIND", "ENABLED", "ORDER", "TIMEOUT", "VERIFIED", "ORIGIN", "URL",
        ],
        &rows,
        &[0, 1, 2, 5, 6],
    )
}

/// Recent searches: `WHEN  RESULTS  PROVIDERS  QUERY`.
pub fn history_table(list: &[SearchHistoryEntry]) -> String {
    let rows: Vec<[String; 4]> = list
        .iter()
        .map(|e| {
            [
                date(Some(e.at)),
                e.result_count.to_string(),
                if e.providers.is_empty() {
                    "all".to_owned()
                } else {
                    e.providers.join(",")
                },
                e.query.clone(),
            ]
        })
        .collect();
    table(["DATE", "RESULTS", "PROVIDERS", "QUERY"], &rows, &[0, 2])
}

/// Aligned `key: value` lines.
pub fn key_values(pairs: &[(&str, String)]) -> String {
    let w = pairs.iter().map(|(k, _)| k.len() + 1).max().unwrap_or(0);
    pairs
        .iter()
        .map(|(k, v)| format!("{:<w$}  {v}\n", format!("{k}:")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_tables() {
        use dman_proto::video::*;
        assert_eq!(duration(65.4), "1:05");
        assert_eq!(duration(3725.0), "1:02:05");
        let t = video_formats_table(&[VideoFormat {
            id: "137".into(),
            ext: "mp4".into(),
            vcodec: Some("avc1".into()),
            acodec: Some("none".into()),
            width: None,
            height: Some(1080),
            fps: Some(30.0),
            size: Some(1 << 20),
            note: Some("1080p".into()),
            kind: VideoStreamKind::VideoOnly,
        }]);
        assert!(
            t.contains("video only") && t.contains("1080p") && t.contains("avc1"),
            "{t}"
        );
        let p = video_plans_table(&[VideoPlan {
            preset: "best".into(),
            kind: VideoPlanKind::Single,
            selector: "b".into(),
            explanation: "x".into(),
            alternative: Some(Box::new(VideoPlan {
                preset: "best".into(),
                kind: VideoPlanKind::Separate,
                selector: "bv*+ba/b".into(),
                explanation: "y".into(),
                alternative: None,
            })),
        }]);
        assert!(p.contains("or separate files"), "{p}");
    }

    fn result(title: &str, size: Option<u64>, s: Option<u32>, l: Option<u32>) -> SearchResultDto {
        SearchResultDto {
            title: title.into(),
            size,
            seeders: s,
            leechers: l,
            date: Some(1_714_521_600),
            sources: vec!["Nyaa".into(), "YTS".into()],
            ..SearchResultDto::default()
        }
    }

    #[test]
    fn search_table_columns_numbers_and_truncation() {
        let rs = vec![
            result(
                "Ubuntu 24.04 desktop amd64",
                Some(6 << 30),
                Some(120),
                Some(4),
            ),
            result(&"x".repeat(200), None, None, None),
        ];
        let t = search_table(&rs, 1, 80);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines.len(), 3);
        for h in ["#", "TITLE", "SIZE", "S/L", "SOURCES", "DATE"] {
            assert!(lines[0].contains(h), "{}", lines[0]);
        }
        assert!(
            lines[1].starts_with("1  Ubuntu 24.04 desktop amd64"),
            "{}",
            lines[1]
        );
        assert!(
            lines[1].contains("120/4")
                && lines[1].contains("Nyaa,YTS")
                && lines[1].contains("2024-05-01")
        );
        assert!(lines[2].trim_start().starts_with('2') && lines[2].contains('…'));
        assert!(lines.iter().all(|l| l.chars().count() <= 80), "{t}");
        // Numbering continues from `first`.
        assert!(
            search_table(&rs[..1], 21, 80)
                .lines()
                .nth(1)
                .unwrap()
                .trim_start()
                .starts_with("21")
        );
    }

    #[test]
    fn seeds_and_dates() {
        assert_eq!(seeds(Some(3), None), "3/?");
        assert_eq!(seeds(None, None), "-");
        assert_eq!(date(None), "-");
        assert_eq!(date(Some(0)), "1970-01-01");
    }

    #[test]
    fn providers_and_history_tables() {
        let p = ProviderInfo {
            name: "Nyaa".into(),
            kind: "feed".into(),
            enabled: true,
            order: 3,
            timeout_secs: 15,
            verified: false,
            builtin: true,
            url: "https://nyaa.si/".into(),
            description: None,
            rate_limit_per_min: None,
            max_results: 100,
            api_key_ref: None,
        };
        let t = providers_table(&[p]);
        assert!(
            t.lines().nth(1).unwrap().contains("Nyaa")
                && t.contains("built-in")
                && t.contains("15s")
        );
        let h = history_table(&[SearchHistoryEntry {
            query: "ubuntu".into(),
            providers: vec![],
            category: None,
            at: 0,
            result_count: 7,
        }]);
        assert!(h.lines().nth(1).unwrap().contains("all") && h.contains("ubuntu"));
    }
    use dman_proto::{Engine, JobId, SourceKind};

    fn job(id: u64, name: &str) -> Job {
        Job {
            id: JobId(id),
            engine: Engine::Aria2,
            kind: SourceKind::Http,
            name: name.into(),
            state: JobState::Active,
            dir: "/tmp".into(),
            source: "http://x".into(),
            total_bytes: 10 << 20,
            completed_bytes: 5 << 20,
            uploaded_bytes: 0,
            download_speed: 1_258_291,
            upload_speed: 0,
            connections: 1,
            seeders: None,
            info_hash: None,
            added_at: 0,
            completed_at: None,
            queue_position: None,
            error: None,
        }
    }

    #[test]
    fn sizes_and_speeds() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1_258_291), "1.2 MiB");
        assert_eq!(speed(1_258_291), "1.2 MiB/s");
        assert_eq!(limit(0), "unlimited");
    }

    #[test]
    fn eta_formats() {
        assert_eq!(eta(None), "-");
        assert_eq!(eta(Some(45)), "45s");
        assert_eq!(eta(Some(303)), "5m03s");
        assert_eq!(eta(Some(3720)), "1h02m");
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("abcdef", 6), "abcdef");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("ééééé", 3), "éé…");
    }

    #[test]
    fn progress_text() {
        let mut j = job(1, "a");
        assert_eq!(progress(&j), "50.0%");
        j.total_bytes = 0;
        assert_eq!(progress(&j), "-");
    }

    #[test]
    fn table_truncates_name_to_width() {
        let long = "x".repeat(200);
        let t = job_table(&[job(1, &long), job(22, "short")], 80);
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[0].starts_with(" ID  STATE") || lines[0].starts_with("ID"));
        assert!(lines[0].ends_with("NAME"));
        assert!(lines.iter().all(|l| l.chars().count() <= 80), "{t}");
        assert!(lines[1].ends_with('…'));
        assert!(lines[2].ends_with("short"));
    }

    #[test]
    fn file_and_peer_tables() {
        let t = file_table(&[
            FileInfo {
                index: 1,
                path: "/dl/a.bin".into(),
                length: 2 << 20,
                completed: 1 << 20,
                selected: true,
            },
            FileInfo {
                index: 12,
                path: "/dl/b".into(),
                length: 0,
                completed: 0,
                selected: false,
            },
        ]);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with(" #   DONE"), "{t}");
        assert!(lines[1].contains("50.0%") && lines[1].ends_with("/dl/a.bin"));
        assert!(lines[2].contains("no   /dl/b"), "{t}");
        let t = peer_table(&[PeerInfo {
            ip: "::1".into(),
            port: 6881,
            download_speed: 0,
            upload_speed: 1024,
            seeder: true,
            progress: 1.0,
        }]);
        assert!(t.lines().nth(1).unwrap().starts_with("[::1]:6881"), "{t}");
        assert!(t.trim_end().ends_with("seeder"));
    }

    #[test]
    fn key_values_align() {
        let s = key_values(&[("id", "1".into()), ("name", "x".into())]);
        assert_eq!(s, "id:    1\nname:  x\n");
    }
}
