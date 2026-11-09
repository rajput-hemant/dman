//! Byte-size and disk-threshold value types.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Parse a byte size such as `"5GiB"`, `"500MB"` or `"1073741824"`.
pub fn parse_bytes(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty size".into());
    }
    s.parse::<bytesize::ByteSize>()
        .map(|b| b.as_u64())
        .map_err(|e| format!("invalid size `{s}`: {e}"))
}

/// Format bytes with the largest binary unit that divides the value exactly,
/// otherwise as a plain integer.
pub fn format_bytes(n: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("TiB", 1 << 40),
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
    ];
    if n == 0 {
        return "0".into();
    }
    for (name, size) in UNITS {
        if n.is_multiple_of(size) {
            return format!("{}{name}", n / size);
        }
    }
    n.to_string()
}

struct StrOrInt;

impl Visitor<'_> for StrOrInt {
    type Value = String;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a size string like \"10MiB\" or a non-negative integer")
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
        Ok(v.to_owned())
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<String, E> {
        Ok(v.to_string())
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<String, E> {
        u64::try_from(v)
            .map(|v| v.to_string())
            .map_err(|_| E::custom("size must not be negative"))
    }
}

/// A byte rate limit; `0` means unlimited. Accepts a string or integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ByteLimit(pub u64);

impl ByteLimit {
    /// True when no limit applies.
    pub fn is_unlimited(self) -> bool {
        self.0 == 0
    }
}

impl Serialize for ByteLimit {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format_bytes(self.0))
    }
}

impl<'de> Deserialize<'de> for ByteLimit {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = d.deserialize_any(StrOrInt)?;
        parse_bytes(&s).map(ByteLimit).map_err(de::Error::custom)
    }
}

/// Free-disk-space threshold: absolute bytes (`"5GiB"`) or percent (`"5%"`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DiskThreshold {
    /// Absolute number of bytes.
    Absolute(u64),
    /// Percentage of total capacity, in `[0, 100]`.
    Percent(f64),
}

impl DiskThreshold {
    /// True when `free` bytes out of `total` is below this threshold.
    pub fn violated(&self, free: u64, total: u64) -> bool {
        match *self {
            Self::Absolute(n) => free < n,
            Self::Percent(p) => (free as f64) < (total as f64) * p / 100.0,
        }
    }
}

impl FromStr for DiskThreshold {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if let Some(p) = s.strip_suffix('%') {
            let v: f64 = p
                .trim()
                .parse()
                .map_err(|_| format!("invalid percentage `{s}`"))?;
            if !v.is_finite() || !(0.0..=100.0).contains(&v) {
                return Err(format!("percentage `{s}` must be between 0% and 100%"));
            }
            Ok(Self::Percent(v))
        } else {
            parse_bytes(s).map(Self::Absolute)
        }
    }
}

impl fmt::Display for DiskThreshold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absolute(n) => f.write_str(&format_bytes(*n)),
            Self::Percent(p) => write!(f, "{p}%"),
        }
    }
}

impl Serialize for DiskThreshold {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DiskThreshold {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(StrOrInt)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_absolute_and_percent() {
        assert_eq!(
            "5GiB".parse::<DiskThreshold>(),
            Ok(DiskThreshold::Absolute(5 << 30))
        );
        assert_eq!(
            "500MB".parse::<DiskThreshold>(),
            Ok(DiskThreshold::Absolute(500_000_000))
        );
        assert_eq!(
            "1073741824".parse::<DiskThreshold>(),
            Ok(DiskThreshold::Absolute(1 << 30))
        );
        assert_eq!(
            "5%".parse::<DiskThreshold>(),
            Ok(DiskThreshold::Percent(5.0))
        );
        assert_eq!(
            "2.5%".parse::<DiskThreshold>(),
            Ok(DiskThreshold::Percent(2.5))
        );
    }

    #[test]
    fn rejects_invalid() {
        for bad in ["", "abc", "-5%", "101%", "%", "5 parsecs", "nan%"] {
            assert!(bad.parse::<DiskThreshold>().is_err(), "{bad}");
        }
    }

    #[test]
    fn violation_logic() {
        let abs = DiskThreshold::Absolute(100);
        assert!(abs.violated(99, 1000));
        assert!(!abs.violated(100, 1000));
        let pct = DiskThreshold::Percent(10.0);
        assert!(pct.violated(99, 1000));
        assert!(!pct.violated(100, 1000));
        assert!(!DiskThreshold::Absolute(0).violated(0, 0));
    }

    #[test]
    fn display_round_trips() {
        for s in ["5GiB", "1GiB", "5%", "2.5%", "1500", "0"] {
            let t: DiskThreshold = s.parse().unwrap();
            assert_eq!(t.to_string(), s);
        }
        let t: DiskThreshold = "500MB".parse().unwrap();
        assert_eq!(t.to_string().parse::<DiskThreshold>().unwrap(), t);
    }

    #[test]
    fn byte_sizes() {
        assert_eq!(parse_bytes("10MiB"), Ok(10 << 20));
        assert_eq!(parse_bytes("0"), Ok(0));
        assert!(parse_bytes("ten").is_err());
        assert_eq!(format_bytes(10 << 20), "10MiB");
    }

    #[test]
    fn limit_serde_accepts_int_and_string() {
        #[derive(Deserialize, Serialize)]
        struct T {
            a: ByteLimit,
            b: ByteLimit,
        }
        let t: T = toml::from_str("a = \"10MiB\"\nb = 0").unwrap();
        assert_eq!(t.a, ByteLimit(10 << 20));
        assert!(t.b.is_unlimited());
        assert!(toml::from_str::<T>("a = -1\nb = 0").is_err());
        let out = toml::to_string(&t).unwrap();
        assert!(out.contains("a = \"10MiB\""));
    }
}
