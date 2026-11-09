//! The schedule in the manager: evaluates the windows every tick, applies the
//! limits of the active window, and persists base limit changes to the config.

use std::sync::Arc;

use dman_aria2::global_limits;
use dman_config::{ConfigDoc, ConfigError, Paths, ScheduleAction, ScheduleWindow, format_bytes};
use dman_proto::method::{Event, Level, LimitsUpdate, ScheduleStatus, WindowInfo};
use dman_proto::{Limits, RpcError};
use jiff::Timestamp;

use crate::schedule::{self, Clock, Transition};

use super::Manager;
use super::util::engine_err;

/// Schedule configuration and state of the manager.
pub struct Timetable {
    windows: Vec<ScheduleWindow>,
    clock: Arc<dyn Clock>,
    /// Where `[limits]` is persisted; `None` disables persistence.
    config: Option<Paths>,
    /// Limits outside any window (the `[limits]` section).
    base: Limits,
    /// Runtime overrides of fields the active window sets itself; dropped at
    /// the next window transition.
    temp: LimitsUpdate,
    active: Option<usize>,
    initialised: bool,
    next: Option<Transition>,
    computed_at: Option<Timestamp>,
    /// Global limits last confirmed applied to aria2.
    applied: Option<(u64, u64)>,
    tz_name: String,
}

impl Timetable {
    /// `base` are the configured `[limits]`.
    pub fn new(
        windows: Vec<ScheduleWindow>,
        clock: Arc<dyn Clock>,
        config: Option<Paths>,
        base: Limits,
    ) -> Self {
        let tz_name = schedule::zone_name(clock.now().time_zone());
        Self {
            windows,
            clock,
            config,
            base,
            temp: LimitsUpdate::default(),
            active: None,
            initialised: false,
            next: None,
            computed_at: None,
            applied: None,
            tz_name,
        }
    }

    fn window(&self) -> Option<&ScheduleWindow> {
        self.active.and_then(|i| self.windows.get(i))
    }

    /// A pause window is active: nothing may run.
    pub(crate) fn paused(&self) -> bool {
        self.window()
            .is_some_and(|w| w.action == ScheduleAction::Pause)
    }

    /// Limits in effect: base, then the window's, then temporary overrides.
    fn effective(&self) -> Limits {
        let mut l = schedule::apply_window(&self.base, self.window());
        if let Some(d) = self.temp.download {
            l.download = d;
        }
        if let Some(u) = self.temp.upload {
            l.upload = u;
        }
        if let Some(m) = self.temp.max_concurrent {
            l.max_concurrent = m;
        }
        l
    }

    fn override_active(&self) -> bool {
        self.temp != LimitsUpdate::default()
    }

    pub(crate) fn status(&self) -> ScheduleStatus {
        let name = |i: Option<usize>| i.and_then(|i| self.windows.get(i)).map(|w| w.name.clone());
        ScheduleStatus {
            windows: self.windows.iter().map(window_info).collect(),
            active: name(self.active),
            next_change: self.next.map(|t| t.at.as_second()),
            next_window: self.next.and_then(|t| name(t.window)),
            time_zone: self.tz_name.clone(),
            paused: self.paused(),
            override_active: self.override_active(),
        }
    }

    /// `HH:MM` of the next change in the schedule's zone.
    fn next_clock(&self) -> Option<String> {
        let t = self.next?;
        let z = t.at.to_zoned(self.clock.now().time_zone().clone());
        Some(z.strftime("%H:%M").to_string())
    }
}

fn window_info(w: &ScheduleWindow) -> WindowInfo {
    let set = w.day_set();
    WindowInfo {
        name: w.name.clone(),
        days: dman_config::Weekday::ALL
            .iter()
            .filter(|d| set[d.index()])
            .map(|d| d.as_str().to_owned())
            .collect(),
        start: w.start.to_string(),
        end: w.end.to_string(),
        action: w.action.as_str().to_owned(),
        download_limit: w.download_limit.map(|l| l.0),
        upload_limit: w.upload_limit.map(|l| l.0),
        max_concurrent: w.max_concurrent,
    }
}

fn rate(n: u64) -> String {
    if n == 0 {
        "unlimited".into()
    } else {
        format!("{}/s", format_bytes(n))
    }
}

/// "download 10MiB/s, max 5 concurrent" for the fields `w` overrides.
fn describe_overrides(w: &ScheduleWindow) -> String {
    let mut parts = Vec::new();
    if let Some(d) = w.download_limit {
        parts.push(format!("download {}", rate(d.0)));
    }
    if let Some(u) = w.upload_limit {
        parts.push(format!("upload {}", rate(u.0)));
    }
    if let Some(m) = w.max_concurrent {
        parts.push(format!("max {m} concurrent"));
    }
    parts.join(", ")
}

fn describe_limits(l: &Limits) -> String {
    format!(
        "download {}, upload {}, max {} concurrent",
        rate(l.download),
        rate(l.upload),
        l.max_concurrent
    )
}

/// Writes the given base limits to `config.toml`, keeping comments.
fn persist(
    paths: &Paths,
    download: Option<u64>,
    upload: Option<u64>,
    max_concurrent: Option<u32>,
) -> Result<(), ConfigError> {
    let file = paths.config_file();
    if let Ok(meta) = std::fs::metadata(&file)
        && meta.permissions().readonly()
    {
        return Err(ConfigError::Invalid(format!(
            "{} is read-only",
            file.display()
        )));
    }
    let mut doc = ConfigDoc::open(paths)?;
    if let Some(d) = download {
        doc.set(&["limits", "download"], format_bytes(d))?;
    }
    if let Some(u) = upload {
        doc.set(&["limits", "upload"], format_bytes(u))?;
    }
    if let Some(m) = max_concurrent {
        doc.set(&["limits", "max_concurrent"], i64::from(m))?;
    }
    doc.save()
}

impl Manager {
    /// Re-evaluates the schedule; cheap enough for every tick. On a window
    /// change it applies the new limits and tells the clients.
    pub(super) async fn evaluate_schedule(&mut self) {
        if !self.timetable.windows.is_empty() {
            let now = self.timetable.clock.now();
            let ts = now.timestamp();
            let idx = schedule::current(&self.timetable.windows, &now);
            let tt = &mut self.timetable;
            let stale =
                tt.next.is_some_and(|t| ts >= t.at) || tt.computed_at.is_some_and(|c| ts < c);
            let changed = !tt.initialised || idx != tt.active;
            if stale || changed || tt.computed_at.is_none() {
                tt.next = schedule::next_transition(&tt.windows, &now);
                tt.computed_at = Some(ts);
            }
            if changed {
                let first = !tt.initialised;
                let prev = tt.active;
                tt.initialised = true;
                tt.active = idx;
                tt.temp = LimitsUpdate::default();
                self.apply_effective().await;
                self.announce_transition(prev, first);
                // Preempt / start right away instead of on the next tick.
                self.urgent = true;
            }
        }
        self.sync_global_limits().await;
    }

    fn announce_transition(&self, prev: Option<usize>, first: bool) {
        let tt = &self.timetable;
        let until = tt
            .next_clock()
            .map(|t| format!(" (until {t})"))
            .unwrap_or_default();
        let text = match (tt.window(), prev.and_then(|i| tt.windows.get(i))) {
            (Some(w), _) if w.action == ScheduleAction::Pause => format!(
                "schedule '{}' {}: downloads paused{until}",
                w.name,
                if first { "active" } else { "started" }
            ),
            (Some(w), _) => {
                let o = describe_overrides(w);
                let o = if o.is_empty() {
                    String::new()
                } else {
                    format!(": {o}")
                };
                format!(
                    "schedule '{}' {}{o}{until}",
                    w.name,
                    if first { "active" } else { "started" }
                )
            }
            (None, Some(p)) if p.action == ScheduleAction::Pause => format!(
                "schedule '{}' ended: downloads resume ({})",
                p.name,
                describe_limits(&self.limits)
            ),
            (None, Some(p)) => format!(
                "schedule '{}' ended: limits restored ({})",
                p.name,
                describe_limits(&self.limits)
            ),
            (None, None) => return,
        };
        self.message(Level::Info, text);
    }

    /// Makes `self.limits` the effective limits and tells everyone.
    async fn apply_effective(&mut self) {
        let eff = self.timetable.effective();
        if eff == self.limits {
            return;
        }
        self.limits = eff.clone();
        self.sync_global_limits().await;
        self.emit(Event::Limits { limits: eff });
    }

    /// Pushes the effective download/upload cap to aria2 when it is not
    /// there yet (after a change, an engine start or a failed attempt).
    pub(super) async fn sync_global_limits(&mut self) {
        let want = (self.limits.download, self.limits.upload);
        if self.timetable.applied == Some(want) {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        match client
            .change_global_option(&global_limits(want.0, want.1, None))
            .await
        {
            Ok(()) => self.timetable.applied = Some(want),
            Err(e) => tracing::warn!("cannot apply global limits: {e}"),
        }
    }

    /// The engine (re)started: its global options are gone.
    pub(super) fn limits_not_applied(&mut self) {
        self.timetable.applied = None;
    }

    pub(super) fn schedule_status(&self) -> ScheduleStatus {
        self.timetable.status()
    }

    pub(super) async fn set_limits(&mut self, u: LimitsUpdate) -> Result<Limits, RpcError> {
        if u.max_concurrent == Some(0) {
            return Err(RpcError::invalid("max_concurrent must be at least 1"));
        }
        // Fields the active run window sets itself are only overridden until
        // the next transition; everything else is a change of the base limits.
        let window = self
            .timetable
            .window()
            .filter(|w| w.action == ScheduleAction::Run)
            .cloned();
        let w_dl = window.as_ref().is_some_and(|w| w.download_limit.is_some());
        let w_ul = window.as_ref().is_some_and(|w| w.upload_limit.is_some());
        let w_mc = window.as_ref().is_some_and(|w| w.max_concurrent.is_some());

        let mut next = self.timetable.base.clone();
        let mut temp = self.timetable.temp.clone();
        let mut persist_now = LimitsUpdate::default();
        let mut temporary = false;
        if let Some(d) = u.download {
            if w_dl {
                temp.download = Some(d);
                temporary = true;
            } else {
                next.download = d;
                persist_now.download = Some(d);
            }
        }
        if let Some(v) = u.upload {
            if w_ul {
                temp.upload = Some(v);
                temporary = true;
            } else {
                next.upload = v;
                persist_now.upload = Some(v);
            }
        }
        if let Some(m) = u.max_concurrent {
            if w_mc {
                temp.max_concurrent = Some(m);
                temporary = true;
            } else {
                next.max_concurrent = m;
                persist_now.max_concurrent = Some(m);
            }
        }
        let previous = (self.timetable.base.clone(), self.timetable.temp.clone());
        self.timetable.base = next;
        self.timetable.temp = temp;
        let eff = self.timetable.effective();

        if (eff.download, eff.upload) != (self.limits.download, self.limits.upload) {
            match self.client.clone() {
                Some(client) => {
                    if let Err(e) = client
                        .change_global_option(&global_limits(eff.download, eff.upload, None))
                        .await
                    {
                        (self.timetable.base, self.timetable.temp) = previous;
                        return Err(engine_err(e));
                    }
                    self.timetable.applied = Some((eff.download, eff.upload));
                }
                // Applied when the engine starts.
                None => self.timetable.applied = None,
            }
        }
        self.limits = eff.clone();
        self.emit(Event::Limits {
            limits: eff.clone(),
        });

        if temporary && let Some(w) = &window {
            self.message(
                Level::Info,
                format!(
                    "limit change applies until the schedule window '{}' ends; the limits in config.toml are unchanged",
                    w.name
                ),
            );
        }
        let (download, upload, max_concurrent) = (
            persist_now.download,
            persist_now.upload,
            persist_now.max_concurrent,
        );
        if (download, upload, max_concurrent) != (None, None, None)
            && let Some(paths) = self.timetable.config.clone()
        {
            let res = tokio::task::spawn_blocking(move || {
                persist(&paths, download, upload, max_concurrent)
            })
            .await;
            let err = match res {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e.to_string()),
                Err(e) => Some(e.to_string()),
            };
            if let Some(e) = err {
                self.message(
                    Level::Warn,
                    format!(
                        "limits changed for this session only; cannot save them to config.toml: {e}"
                    ),
                );
            }
        }
        Ok(eff)
    }
}
