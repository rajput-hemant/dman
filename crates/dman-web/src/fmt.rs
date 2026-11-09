//! Human-readable formatting and small parsers shared by the views.

/// `1536` -> `1.5 KiB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Bytes per second; zero shows as a dash.
pub fn speed(n: u64) -> String {
    if n == 0 {
        "-".into()
    } else {
        format!("{}/s", bytes(n))
    }
}

/// Bytes per second; zero shows as `0 B/s`.
pub fn speed0(n: u64) -> String {
    format!("{}/s", bytes(n))
}

/// A rate as typed into a form: `""` for no limit, else `2M`, `500K` or bytes.
pub fn rate_input(n: u64) -> String {
    const K: u64 = 1024;
    match n {
        0 => String::new(),
        n if n % (K * K) == 0 => format!("{}M", n / (K * K)),
        n if n % K == 0 => format!("{}K", n / K),
        n => n.to_string(),
    }
}

/// Seconds as `1h 02m`, `3m 04s` or `12s`.
pub fn eta(secs: Option<u64>) -> String {
    let Some(s) = secs else {
        return "-".into();
    };
    let (d, h, m, sec) = (s / 86_400, s % 86_400 / 3600, s % 3600 / 60, s % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {sec:02}s")
    } else {
        format!("{sec}s")
    }
}

/// Unix seconds as `YYYY-MM-DD HH:MM` (UTC).
pub fn timestamp(t: i64) -> String {
    let days = t.div_euclid(86_400);
    let rem = t.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60
    )
}

/// Parses `500K`, `2M`, `1.5G`, `1024` (bytes), `0` / `unlimited` (= 0).
/// Returns `None` for garbage.
pub fn parse_rate(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    if s == "unlimited" || s == "none" {
        return Some(0);
    }
    let s = s.trim_end_matches("/s").trim_end_matches('b');
    let s = s.trim_end_matches('i');
    let (num, mult) = match s.chars().last()? {
        'k' => (&s[..s.len() - 1], 1024.0),
        'm' => (&s[..s.len() - 1], 1024.0 * 1024.0),
        'g' => (&s[..s.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (s, 1.0),
    };
    let v: f64 = num.trim().parse().ok()?;
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    Some((v * mult) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(5), "5 B");
        assert_eq!(speed(0), "-");
        assert_eq!(speed0(0), "0 B/s");
        assert_eq!(rate_input(0), "");
        assert_eq!(rate_input(2 * 1024 * 1024), "2M");
        assert_eq!(rate_input(512_000), "500K");
        assert_eq!(rate_input(1000), "1000");
        assert_eq!(eta(Some(3725)), "1h 02m");
        assert_eq!(timestamp(0), "1970-01-01 00:00");
        assert_eq!(timestamp(1_700_000_000), "2023-11-14 22:13");
    }

    #[test]
    fn rates() {
        assert_eq!(parse_rate("500K"), Some(512_000));
        assert_eq!(parse_rate("2m"), Some(2 * 1024 * 1024));
        assert_eq!(parse_rate("0"), Some(0));
        assert_eq!(parse_rate("unlimited"), Some(0));
        assert_eq!(parse_rate("1.5 MiB/s"), Some(1_572_864));
        assert_eq!(parse_rate("abc"), None);
        assert_eq!(parse_rate("-3"), None);
    }
}
