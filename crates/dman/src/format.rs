//! Human-readable formatting: sizes, speeds, ETAs and the job table.

use bytesize::ByteSize;
use dman_proto::{FileInfo, Job, JobState, PeerInfo};

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
    // STATE (index 1) is left-aligned; the other columns are right-aligned.
    let rows: Vec<[String; 7]> = jobs.iter().map(job_row).collect();
    let mut widths = HEADERS.map(str::len);
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let used: usize = widths.iter().map(|w| w + COLUMN_GAP.len()).sum();
    let name_width = width.saturating_sub(used).max(MIN_NAME_WIDTH);

    let line = |cells: [&str; 7], name: &str| {
        let mut out = String::new();
        for (i, cell) in cells.iter().enumerate() {
            let w = widths[i];
            if i == 1 {
                out.push_str(&format!("{cell:<w$}"));
            } else {
                out.push_str(&format!("{cell:>w$}"));
            }
            out.push_str(COLUMN_GAP);
        }
        out.push_str(name);
        out.push('\n');
        out
    };

    let mut out = line(HEADERS, "NAME");
    for (job, row) in jobs.iter().zip(&rows) {
        let cells = row.each_ref().map(String::as_str);
        out.push_str(&line(cells, &truncate(&job.name, name_width)));
    }
    out
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
