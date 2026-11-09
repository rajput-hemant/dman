//! Human-readable sizes, speeds, ETAs and text helpers.

use bytesize::ByteSize;
use dman_proto::{Job, JobState};

/// `1.2 MiB`.
pub fn size(bytes: u64) -> String {
    ByteSize::b(bytes).display().iec().to_string()
}

/// `1.2 MiB`, or `-` for 0 (unknown size).
pub fn size_or_dash(bytes: u64) -> String {
    if bytes == 0 { "-".into() } else { size(bytes) }
}

/// `1.2 MiB/s`.
pub fn speed(bytes_per_sec: u64) -> String {
    format!("{}/s", size(bytes_per_sec))
}

/// A speed, or `-` when zero.
pub fn speed_or_dash(bytes_per_sec: u64) -> String {
    if bytes_per_sec == 0 {
        "-".into()
    } else {
        speed(bytes_per_sec)
    }
}

/// A speed limit where 0 means unlimited.
pub fn limit(bytes_per_sec: u64) -> String {
    if bytes_per_sec == 0 {
        "unlimited".into()
    } else {
        speed(bytes_per_sec)
    }
}

/// `45s`, `5m03s`, `1h02m`, `3d04h`; `-` when unknown.
pub fn eta(secs: Option<u64>) -> String {
    let Some(s) = secs else { return "-".into() };
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        3600..=86_399 => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d{:02}h", s / 86_400, (s % 86_400) / 3600),
    }
}

/// Short state label (fits 8 columns).
pub const fn state(s: JobState) -> &'static str {
    match s {
        JobState::Waiting => "waiting",
        JobState::Active => "active",
        JobState::Seeding => "seeding",
        JobState::Paused => "paused",
        JobState::Complete => "done",
        JobState::Error => "error",
        JobState::Removed => "removed",
    }
}

/// Text progress bar of `width` cells followed by a percentage, e.g.
/// `█████░░░  62%`. Unknown sizes render as an empty bar with `  -`.
pub fn progress_bar(job: &Job, width: usize) -> String {
    let known = job.total_bytes > 0;
    let p = if known {
        job.progress().clamp(0.0, 1.0)
    } else {
        0.0
    };
    // Truncation is intended: a cell is only full when reached.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let filled = ((p * width as f64).floor() as usize).min(width);
    let mut out = String::with_capacity(width * 3 + 5);
    out.extend(std::iter::repeat_n('█', filled));
    out.extend(std::iter::repeat_n('░', width - filled));
    if known {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pct = (p * 100.0).floor() as u32;
        out.push_str(&format!(" {pct:>3}%"));
    } else {
        out.push_str("    -");
    }
    out
}

/// `1.23`, or `-` when nothing was downloaded or uploaded yet.
pub fn ratio(job: &Job) -> String {
    if job.completed_bytes == 0 || job.uploaded_bytes == 0 {
        "-".into()
    } else {
        format!("{:.2}", job.ratio())
    }
}

/// `connections/seeders` for torrents, `connections` otherwise.
pub fn peers(job: &Job) -> String {
    match job.seeders {
        Some(s) => format!("{}/{}", job.connections, s),
        None if job.connections > 0 => job.connections.to_string(),
        None => "-".into(),
    }
}

/// Shorten `s` to at most `max` characters, ending in `…` when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    if max > 0 {
        out.push('…');
    }
    out
}

/// `3 downloads` / `1 download`.
pub fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eta_formats() {
        assert_eq!(eta(None), "-");
        assert_eq!(eta(Some(5)), "5s");
        assert_eq!(eta(Some(303)), "5m03s");
        assert_eq!(eta(Some(3720)), "1h02m");
        assert_eq!(eta(Some(90_000)), "1d01h");
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn plurals() {
        assert_eq!(plural(1, "download"), "1 download");
        assert_eq!(plural(3, "download"), "3 downloads");
    }
}
