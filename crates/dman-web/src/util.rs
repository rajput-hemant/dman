//! Small formatters and parsers for form fields and timestamps. Sizes, speeds
//! and ETAs live in `dman_proto::fmt`.

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
        assert_eq!(rate_input(0), "");
        assert_eq!(rate_input(2 * 1024 * 1024), "2M");
        assert_eq!(rate_input(512_000), "500K");
        assert_eq!(rate_input(1000), "1000");
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
