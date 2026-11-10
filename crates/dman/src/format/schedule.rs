//! Schedule windows, effective limits and the `config reload` summary.

use dman_proto::method::{ScheduleStatus, WindowInfo};

use super::{key_values, limit, table};

/// Limits a window sets, e.g. `down 10.0 MiB/s, max 5`; `-` when it sets none.
fn window_limits(w: &WindowInfo) -> String {
    let mut parts = Vec::new();
    if let Some(d) = w.download_limit {
        parts.push(format!("down {}", limit(d)));
    }
    if let Some(u) = w.upload_limit {
        parts.push(format!("up {}", limit(u)));
    }
    if let Some(m) = w.max_concurrent {
        parts.push(format!("max {m}"));
    }
    if parts.is_empty() {
        "-".into()
    } else {
        parts.join(", ")
    }
}

/// Day list in the shortest readable form.
fn days_text(days: &[String]) -> String {
    let has = |s: &[&str]| s.iter().all(|d| days.iter().any(|x| x == d));
    match days.len() {
        7 => "daily".into(),
        5 if has(&["mon", "tue", "wed", "thu", "fri"]) => "weekdays".into(),
        2 if has(&["sat", "sun"]) => "weekends".into(),
        _ => days.join(","),
    }
}

/// Local `Tue 07:00` and a relative `in 3h12m` for a unix time.
fn when(secs: i64, zone: &str, now: i64) -> String {
    let rel = u64::try_from(secs - now).ok().map(|d| {
        let (h, m) = (d / 3600, d % 3600 / 60);
        if h > 0 {
            format!("in {h}h{m:02}m")
        } else {
            format!("in {m}m")
        }
    });
    let tz = jiff::tz::TimeZone::get(zone).unwrap_or(jiff::tz::TimeZone::UTC);
    let clock = jiff::Timestamp::from_second(secs)
        .map(|t| t.to_zoned(tz).strftime("%a %H:%M").to_string())
        .unwrap_or_else(|_| secs.to_string());
    match rel {
        Some(r) => format!("{clock}, {r}"),
        None => clock,
    }
}

/// `dman schedule` output: summary lines followed by the window table.
pub fn schedule_text(s: &ScheduleStatus, now: i64) -> String {
    if s.windows.is_empty() {
        return "no schedule windows configured; downloads always run\n".into();
    }
    let active = match &s.active {
        None => "none (base limits apply)".to_owned(),
        Some(n) if s.paused => format!("{n} (downloads paused)"),
        Some(n) => n.clone(),
    };
    let mut pairs = vec![("active", active)];
    if let Some(t) = s.next_change {
        let next = s.next_window.as_deref().unwrap_or("none");
        pairs.push((
            "next change",
            format!("{} -> {next}", when(t, &s.time_zone, now)),
        ));
    }
    pairs.push(("time zone", s.time_zone.clone()));
    if s.override_active {
        pairs.push((
            "note",
            "a runtime limit override is active until the next change".into(),
        ));
    }
    let rows: Vec<[String; 5]> = s
        .windows
        .iter()
        .map(|w| {
            [
                w.name.clone(),
                days_text(&w.days),
                format!("{}-{}", w.start, w.end),
                w.action.clone(),
                window_limits(w),
            ]
        })
        .collect();
    format!(
        "{}\n{}",
        key_values(&pairs),
        table(
            ["NAME", "DAYS", "TIME", "ACTION", "LIMITS"],
            &rows,
            &[0, 1, 2, 3]
        )
    )
}

/// `HH:MM` (or `Tue 07:00` when not today) of unix time `at` in `zone`.
fn until_clock(at: i64, zone: &str, now: i64) -> String {
    let tz = jiff::tz::TimeZone::get(zone).unwrap_or(jiff::tz::TimeZone::UTC);
    let conv = |s: i64| jiff::Timestamp::from_second(s).map(|t| t.to_zoned(tz.clone()));
    match (conv(at), conv(now)) {
        (Ok(a), Ok(n)) if a.date() == n.date() => a.strftime("%H:%M").to_string(),
        (Ok(a), _) => a.strftime("%a %H:%M").to_string(),
        _ => at.to_string(),
    }
}

/// Limit lines for `dman limit` / `dman stats`: the effective value of each
/// limit, and when the active window overrides it, which window, until when
/// and what the base limit is, e.g.
/// `10.0 MiB/s (window 'night' until 07:00; base: unlimited)`.
pub fn limit_lines(
    l: &dman_proto::Limits,
    s: Option<&ScheduleStatus>,
    now: i64,
) -> Vec<(&'static str, String)> {
    let mut out = vec![
        ("download", limit(l.download)),
        ("upload", limit(l.upload)),
        ("max-concurrent", l.max_concurrent.to_string()),
    ];
    let Some(s) = s else { return out };
    let Some(active) = s.active.as_deref() else {
        return out;
    };
    let Some(w) = s.windows.iter().find(|w| w.name == active) else {
        return out;
    };
    let until = s
        .next_change
        .map(|t| format!(" until {}", until_clock(t, &s.time_zone, now)))
        .unwrap_or_default();
    let note = |base: Option<String>| {
        let base = base.map(|b| format!("; base: {b}")).unwrap_or_default();
        format!(" (window '{active}'{until}{base})")
    };
    if s.paused {
        out.push((
            "schedule",
            format!("downloads paused by window '{active}'{until}"),
        ));
        return out;
    }
    let b = s.base.as_ref();
    if w.download_limit.is_some() {
        out[0].1 += &note(b.map(|b| limit(b.download)));
    }
    if w.upload_limit.is_some() {
        out[1].1 += &note(b.map(|b| limit(b.upload)));
    }
    if w.max_concurrent.is_some() {
        out[2].1 += &note(b.map(|b| b.max_concurrent.to_string()));
    }
    if w.download_limit.is_none() && w.upload_limit.is_none() && w.max_concurrent.is_none() {
        out.push((
            "schedule",
            format!("window '{active}'{until}: no limit overrides"),
        ));
    }
    out
}

/// `dman config reload` output.
pub fn reload_text(r: &dman_proto::method::ReloadResult) -> String {
    let mut out = if r.applied.is_empty() {
        "configuration reloaded: no live changes\n".to_owned()
    } else {
        format!("configuration reloaded: applied {}\n", r.applied.join(", "))
    };
    for item in &r.restart_required {
        out.push_str(&format!("restart required: {item}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_text_lists_windows_and_state() {
        let st = ScheduleStatus {
            windows: vec![
                WindowInfo {
                    name: "night".into(),
                    days: ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
                        .map(String::from)
                        .to_vec(),
                    start: "22:00".into(),
                    end: "06:00".into(),
                    action: "run".into(),
                    download_limit: Some(10 << 20),
                    upload_limit: None,
                    max_concurrent: Some(5),
                },
                WindowInfo {
                    name: "work".into(),
                    days: ["mon", "tue", "wed", "thu", "fri"]
                        .map(String::from)
                        .to_vec(),
                    start: "09:00".into(),
                    end: "17:00".into(),
                    action: "pause".into(),
                    download_limit: None,
                    upload_limit: None,
                    max_concurrent: None,
                },
            ],
            active: Some("night".into()),
            next_change: Some(3 * 3600 + 120),
            next_window: None,
            time_zone: "UTC".into(),
            paused: false,
            override_active: false,
            base: None,
        };
        let t = schedule_text(&st, 0);
        assert!(t.contains("active:") && t.contains("night"), "{t}");
        assert!(t.contains("Thu 03:02, in 3h02m -> none"), "{t}");
        assert!(t.contains("22:00-06:00") && t.contains("daily"), "{t}");
        assert!(t.contains("weekdays") && t.contains("pause"), "{t}");
        assert!(t.contains("down 10.0 MiB/s, max 5"), "{t}");
        assert!(schedule_text(&ScheduleStatus::default(), 0).contains("no schedule windows"));
    }

    #[test]
    fn reload_text_lists_applied_and_restart() {
        let r = dman_proto::method::ReloadResult {
            applied: vec!["limits".into(), "notify".into()],
            restart_required: vec!["aria2 (path)".into()],
        };
        assert_eq!(
            reload_text(&r),
            "configuration reloaded: applied limits, notify\nrestart required: aria2 (path)\n"
        );
        assert!(reload_text(&Default::default()).contains("no live changes"));
    }

    #[test]
    fn limit_lines_show_window_and_base() {
        let win = |dl: Option<u64>, action: &str| WindowInfo {
            name: "night".into(),
            days: vec![],
            start: "22:00".into(),
            end: "07:00".into(),
            action: action.into(),
            download_limit: dl,
            upload_limit: None,
            max_concurrent: None,
        };
        let mut st = ScheduleStatus {
            windows: vec![win(Some(10 << 20), "run")],
            active: Some("night".into()),
            next_change: Some(7 * 3600),
            time_zone: "UTC".into(),
            base: Some(dman_proto::Limits {
                download: 0,
                upload: 0,
                max_concurrent: 3,
            }),
            ..ScheduleStatus::default()
        };
        let eff = dman_proto::Limits {
            download: 10 << 20,
            upload: 0,
            max_concurrent: 3,
        };
        let l = limit_lines(&eff, Some(&st), 0);
        assert_eq!(
            l[0].1,
            format!(
                "{} (window 'night' until 07:00; base: unlimited)",
                limit(10 << 20)
            )
        );
        assert_eq!(l[1].1, limit(0));
        assert_eq!(limit_lines(&eff, None, 0).len(), 3);
        st.paused = true;
        let l = limit_lines(&eff, Some(&st), 0);
        assert!(l[3].1.contains("paused by window 'night'"), "{l:?}");
    }
}
