//! Live progress display on stderr: an in-place redrawn bar on a terminal, plain
//! lines every few seconds elsewhere. Drawn with ANSI escapes only.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use dman_proto::{Job, JobState, SourceKind};

use crate::format;

/// How often a non-terminal stderr gets a new line per job.
const PLAIN_EVERY: Duration = Duration::from_secs(5);
/// Width assumed when neither `COLUMNS` nor the terminal tells: narrow enough that
/// a redrawn line rarely wraps (a wrapped line cannot be erased in place).
const FALLBACK_WIDTH: usize = 80;
const BAR_WIDTH: usize = 20;

/// Colours are used on a terminal unless `NO_COLOR` is set (and non-empty).
pub fn color_enabled(is_tty: bool) -> bool {
    is_tty && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
}

fn line_width() -> usize {
    format::columns_env()
        .or_else(dman_tui::terminal_columns)
        .unwrap_or(FALLBACK_WIDTH)
}

/// `████░░░░` for a fraction in `0.0..=1.0`.
pub fn bar(fraction: f64, width: usize) -> String {
    let filled = ((fraction.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

/// Peers and seeders of a torrent, e.g. `peers 12 seeders 5`.
fn swarm(job: &Job) -> Option<String> {
    if !matches!(job.kind, SourceKind::Magnet | SourceKind::Torrent) {
        return None;
    }
    Some(match job.seeders {
        Some(s) => format!("peers {} seeders {s}", job.connections),
        None => format!("peers {}", job.connections),
    })
}

/// What follows the bar: speed and ETA while running, else the state.
fn status(job: &Job) -> String {
    match job.state {
        JobState::Active => {
            let mut s = format!(
                "{}  eta {}",
                format::speed(job.download_speed),
                format::eta(job.eta_secs())
            );
            if let Some(w) = swarm(job) {
                s.push_str("  ");
                s.push_str(&w);
            }
            s
        }
        JobState::Seeding => {
            let mut s = format!("done, up {}", format::speed(job.upload_speed));
            if let Some(w) = swarm(job) {
                s.push_str("  ");
                s.push_str(&w);
            }
            s
        }
        JobState::Complete => format!("done  {}", format::size(job.total_bytes)),
        JobState::Error => format!("error: {}", job.error.as_deref().unwrap_or("failed")),
        other => format::state(other).to_owned(),
    }
}

/// The percentage text, `-` while the size is unknown.
fn percent(job: &Job) -> String {
    if job.total_bytes == 0 {
        "-".into()
    } else {
        format!("{:.1}%", job.progress() * 100.0)
    }
}

/// One job as `#12 name  45.2% [██████░░░░░░░░░░░░░░] 3.1 MiB/s  eta 5m03s`,
/// fitted to `width` columns by shortening the name (and, on very narrow
/// terminals, dropping the bar and cutting the status). The line stays one
/// column short of `width` so that the terminal never wraps it.
pub fn line(job: &Job, width: usize, color: bool) -> String {
    let fraction = if job.state == JobState::Complete {
        1.0
    } else {
        job.progress()
    };
    let width = width.max(20) - 1;
    let id = format!("#{}", job.id);
    let pct = format!("{:>6}", percent(job));
    let status = format::printable(&status(job)).into_owned();
    let name = format::printable(&job.name).into_owned();
    let bar_width = match width {
        0..60 => 0,
        60..70 => BAR_WIDTH / 2,
        _ => BAR_WIDTH,
    };
    // Room for the status once the id, percentage, bar and a short name fit.
    let room = width.saturating_sub(id.chars().count() + pct.len() + bar_width + 16);
    if bar_width == 0 || room < 12 {
        // `#12 name  45.2% status`, cut to the width.
        let status = format::truncate(&status, width.saturating_sub(id.len() + 18).max(8));
        let fixed = id.chars().count() + 1 + 2 + pct.len() + 1 + status.chars().count();
        let name = format::truncate(&name, width.saturating_sub(fixed).max(4));
        return format::truncate(&format!("{id} {name}  {pct} {status}"), width);
    }
    let status = format::truncate(&status, room.min(width.saturating_sub(40).max(16)));
    let fixed = id.chars().count() + pct.len() + bar_width + 2 + status.chars().count() + 6;
    let name_width = width.saturating_sub(fixed).max(8);
    let name = format::truncate(&name, name_width);
    let bar = bar(fraction, bar_width);
    let (on, off) = match (color, job.state) {
        (true, JobState::Complete | JobState::Seeding) => ("\x1b[32m", "\x1b[0m"),
        (true, JobState::Error) => ("\x1b[31m", "\x1b[0m"),
        _ => ("", ""),
    };
    format!("{id} {name}  {pct} [{on}{bar}{off}] {status}")
}

/// A line for logs: no bar, no escapes.
pub fn plain_line(job: &Job) -> String {
    let text = format!(
        "#{} {}  {}  {}",
        job.id,
        job.name,
        percent(job),
        status(job)
    );
    format::printable(&text).into_owned()
}

/// Draws progress for a set of jobs on stderr.
pub struct Renderer {
    tty: bool,
    color: bool,
    plain: bool,
    drawn: usize,
    last_plain: Option<Instant>,
}

impl Renderer {
    /// `plain_fallback`: print periodic plain lines when stderr is not a terminal
    /// (otherwise nothing is drawn there).
    pub fn new(plain_fallback: bool) -> Self {
        let tty = std::io::stderr().is_terminal();
        Self {
            tty,
            color: color_enabled(tty),
            plain: plain_fallback && !tty,
            drawn: 0,
            last_plain: None,
        }
    }

    /// Show the current state of `jobs`. `last` forces output of the final state.
    pub fn draw(&mut self, jobs: &[&Job], footer: Option<&str>, last: bool) {
        let mut err = std::io::stderr().lock();
        if self.tty {
            let width = line_width();
            let mut frame = String::new();
            if self.drawn > 0 {
                frame.push_str(&format!("\x1b[{}A", self.drawn));
            }
            for j in jobs {
                frame.push_str("\r\x1b[2K");
                frame.push_str(&line(j, width, self.color));
                frame.push('\n');
            }
            if let Some(f) = footer {
                frame.push_str("\r\x1b[2K");
                frame.push_str(&format::truncate(f, width.saturating_sub(1)));
                frame.push('\n');
            }
            self.drawn = jobs.len() + usize::from(footer.is_some());
            let _ = err.write_all(frame.as_bytes());
            let _ = err.flush();
        } else if self.plain {
            let due = self.last_plain.is_none_or(|t| t.elapsed() >= PLAIN_EVERY);
            if due || last {
                self.last_plain = Some(Instant::now());
                for j in jobs {
                    let _ = writeln!(err, "{}", plain_line(j));
                }
                if let Some(f) = footer {
                    let _ = writeln!(err, "{f}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dman_proto::{Engine, JobId};

    pub(crate) fn job(state: JobState, done: u64, total: u64, speed: u64) -> Job {
        Job {
            id: JobId(7),
            engine: Engine::Aria2,
            kind: SourceKind::Http,
            name: "ubuntu-24.04-desktop-amd64.iso".into(),
            state,
            dir: "/".into(),
            source: "s".into(),
            total_bytes: total,
            completed_bytes: done,
            uploaded_bytes: 0,
            download_speed: speed,
            upload_speed: 0,
            connections: 4,
            seeders: None,
            info_hash: None,
            added_at: 0,
            completed_at: None,
            queue_position: None,
            error: None,
        }
    }

    #[test]
    fn bar_fills_proportionally() {
        assert_eq!(bar(0.0, 4), "░░░░");
        assert_eq!(bar(0.5, 4), "██░░");
        assert_eq!(bar(1.0, 4), "████");
        assert_eq!(bar(7.0, 4), "████");
        assert_eq!(bar(-1.0, 4), "░░░░");
    }

    #[test]
    fn active_line_shows_speed_and_eta() {
        let j = job(JobState::Active, 512 << 20, 1024 << 20, 1 << 20);
        let l = line(&j, 120, false);
        assert!(l.starts_with("#7 ubuntu"), "{l}");
        assert!(l.contains(" 50.0% ["), "{l}");
        assert!(l.contains("1.0 MiB/s"), "{l}");
        assert!(l.contains("eta 8m32s"), "{l}");
        assert!(!l.contains('\x1b'));
    }

    #[test]
    fn line_fits_the_width() {
        let j = job(JobState::Active, 1, 100, 1);
        for w in [10, 30, 45, 59, 60, 69, 70, 80, 120] {
            assert!(line(&j, w, false).chars().count() < w.max(20), "width {w}");
        }
        let mut long = job(JobState::Error, 1, 100, 1);
        long.name = "n".repeat(300);
        long.error = Some("e".repeat(300));
        for w in [30, 50, 80] {
            assert!(line(&long, w, false).chars().count() < w, "width {w}");
        }
    }

    #[test]
    fn torrent_line_shows_peers_and_seeders() {
        let mut j = job(JobState::Active, 1, 100, 10);
        j.kind = SourceKind::Magnet;
        j.seeders = Some(5);
        j.connections = 12;
        assert!(line(&j, 140, false).contains("peers 12 seeders 5"));
    }

    #[test]
    fn finished_states() {
        let done = job(JobState::Complete, 100, 100, 0);
        let l = line(&done, 120, false);
        assert!(l.contains("100.0%") && l.contains("done"), "{l}");
        let mut bad = job(JobState::Error, 10, 100, 0);
        bad.error = Some("404 not found".into());
        assert!(line(&bad, 120, false).contains("error: 404 not found"));
        assert!(plain_line(&bad).contains("error: 404"));
    }

    #[test]
    fn color_only_with_a_terminal_and_without_no_color() {
        let j = job(JobState::Complete, 1, 1, 0);
        assert!(line(&j, 120, true).contains("\x1b[32m"));
        assert!(!line(&j, 120, false).contains('\x1b'));
        assert!(!color_enabled(false));
    }
}
