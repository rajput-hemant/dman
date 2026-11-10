//! Shared helpers: size and date parsing, info-hash handling, URL templates.

use std::time::{SystemTime, UNIX_EPOCH};

/// Collapse runs of whitespace to single spaces and trim.
pub fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------- hashes

/// Decode RFC 4648 base32 (case-insensitive, no padding required).
fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.trim_end_matches('=').chars() {
        let v = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u32 - 'A' as u32,
            c @ '2'..='7' => c as u32 - '2' as u32 + 26,
            _ => return None,
        };
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(40), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Normalize an info hash given as 40 hex or 32 base32 characters to
/// lowercase hex. Returns `None` for anything else.
pub fn normalize_info_hash(s: &str) -> Option<String> {
    let s = s.trim();
    match s.len() {
        40 if s.bytes().all(|b| b.is_ascii_hexdigit()) => Some(s.to_ascii_lowercase()),
        32 => base32_decode(s).filter(|b| b.len() == 20).map(|b| hex(&b)),
        _ => None,
    }
}

/// Extract the BitTorrent v1 info hash (`xt=urn:btih:`) from a magnet link.
pub fn info_hash_from_magnet(magnet: &str) -> Option<String> {
    let query = magnet.trim().split_once('?')?.1;
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        if !k.eq_ignore_ascii_case("xt") {
            return None;
        }
        let v = percent_decode(v);
        let lower = v.to_ascii_lowercase();
        let hash = lower.strip_prefix("urn:btih:")?;
        // base32 is case-insensitive but keep the original casing for decoding
        normalize_info_hash(&v[v.len() - hash.len()..])
    })
}

/// Build a magnet URI from a hash, display name and trackers.
pub fn build_magnet(hash: &str, name: &str, trackers: &[&str]) -> String {
    let mut m = format!("magnet:?xt=urn:btih:{hash}");
    if !name.is_empty() {
        m.push_str("&dn=");
        m.push_str(&percent_encode(name));
    }
    for t in trackers {
        m.push_str("&tr=");
        m.push_str(&percent_encode(t));
    }
    m
}

// ------------------------------------------------------- percent coding

/// Percent-encode everything except RFC 3986 unreserved characters.
pub fn percent_encode(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Decode `%XX` escapes and `+` (as space); invalid escapes are kept as is.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                if let Some(v) = h {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Escape for embedding inside a JSON string literal (no surrounding quotes).
pub fn json_escape(s: &str) -> String {
    let q = serde_json::to_string(s).unwrap_or_default();
    q.get(1..q.len().saturating_sub(1))
        .unwrap_or("")
        .to_string()
}

/// Fill `{query}` and `{category}` in a URL template (values percent-encoded).
pub fn fill_url(template: &str, query: &str, category: &str) -> String {
    template
        .replace("{query}", &percent_encode(query))
        .replace("{category}", &percent_encode(category))
}

/// Fill `{query}` and `{category}` in a JSON body template (values JSON-escaped).
pub fn fill_json_body(template: &str, query: &str, category: &str) -> String {
    template
        .replace("{query}", &json_escape(query))
        .replace("{category}", &json_escape(category))
}

// ----------------------------------------------------------------- sizes

/// Decode the common HTML entities (`&amp; &lt; &gt; &quot; &apos;` and numeric
/// references). Unknown entities are left untouched.
pub fn decode_html_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let decoded = tail.find(';').filter(|&j| j <= 10).and_then(|j| {
            let name = &tail[1..j];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => name.strip_prefix('#').and_then(|n| {
                    let code = match n.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok()?,
                        None => n.parse().ok()?,
                    };
                    char::from_u32(code)
                }),
            }?;
            Some((c, j + 1))
        });
        match decoded {
            Some((c, n)) => {
                out.push(c);
                rest = &tail[n..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parse a human size such as `1.4 GiB`, `700 MB`, `1,234 KB` or plain bytes.
/// Trailing junk after the unit is ignored.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let mut num = String::new();
    let mut rest = "";
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
        } else if c == ',' {
            // `1,234` thousands separator vs `1,5` decimal comma
            let digits = s[i + 1..].chars().take_while(char::is_ascii_digit).count();
            if digits > 0 && digits != 3 && !num.contains('.') {
                num.push('.');
            }
        } else if c.is_whitespace() && num.is_empty() {
            // leading space
        } else {
            rest = &s[i..];
            break;
        }
    }
    if num.is_empty() {
        return None;
    }
    let value: f64 = num.parse().ok()?;
    let unit: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    let mult: f64 = match unit.as_str() {
        "" | "b" | "bytes" | "byte" => 1.0,
        "k" | "kb" => 1e3,
        "m" | "mb" => 1e6,
        "g" | "gb" => 1e9,
        "t" | "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024f64.powi(3),
        "tib" => 1024f64.powi(4),
        _ => return None,
    };
    Some((value * mult).round() as u64)
}

/// Parse a non-negative integer, tolerating thousands separators and spaces.
pub fn parse_count(s: &str) -> Option<u32> {
    let digits: String = s
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits
        .parse::<u64>()
        .ok()
        .map(|v| v.min(u32::MAX as u64) as u32)
}

// ----------------------------------------------------------------- dates

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn to_unix(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> Option<i64> {
    if !(1..=9999).contains(&y)
        || !(1..=12).contains(&mo)
        || !(1..=31).contains(&d)
        || !(0..=23).contains(&h)
        || !(0..=59).contains(&mi)
        || !(0..=60).contains(&s)
    {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s)
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

fn month_from_name(s: &str) -> Option<i64> {
    let l = s.trim_end_matches('.').to_ascii_lowercase();
    if l.len() < 3 {
        return None;
    }
    MONTHS
        .iter()
        .position(|m| l.starts_with(m))
        .map(|i| i as i64 + 1)
}

fn tz_offset(z: &str) -> Option<i64> {
    let z = z.trim();
    match z {
        "" | "Z" | "z" | "GMT" | "UT" | "UTC" => return Some(0),
        "EST" => return Some(-5 * 3600),
        "EDT" => return Some(-4 * 3600),
        "CST" => return Some(-6 * 3600),
        "CDT" => return Some(-5 * 3600),
        "MST" => return Some(-7 * 3600),
        "MDT" => return Some(-6 * 3600),
        "PST" => return Some(-8 * 3600),
        "PDT" => return Some(-7 * 3600),
        _ => {}
    }
    let sign = match z.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = z[1..].chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 2 {
        return None;
    }
    let h: i64 = digits[..2].parse().ok()?;
    let m: i64 = if digits.len() >= 4 {
        digits[2..4].parse().ok()?
    } else {
        0
    };
    Some(sign * (h * 3600 + m * 60))
}

fn parse_hms(s: &str) -> Option<(i64, i64, i64)> {
    let mut it = s.split(':');
    let h = it.next()?.parse().ok()?;
    let m = it.next()?.parse().ok()?;
    let sec = match it.next() {
        Some(x) => x.split('.').next()?.parse().ok()?,
        None => 0,
    };
    Some((h, m, sec))
}

/// RFC 2822 / RFC 822 date: `Sun, 09 Nov 2026 10:00:00 +0000`.
pub fn parse_rfc2822(s: &str) -> Option<i64> {
    let s = s.trim();
    let s = s.split_once(',').map_or(s, |x| x.1).trim();
    let mut it = s.split_whitespace();
    let d: i64 = it.next()?.parse().ok()?;
    let mo = month_from_name(it.next()?)?;
    let mut y: i64 = it.next()?.parse().ok()?;
    if y < 100 {
        y += if y < 70 { 2000 } else { 1900 };
    }
    let (h, mi, sec) = parse_hms(it.next()?)?;
    let off = tz_offset(it.next().unwrap_or(""))?;
    Some(to_unix(y, mo, d, h, mi, sec)? - off)
}

/// RFC 3339 / ISO 8601: `2026-11-09T10:00:00Z`, `2026-11-09 10:00`, `2026-11-09`.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = match s.find(['T', 't', ' ']) {
        Some(i) => (&s[..i], s[i + 1..].trim()),
        None => (s, ""),
    };
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    if time.is_empty() {
        return to_unix(y, mo, d, 0, 0, 0);
    }
    let split = time.find(['Z', 'z', '+', '-', ' ']).unwrap_or(time.len());
    let (hms, tz) = time.split_at(split);
    let (h, mi, sec) = parse_hms(hms)?;
    let off = tz_offset(tz)?;
    Some(to_unix(y, mo, d, h, mi, sec)? - off)
}

/// Relative date such as `2 hours ago`, `yesterday`, `3 weeks ago`.
pub fn parse_relative(s: &str, now: i64) -> Option<i64> {
    let l = s.trim().to_ascii_lowercase();
    match l.as_str() {
        "now" | "just now" | "today" => return Some(now),
        "yesterday" => return Some(now - 86_400),
        _ => {}
    }
    let l = l.strip_suffix(" ago").unwrap_or(&l);
    let mut it = l.split_whitespace();
    let first = it.next()?;
    let n: i64 = if first == "a" || first == "an" {
        1
    } else {
        first.parse().ok()?
    };
    let unit = it.next()?;
    let secs = match unit.trim_end_matches('s') {
        "sec" | "second" => 1,
        "min" | "minute" => 60,
        "hour" | "hr" | "h" => 3600,
        "day" | "d" => 86_400,
        "week" | "w" => 7 * 86_400,
        "month" | "mon" => 30 * 86_400,
        "year" | "y" => 365 * 86_400,
        _ => return None,
    };
    n.checked_mul(secs).and_then(|d| now.checked_sub(d))
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Parse a date in any supported format: unix seconds/millis, RFC 2822,
/// RFC 3339, `YYYY-MM-DD[ HH:MM[:SS]]`, relative.
pub fn parse_date(s: &str) -> Option<i64> {
    parse_date_at(s, now_unix())
}

/// Like [`parse_date`] with an explicit "now" for relative dates.
pub fn parse_date_at(s: &str, now: i64) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.bytes().all(|b| b.is_ascii_digit()) {
        let v: i64 = s.parse().ok()?;
        return Some(if v >= 100_000_000_000 { v / 1000 } else { v });
    }
    let looks_rfc2822 = s.contains(',')
        || s.split_whitespace()
            .nth(1)
            .is_some_and(|w| month_from_name(w).is_some());
    if looks_rfc2822 && let Some(v) = parse_rfc2822(s) {
        return Some(v);
    }
    parse_rfc3339(s).or_else(|| parse_relative(s, now))
}

/// Parse a date with a strftime-like format. Supported: `%Y %y %m %d %e %H %M %S
/// %b %B %z %%`, plus the special formats `unix`, `rfc2822`, `rfc3339`,
/// `relative` and `auto`.
pub fn parse_date_format(s: &str, fmt: &str) -> Option<i64> {
    match fmt {
        "" | "auto" => return parse_date(s),
        "unix" => return parse_date(s).filter(|_| s.trim().bytes().all(|b| b.is_ascii_digit())),
        "rfc2822" => return parse_rfc2822(s),
        "rfc3339" => return parse_rfc3339(s),
        "relative" => return parse_relative(s, now_unix()),
        _ => {}
    }
    let (mut y, mut mo, mut d, mut h, mut mi, mut sec, mut off) =
        (1970i64, 1i64, 1i64, 0i64, 0i64, 0i64, 0i64);
    let s = s.trim();
    let mut si = 0usize;
    let mut chars = fmt.chars();
    let num = |si: &mut usize, max: usize| -> Option<i64> {
        let digits: String = s[*si..]
            .chars()
            .take(max)
            .take_while(char::is_ascii_digit)
            .collect();
        if digits.is_empty() {
            return None;
        }
        *si += digits.len();
        digits.parse().ok()
    };
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next()? {
                'Y' => y = num(&mut si, 4)?,
                'y' => {
                    let v = num(&mut si, 2)?;
                    y = if v < 70 { 2000 + v } else { 1900 + v };
                }
                'm' => mo = num(&mut si, 2)?,
                'd' | 'e' => {
                    while s[si..].starts_with(' ') {
                        si += 1;
                    }
                    d = num(&mut si, 2)?;
                }
                'H' => h = num(&mut si, 2)?,
                'M' => mi = num(&mut si, 2)?,
                'S' => sec = num(&mut si, 2)?,
                'b' | 'B' => {
                    let name: String = s[si..]
                        .chars()
                        .take_while(|c| c.is_ascii_alphabetic())
                        .collect();
                    mo = month_from_name(&name)?;
                    si += name.len();
                }
                'z' => {
                    let z: String = s[si..].chars().take_while(|c| !c.is_whitespace()).collect();
                    off = tz_offset(&z)?;
                    si += z.len();
                }
                '%' => {
                    if !s[si..].starts_with('%') {
                        return None;
                    }
                    si += 1;
                }
                _ => return None,
            }
        } else {
            let next = s[si..].chars().next()?;
            if next != c {
                return None;
            }
            si += next.len_utf8();
        }
    }
    Some(to_unix(y, mo, d, h, mi, sec)? - off)
}

#[cfg(test)]
mod tests {
    #[test]
    fn html_entities() {
        use super::decode_html_entities as d;
        assert_eq!(
            d("A &amp; B &lt;x&gt; &#39;q&#x27; &quot;"),
            "A & B <x> 'q' \""
        );
        assert_eq!(d("AT&T &bogus; &"), "AT&T &bogus; &");
    }

    use super::*;

    #[test]
    fn hostile_dates_do_not_overflow() {
        assert_eq!(
            parse_rfc2822("Sun, 09 Nov 99999999999999999 10:00:00 +0000"),
            None
        );
        assert_eq!(parse_rfc3339("99999999999999999-01-01"), None);
        assert_eq!(parse_rfc3339("2026-11-09T-1:00:00Z"), None);
        assert_eq!(parse_relative("9223372036854775807 years ago", 1000), None);
        assert!(parse_relative("2 hours ago", 10_000).is_some());
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1.4 GiB"), Some(1_503_238_554));
        assert_eq!(parse_size("700 MB"), Some(700_000_000));
        assert_eq!(parse_size("1,234 KB"), Some(1_234_000));
        assert_eq!(parse_size("1234"), Some(1234));
        assert_eq!(parse_size("  2.5GB "), Some(2_500_000_000));
        assert_eq!(parse_size("1.4 GB3"), Some(1_400_000_000));
        assert_eq!(parse_size("512 KiB"), Some(524_288));
        assert_eq!(parse_size("1,5 GB"), Some(1_500_000_000));
        assert_eq!(parse_size("n/a"), None);
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("5 parsecs"), None);
    }

    #[test]
    fn counts() {
        assert_eq!(parse_count("1,234"), Some(1234));
        assert_eq!(parse_count(" 42 "), Some(42));
        assert_eq!(parse_count("-"), None);
    }

    #[test]
    fn dates() {
        assert_eq!(
            parse_rfc2822("Sun, 09 Nov 2026 10:00:00 +0000"),
            Some(1_794_218_400)
        );
        assert_eq!(
            parse_rfc2822("Sun, 09 Nov 2026 12:00:00 +0200"),
            Some(1_794_218_400)
        );
        assert_eq!(parse_rfc2822("09 Nov 2026 10:00 GMT"), Some(1_794_218_400));
        assert_eq!(parse_rfc3339("2026-11-09T10:00:00Z"), Some(1_794_218_400));
        assert_eq!(
            parse_rfc3339("2026-11-09T11:00:00.123+01:00"),
            Some(1_794_218_400)
        );
        assert_eq!(parse_rfc3339("2026-11-09 10:00"), Some(1_794_218_400));
        assert_eq!(parse_rfc3339("2026-11-09"), Some(1_794_182_400));
        assert_eq!(parse_date("1794218400"), Some(1_794_218_400));
        assert_eq!(parse_date("1794218400000"), Some(1_794_218_400));
        assert_eq!(parse_relative("2 hours ago", 10_000), Some(10_000 - 7200));
        assert_eq!(parse_relative("an hour ago", 10_000), Some(10_000 - 3600));
        assert_eq!(
            parse_relative("3 weeks ago", 10_000_000),
            Some(10_000_000 - 3 * 7 * 86_400)
        );
        assert_eq!(parse_date("nonsense"), None);
        assert_eq!(parse_date_at("5 minutes ago", 1000), Some(700));
        assert_eq!(
            parse_date_format("Nov 09, 2026 10:30", "%b %d, %Y %H:%M"),
            Some(1_794_218_400 + 1800)
        );
        assert_eq!(
            parse_date_format("09/11/26", "%d/%m/%y"),
            Some(1_794_182_400)
        );
        assert_eq!(parse_date_format("x", "%Y"), None);
        // epoch sanity
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2000-03-01T00:00:00Z"), Some(951_868_800));
    }

    #[test]
    fn hashes() {
        let hex = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            normalize_info_hash(&hex.to_uppercase()).as_deref(),
            Some(hex)
        );
        assert_eq!(normalize_info_hash("abc"), None);
        // base32 of bytes 0x00..0x13
        let bytes: Vec<u8> = (0u8..20).collect();
        let b32 = "AAAQEAYEAUDAOCAJBIFQYDIOB4IBCEQT";
        assert_eq!(base32_decode(b32).unwrap(), bytes);
        let expect: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(normalize_info_hash(b32).as_deref(), Some(expect.as_str()));
        assert_eq!(
            normalize_info_hash(&b32.to_lowercase()).as_deref(),
            Some(expect.as_str())
        );
        let magnet = format!("magnet:?xt=urn:btih:{b32}&dn=Foo+Bar&tr=udp%3A%2F%2Fx");
        assert_eq!(
            info_hash_from_magnet(&magnet).as_deref(),
            Some(expect.as_str())
        );
        let magnet = format!("magnet:?dn=x&xt=URN:BTIH:{}", hex.to_uppercase());
        assert_eq!(info_hash_from_magnet(&magnet).as_deref(), Some(hex));
        assert_eq!(info_hash_from_magnet("magnet:?dn=nothing"), None);
        assert_eq!(info_hash_from_magnet("http://x"), None);
    }

    #[test]
    fn coding() {
        assert_eq!(percent_encode("a b&c/é"), "a%20b%26c%2F%C3%A9");
        assert_eq!(percent_decode("a%20b+c%zz%4"), "a b c%zz%4");
        assert_eq!(json_escape("a\"b\\c\n"), "a\\\"b\\\\c\\n");
        assert_eq!(
            fill_url("http://x/{query}?c={category}", "a b", ""),
            "http://x/a%20b?c="
        );
        assert_eq!(
            build_magnet("ab", "My File", &["udp://t:1/announce"]),
            "magnet:?xt=urn:btih:ab&dn=My%20File&tr=udp%3A%2F%2Ft%3A1%2Fannounce"
        );
    }
}
