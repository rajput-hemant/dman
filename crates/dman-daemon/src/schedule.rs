//! Schedule windows (decision D11) as pure functions over `jiff` zoned times,
//! plus the [`Clock`] the manager reads time from (injectable for tests).
//!
//! Matching works on the local wall clock: a window is active when the local
//! date-time lies in `[start, end)` of a day listed in its `days` (a window
//! that crosses midnight belongs to the day it starts on). Across a DST change
//! the wall clock repeats or skips an hour, so [`next_transition`] also looks at
//! the zone's own transitions; the answer is always "the first instant after
//! `now` at which the active window differs".

use std::sync::Arc;

use dman_config::{ScheduleAction, ScheduleWindow};
use dman_proto::Limits;
use jiff::civil::{DateTime, Time};
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

/// How far ahead [`next_transition`] looks (a week repeats; one day of slack).
const HORIZON_DAYS: i64 = 9;

/// Source of the current local time.
pub trait Clock: Send + Sync + 'static {
    /// Now, in the zone the windows are interpreted in.
    fn now(&self) -> Zoned;
}

/// The system clock in a fixed time zone.
#[derive(Debug, Clone)]
pub struct SystemClock {
    tz: TimeZone,
}

impl SystemClock {
    /// The system time zone; falls back to UTC (with a warning) if it cannot
    /// be determined.
    pub fn local() -> Self {
        match TimeZone::try_system() {
            Ok(tz) => Self { tz },
            Err(e) => {
                tracing::warn!("cannot determine the system time zone ({e}); schedules use UTC");
                Self { tz: TimeZone::UTC }
            }
        }
    }

    /// A specific zone.
    pub fn with_zone(tz: TimeZone) -> Self {
        Self { tz }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Zoned {
        Timestamp::now().to_zoned(self.tz.clone())
    }
}

/// Convenience for `Arc<dyn Clock>`.
pub fn system_clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock::local())
}

/// Name of a zone for display (`UTC` when it has no IANA name).
pub fn zone_name(tz: &TimeZone) -> String {
    tz.iana_name().unwrap_or("UTC").to_owned()
}

fn seconds_of_day(t: Time) -> u32 {
    t.hour() as u32 * 3600 + t.minute() as u32 * 60 + t.second() as u32
}

/// Is `w` active at the local wall-clock time `dt`?
pub fn window_active(w: &ScheduleWindow, dt: DateTime) -> bool {
    let days = w.day_set();
    let today = dt.date().weekday().to_monday_zero_offset() as usize;
    let yesterday = (today + 6) % 7;
    let now = seconds_of_day(dt.time());
    let start = u32::from(w.start.minutes()) * 60;
    let end = u32::from(w.end.minutes()) * 60;
    if w.crosses_midnight() {
        (days[today] && now >= start) || (days[yesterday] && now < end)
    } else {
        days[today] && (start..end).contains(&now)
    }
}

/// Index of the first window active at `dt` (first match wins).
pub fn active_index(windows: &[ScheduleWindow], dt: DateTime) -> Option<usize> {
    windows.iter().position(|w| window_active(w, dt))
}

/// Index of the window active at `now`.
pub fn current(windows: &[ScheduleWindow], now: &Zoned) -> Option<usize> {
    active_index(windows, now.datetime())
}

/// A change of the active window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    /// When the change happens.
    pub at: Timestamp,
    /// The window active from then on (`None` = outside all windows).
    pub window: Option<usize>,
}

/// The first change of the active window after `now`, within the next days;
/// `None` when it never changes (no windows, or one window covering every
/// instant).
pub fn next_transition(windows: &[ScheduleWindow], now: &Zoned) -> Option<Transition> {
    if windows.is_empty() {
        return None;
    }
    let tz = now.time_zone().clone();
    let now_ts = now.timestamp();
    let horizon = now_ts
        .checked_add(jiff::SignedDuration::from_hours(HORIZON_DAYS * 24 + 24))
        .ok();
    let mut candidates: Vec<Timestamp> = Vec::new();
    let mut push = |dt: DateTime| {
        // A repeated wall-clock time (DST fold) occurs twice: both count.
        let amb = tz.to_ambiguous_zoned(dt);
        let all = [amb.clone().compatible(), amb.clone().earlier(), amb.later()];
        for z in all.into_iter().flatten() {
            if z.timestamp() > now_ts {
                candidates.push(z.timestamp());
            }
        }
    };
    let today = now.date();
    for offset in -1..=HORIZON_DAYS {
        let Ok(day) = today.checked_add(jiff::Span::new().days(offset)) else {
            continue;
        };
        let weekday = day.weekday().to_monday_zero_offset() as usize;
        for w in windows {
            if !w.day_set()[weekday] {
                continue;
            }
            let minutes = |m: u16| Time::new((m / 60) as i8 % 24, (m % 60) as i8, 0, 0);
            let (Ok(start), Ok(end)) = (minutes(w.start.minutes()), minutes(w.end.minutes()))
            else {
                continue;
            };
            push(day.to_datetime(start));
            // The end lands on the next day when crossing midnight or at 24:00.
            let end_day = if w.crosses_midnight() || w.end.minutes() >= 24 * 60 {
                day.checked_add(jiff::Span::new().days(1))
            } else {
                Ok(day)
            };
            if let Ok(d) = end_day {
                push(d.to_datetime(end));
            }
        }
    }
    // The wall clock repeats or skips an hour at DST changes.
    for t in tz.following(now_ts).take(4) {
        if horizon.is_none_or(|h| t.timestamp() <= h) {
            candidates.push(t.timestamp());
        }
    }
    candidates.sort();
    candidates.dedup();
    let cur = current(windows, now);
    candidates.into_iter().find_map(|ts| {
        let idx = active_index(windows, ts.to_zoned(tz.clone()).datetime());
        (idx != cur).then_some(Transition {
            at: ts,
            window: idx,
        })
    })
}

/// Limits in effect: `base`, overridden by the fields of a `run` window.
pub fn apply_window(base: &Limits, window: Option<&ScheduleWindow>) -> Limits {
    let mut l = base.clone();
    if let Some(w) = window.filter(|w| w.action == ScheduleAction::Run) {
        if let Some(d) = w.download_limit {
            l.download = d.0;
        }
        if let Some(u) = w.upload_limit {
            l.upload = u.0;
        }
        if let Some(m) = w.max_concurrent {
            l.max_concurrent = m;
        }
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn windows(toml: &str) -> Vec<ScheduleWindow> {
        #[derive(serde::Deserialize)]
        struct W {
            schedule: Vec<ScheduleWindow>,
        }
        let w: W = toml::from_str(toml).unwrap();
        dman_config::validate_windows(&w.schedule).unwrap();
        w.schedule
    }

    fn night_work() -> Vec<ScheduleWindow> {
        windows(
            r#"
[[schedule]]
name = "night"
start = "22:00"
end = "06:00"
download_limit = "10MiB"

[[schedule]]
name = "work"
days = ["weekdays"]
start = "09:00"
end = "17:00"
action = "pause"
"#,
        )
    }

    fn tz(name: &str) -> Option<TimeZone> {
        match TimeZone::get(name) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("time zone database unavailable ({e}); skipping");
                None
            }
        }
    }

    fn at(tz: &TimeZone, dt: DateTime) -> Zoned {
        tz.to_ambiguous_zoned(dt).compatible().unwrap()
    }

    fn names(ws: &[ScheduleWindow], i: Option<usize>) -> Option<&str> {
        i.map(|i| ws[i].name.as_str())
    }

    #[test]
    fn plain_and_midnight_crossing_windows() {
        let ws = night_work();
        // 2027-03-24 is a Wednesday.
        let wed = date(2027, 3, 24);
        let m = |h, mi| names(&ws, active_index(&ws, wed.at(h, mi, 0, 0)));
        assert_eq!(m(8, 59), None);
        assert_eq!(m(9, 0), Some("work"));
        assert_eq!(m(16, 59), Some("work"));
        assert_eq!(m(17, 0), None);
        assert_eq!(m(21, 59), None);
        assert_eq!(m(22, 0), Some("night"));
        assert_eq!(m(23, 59), Some("night"));
        // After midnight the window of the previous day still applies.
        let thu = date(2027, 3, 25);
        assert_eq!(
            names(&ws, active_index(&ws, thu.at(0, 0, 0, 0))),
            Some("night")
        );
        assert_eq!(
            names(&ws, active_index(&ws, thu.at(5, 59, 59, 0))),
            Some("night")
        );
        assert_eq!(names(&ws, active_index(&ws, thu.at(6, 0, 0, 0))), None);
    }

    #[test]
    fn crossing_window_belongs_to_its_start_day() {
        let ws = windows("[[schedule]]\nname='fri'\ndays=['fri']\nstart='22:00'\nend='06:00'\n");
        // 2027-03-26 is a Friday.
        let fri = date(2027, 3, 26);
        let sat = date(2027, 3, 27);
        let thu = date(2027, 3, 25);
        assert!(active_index(&ws, fri.at(23, 0, 0, 0)).is_some());
        assert!(active_index(&ws, sat.at(2, 0, 0, 0)).is_some());
        assert!(active_index(&ws, sat.at(23, 0, 0, 0)).is_none());
        assert!(active_index(&ws, fri.at(2, 0, 0, 0)).is_none());
        assert!(active_index(&ws, thu.at(23, 0, 0, 0)).is_none());
    }

    #[test]
    fn weekday_sets_weekends_and_whole_day() {
        let ws =
            windows("[[schedule]]\nname='we'\ndays=['weekends']\nstart='00:00'\nend='24:00'\n");
        let dt = |d: i8| date(2027, 3, d).at(12, 0, 0, 0);
        // 26 = Fri, 27 = Sat, 28 = Sun, 29 = Mon.
        assert!(active_index(&ws, dt(26)).is_none());
        assert!(active_index(&ws, dt(27)).is_some());
        assert!(active_index(&ws, dt(28)).is_some());
        assert!(active_index(&ws, dt(29)).is_none());
        // 24:00 is exclusive: Monday 00:00 is outside.
        assert!(active_index(&ws, date(2027, 3, 29).at(0, 0, 0, 0)).is_none());
        assert!(active_index(&ws, date(2027, 3, 28).at(23, 59, 59, 0)).is_some());
    }

    #[test]
    fn first_match_wins_on_overlap() {
        let ws = windows(
            "[[schedule]]\nname='a'\nstart='10:00'\nend='12:00'\n[[schedule]]\nname='b'\nstart='11:00'\nend='13:00'\n",
        );
        let d = date(2027, 3, 24);
        assert_eq!(names(&ws, active_index(&ws, d.at(11, 30, 0, 0))), Some("a"));
        assert_eq!(names(&ws, active_index(&ws, d.at(12, 30, 0, 0))), Some("b"));
    }

    #[test]
    fn next_transition_simple_and_across_midnight() {
        let ws = night_work();
        let utc = TimeZone::UTC;
        // Wed 12:00 inside "work": ends 17:00 -> outside.
        let now = at(&utc, date(2027, 3, 24).at(12, 0, 0, 0));
        let t = next_transition(&ws, &now).unwrap();
        assert_eq!(
            t.at,
            at(&utc, date(2027, 3, 24).at(17, 0, 0, 0)).timestamp()
        );
        assert_eq!(t.window, None);
        // Wed 23:00 inside "night": ends Thu 06:00.
        let now = at(&utc, date(2027, 3, 24).at(23, 0, 0, 0));
        let t = next_transition(&ws, &now).unwrap();
        assert_eq!(t.at, at(&utc, date(2027, 3, 25).at(6, 0, 0, 0)).timestamp());
        assert_eq!(t.window, None);
        // Thu 07:00 outside: starts work at 09:00.
        let now = at(&utc, date(2027, 3, 25).at(7, 0, 0, 0));
        let t = next_transition(&ws, &now).unwrap();
        assert_eq!(names(&ws, t.window), Some("work"));
        assert_eq!(t.at, at(&utc, date(2027, 3, 25).at(9, 0, 0, 0)).timestamp());
        // Fri 17:00 -> night at 22:00 (work ended exactly now: 17:00 is outside).
        let now = at(&utc, date(2027, 3, 26).at(18, 0, 0, 0));
        let t = next_transition(&ws, &now).unwrap();
        assert_eq!(names(&ws, t.window), Some("night"));
    }

    #[test]
    fn no_change_means_no_transition() {
        assert!(next_transition(&[], &Timestamp::now().to_zoned(TimeZone::UTC)).is_none());
        let ws = windows("[[schedule]]\nname='all'\nstart='00:00'\nend='24:00'\n");
        let now = at(&TimeZone::UTC, date(2027, 3, 24).at(12, 0, 0, 0));
        assert!(next_transition(&ws, &now).is_none());
    }

    #[test]
    fn adjacent_windows_report_the_switch_not_a_gap() {
        let ws = windows(
            "[[schedule]]\nname='a'\nstart='10:00'\nend='12:00'\n[[schedule]]\nname='b'\nstart='12:00'\nend='14:00'\n",
        );
        let now = at(&TimeZone::UTC, date(2027, 3, 24).at(10, 30, 0, 0));
        let t = next_transition(&ws, &now).unwrap();
        assert_eq!(names(&ws, t.window), Some("b"));
        assert_eq!(
            t.at,
            at(&TimeZone::UTC, date(2027, 3, 24).at(12, 0, 0, 0)).timestamp()
        );
    }

    #[test]
    fn dst_spring_forward_in_berlin() {
        let Some(berlin) = tz("Europe/Berlin") else {
            return;
        };
        let ws = windows("[[schedule]]\nname='night'\nstart='22:00'\nend='06:00'\n");
        // 2027-03-28 02:00 CET -> 03:00 CEST. Saturday night into Sunday.
        let now = at(&berlin, date(2027, 3, 27).at(23, 0, 0, 0));
        assert_eq!(current(&ws, &now), Some(0));
        let t = next_transition(&ws, &now).unwrap();
        // 23:00 CET = 22:00Z; 06:00 CEST = 04:00Z: the night is an hour shorter.
        assert_eq!(t.at, "2027-03-28T04:00:00Z".parse::<Timestamp>().unwrap());
        assert_eq!(t.window, None);
        // The instant of the jump is still inside the window.
        let jump = at(&berlin, date(2027, 3, 28).at(3, 0, 0, 0));
        assert_eq!(
            jump.timestamp(),
            "2027-03-28T01:00:00Z".parse::<Timestamp>().unwrap()
        );
        assert_eq!(current(&ws, &jump), Some(0));
        // Evening of the same day: start is 22:00 CEST = 20:00 UTC.
        let now = at(&berlin, date(2027, 3, 28).at(12, 0, 0, 0));
        let t = next_transition(&ws, &now).unwrap();
        assert_eq!(t.at, "2027-03-28T20:00:00Z".parse::<Timestamp>().unwrap());
        assert_eq!(t.window, Some(0));
    }

    #[test]
    fn dst_fall_back_in_berlin() {
        let Some(berlin) = tz("Europe/Berlin") else {
            return;
        };
        let ws = windows("[[schedule]]\nname='night'\nstart='22:00'\nend='06:00'\n");
        // 2026-10-25 03:00 CEST -> 02:00 CET: the night is an hour longer.
        let now = at(&berlin, date(2026, 10, 24).at(23, 0, 0, 0));
        let t = next_transition(&ws, &now).unwrap();
        assert_eq!(t.at, "2026-10-25T05:00:00Z".parse::<Timestamp>().unwrap());
        assert_eq!(t.window, None);

        // A window ending inside the repeated hour: 01:00-02:30. Wall-clock
        // matching makes it active again for 02:00-02:30 CET; the transitions
        // must be reported in real-time order.
        let ws = windows("[[schedule]]\nname='w'\nstart='01:00'\nend='02:30'\n");
        let now = at(&berlin, date(2026, 10, 25).at(0, 30, 0, 0));
        assert_eq!(current(&ws, &now), None);
        let t1 = next_transition(&ws, &now).unwrap();
        assert_eq!(t1.at, "2026-10-24T23:00:00Z".parse::<Timestamp>().unwrap());
        assert_eq!(t1.window, Some(0));
        let now = t1.at.to_zoned(berlin.clone());
        let t2 = next_transition(&ws, &now).unwrap();
        // 02:30 CEST = 00:30Z: the window ends ...
        assert_eq!(t2.at, "2026-10-25T00:30:00Z".parse::<Timestamp>().unwrap());
        assert_eq!(t2.window, None);
        // ... and at the jump back (01:00Z = 02:00 CET) it is active again.
        let t3 = next_transition(&ws, &t2.at.to_zoned(berlin.clone())).unwrap();
        assert_eq!(t3.at, "2026-10-25T01:00:00Z".parse::<Timestamp>().unwrap());
        assert_eq!(t3.window, Some(0));
        let t4 = next_transition(&ws, &t3.at.to_zoned(berlin)).unwrap();
        assert_eq!(t4.at, "2026-10-25T01:30:00Z".parse::<Timestamp>().unwrap());
        assert_eq!(t4.window, None);
    }

    #[test]
    fn window_start_inside_the_skipped_hour() {
        let Some(berlin) = tz("Europe/Berlin") else {
            return;
        };
        // 02:30 does not exist on 2027-03-28; the window starts when the gap ends.
        let ws = windows("[[schedule]]\nname='w'\nstart='02:30'\nend='04:00'\n");
        let now = at(&berlin, date(2027, 3, 28).at(1, 0, 0, 0));
        let t = next_transition(&ws, &now).unwrap();
        assert_eq!(t.window, Some(0));
        // Gap 02:00-03:00; wall clock 03:00 is already >= 02:30.
        assert_eq!(t.at, "2027-03-28T01:00:00Z".parse::<Timestamp>().unwrap());
    }

    #[test]
    fn effective_limits_overlay() {
        let ws = night_work();
        let base = Limits {
            download: 0,
            upload: 5,
            max_concurrent: 3,
        };
        let night = apply_window(&base, Some(&ws[0]));
        assert_eq!(
            (night.download, night.upload, night.max_concurrent),
            (10 << 20, 5, 3)
        );
        assert_eq!(apply_window(&base, Some(&ws[1])), base);
        assert_eq!(apply_window(&base, None), base);
    }
}
