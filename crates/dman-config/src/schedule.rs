//! Schedule windows: day-of-week plus time-of-day windows, no
//! cron. A window either lets downloads run with its own limits (`run`) or
//! stops dman from running anything (`pause`).
//!
//! Semantics, as implemented by the daemon:
//!
//! * Times are wall-clock times in the daemon host's time zone.
//! * `end` earlier than `start` means the window crosses midnight; its `days`
//!   name the day it *starts* on (`fri 22:00-06:00` ends on Saturday 06:00).
//!   `end = "24:00"` is the end of the day; `00:00`-`24:00` is the whole day.
//! * `start == end` is rejected (ambiguous: empty or a whole day?).
//! * Windows may overlap; the first matching window in file order wins.
//! * Outside every window `[limits]` applies; with no windows nothing is
//!   scheduled and everything runs always.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

use crate::units::ByteLimit;

/// Day of the week; Monday is 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    /// All days, Monday first.
    pub const ALL: [Weekday; 7] = [
        Self::Mon,
        Self::Tue,
        Self::Wed,
        Self::Thu,
        Self::Fri,
        Self::Sat,
        Self::Sun,
    ];

    /// 0 = Monday ... 6 = Sunday.
    pub fn index(self) -> usize {
        self as usize
    }

    /// Short lower-case name.
    pub fn as_str(self) -> &'static str {
        ["mon", "tue", "wed", "thu", "fri", "sat", "sun"][self.index()]
    }
}

/// One entry of a window's `days` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaySpec {
    /// A single weekday (`"mon"`, also `"monday"`).
    Day(Weekday),
    /// Monday to Friday.
    Weekdays,
    /// Saturday and Sunday.
    Weekends,
    /// Every day.
    Daily,
}

impl DaySpec {
    fn add_to(self, set: &mut [bool; 7]) {
        match self {
            Self::Day(d) => set[d.index()] = true,
            Self::Weekdays => set[..5].fill(true),
            Self::Weekends => set[5..].fill(true),
            Self::Daily => set.fill(true),
        }
    }
}

impl FromStr for DaySpec {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let l = s.trim().to_ascii_lowercase();
        Ok(match l.as_str() {
            "mon" | "monday" => Self::Day(Weekday::Mon),
            "tue" | "tues" | "tuesday" => Self::Day(Weekday::Tue),
            "wed" | "wednesday" => Self::Day(Weekday::Wed),
            "thu" | "thur" | "thurs" | "thursday" => Self::Day(Weekday::Thu),
            "fri" | "friday" => Self::Day(Weekday::Fri),
            "sat" | "saturday" => Self::Day(Weekday::Sat),
            "sun" | "sunday" => Self::Day(Weekday::Sun),
            "weekdays" => Self::Weekdays,
            "weekends" => Self::Weekends,
            "daily" | "everyday" => Self::Daily,
            _ => {
                return Err(format!(
                    "unknown day `{s}` (use mon..sun, weekdays, weekends or daily)"
                ));
            }
        })
    }
}

impl fmt::Display for DaySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Day(d) => d.as_str(),
            Self::Weekdays => "weekdays",
            Self::Weekends => "weekends",
            Self::Daily => "daily",
        })
    }
}

impl Serialize for DaySpec {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DaySpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(de::Error::custom)
    }
}

/// A time of day, `"HH:MM"`, stored as minutes since midnight (`0..=1440`;
/// `"24:00"` is only valid as a window `end`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClockTime(pub u16);

impl ClockTime {
    /// Minutes since midnight.
    pub fn minutes(self) -> u16 {
        self.0
    }
}

impl FromStr for ClockTime {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || format!("invalid time `{s}` (expected HH:MM, 00:00 to 24:00)");
        let (h, m) = s.trim().split_once(':').ok_or_else(bad)?;
        if h.is_empty() || h.len() > 2 || m.len() != 2 {
            return Err(bad());
        }
        let h: u16 = h.parse().map_err(|_| bad())?;
        let m: u16 = m.parse().map_err(|_| bad())?;
        if m > 59 || h > 24 || (h == 24 && m != 0) {
            return Err(bad());
        }
        Ok(Self(h * 60 + m))
    }
}

impl fmt::Display for ClockTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.0 / 60, self.0 % 60)
    }
}

impl Serialize for ClockTime {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ClockTime {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(de::Error::custom)
    }
}

/// What a window does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleAction {
    /// Downloads run; the window's limits (if any) override `[limits]`.
    #[default]
    Run,
    /// dman starts nothing and pauses what it started; user-paused jobs are untouched.
    Pause,
}

impl ScheduleAction {
    /// Lower-case name as used in the config file.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Pause => "pause",
        }
    }
}

fn daily() -> Vec<DaySpec> {
    vec![DaySpec::Daily]
}

/// One `[[schedule]]` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleWindow {
    /// Unique, non-empty name shown in messages and the CLI.
    pub name: String,
    /// Days the window starts on; default `["daily"]`.
    #[serde(default = "daily")]
    pub days: Vec<DaySpec>,
    /// Start time, inclusive.
    pub start: ClockTime,
    /// End time, exclusive; earlier than `start` = crosses midnight.
    pub end: ClockTime,
    /// `run` (default) or `pause`.
    #[serde(default)]
    pub action: ScheduleAction,
    /// Download cap inside the window (`run` only); unset = keep `[limits]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_limit: Option<ByteLimit>,
    /// Upload cap inside the window (`run` only); unset = keep `[limits]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_limit: Option<ByteLimit>,
    /// Concurrent downloads inside the window (`run` only); unset = keep `[limits]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<u32>,
}

impl ScheduleWindow {
    /// Days (Monday = index 0) the window starts on.
    pub fn day_set(&self) -> [bool; 7] {
        let mut set = [false; 7];
        for d in &self.days {
            d.add_to(&mut set);
        }
        set
    }

    /// The window ends on the day after it starts.
    pub fn crosses_midnight(&self) -> bool {
        self.end <= self.start
    }

    /// Check this window on its own.
    pub fn validate(&self) -> Result<(), String> {
        let n = &self.name;
        if self.name.trim().is_empty() {
            return Err("schedule window name must not be empty".into());
        }
        if self.days.is_empty() {
            return Err(format!("schedule `{n}`: days must not be empty"));
        }
        if self.start.0 >= 24 * 60 {
            return Err(format!("schedule `{n}`: start must be before 24:00"));
        }
        if self.start == self.end {
            return Err(format!(
                "schedule `{n}`: start and end are equal; use 00:00 to 24:00 for a whole day"
            ));
        }
        if self.action == ScheduleAction::Pause
            && (self.download_limit.is_some()
                || self.upload_limit.is_some()
                || self.max_concurrent.is_some())
        {
            return Err(format!("schedule `{n}`: a pause window cannot set limits"));
        }
        if self.max_concurrent == Some(0) {
            return Err(format!("schedule `{n}`: max_concurrent must be at least 1"));
        }
        Ok(())
    }
}

/// Validate all windows: each one, plus unique names.
pub fn validate_windows(windows: &[ScheduleWindow]) -> Result<(), String> {
    for (i, w) in windows.iter().enumerate() {
        w.validate()?;
        if windows[..i].iter().any(|o| o.name == w.name) {
            return Err(format!("schedule name `{}` is used twice", w.name));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(toml: &str) -> Result<ScheduleWindow, String> {
        let w: ScheduleWindow = toml::from_str(toml).map_err(|e| e.to_string())?;
        w.validate()?;
        Ok(w)
    }

    #[test]
    fn times_parse_and_format() {
        assert_eq!("07:05".parse::<ClockTime>(), Ok(ClockTime(425)));
        assert_eq!("7:05".parse::<ClockTime>(), Ok(ClockTime(425)));
        assert_eq!("24:00".parse::<ClockTime>(), Ok(ClockTime(1440)));
        assert_eq!(ClockTime(425).to_string(), "07:05");
        for bad in ["", "7", "24:01", "25:00", "12:60", "12:5", "ab:cd", "-1:00"] {
            assert!(bad.parse::<ClockTime>().is_err(), "{bad}");
        }
    }

    #[test]
    fn day_sets() {
        let w = win("name='a'\ndays=['weekdays','sun']\nstart='01:00'\nend='02:00'").unwrap();
        assert_eq!(w.day_set(), [true, true, true, true, true, false, true]);
        let w = win("name='a'\nstart='01:00'\nend='02:00'").unwrap();
        assert_eq!(w.day_set(), [true; 7]);
        let w = win("name='a'\ndays=['Weekends']\nstart='01:00'\nend='02:00'").unwrap();
        assert_eq!(w.day_set(), [false, false, false, false, false, true, true]);
        assert!(win("name='a'\ndays=['funday']\nstart='01:00'\nend='02:00'").is_err());
    }

    #[test]
    fn midnight_and_validation() {
        let w = win("name='n'\nstart='22:00'\nend='06:00'").unwrap();
        assert!(w.crosses_midnight());
        let w = win("name='n'\nstart='00:00'\nend='24:00'").unwrap();
        assert!(!w.crosses_midnight());
        assert!(win("name='n'\nstart='10:00'\nend='10:00'").is_err());
        assert!(win("name=' '\nstart='10:00'\nend='11:00'").is_err());
        assert!(win("name='n'\ndays=[]\nstart='10:00'\nend='11:00'").is_err());
        assert!(win("name='n'\nstart='24:00'\nend='11:00'").is_err());
        assert!(
            win("name='n'\naction='pause'\ndownload_limit='1MiB'\nstart='10:00'\nend='11:00'")
                .is_err()
        );
        assert!(win("name='n'\nmax_concurrent=0\nstart='10:00'\nend='11:00'").is_err());
        assert!(win("name='n'\nstart='10:00'\nend='11:00'\nbogus=1").is_err());
        assert!(win("name='n'\naction='stop'\nstart='10:00'\nend='11:00'").is_err());
    }

    #[test]
    fn duplicate_names_rejected() {
        let a = win("name='n'\nstart='10:00'\nend='11:00'").unwrap();
        assert!(validate_windows(&[a.clone(), a]).is_err());
    }
}
