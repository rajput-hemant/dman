//! The animation clock: spinners, eased progress bars and row flashes.
//!
//! Animations are pure state advanced by `Msg::Frame(dt)`. The runtime sends
//! frames only while [`crate::app::State::frame_interval`] asks for them:
//! about 30 fps while a bar eases or a row flashes, 10 fps while only a
//! spinner turns, and none at all when nothing moves or animations are off
//! (`[ui] animations = false`, `DMAN_REDUCE_MOTION`). Without animations every
//! value is drawn as is and spinners stand still.

use std::collections::HashMap;
use std::time::Duration;

use dman_proto::{Job, JobId, JobState};

/// How long a row flashes after its state changed.
pub const FLASH: Duration = Duration::from_millis(900);

/// Time constant of the progress easing.
const EASE_MS: f64 = 140.0;

/// A row flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flash {
    /// Time left.
    pub left: Duration,
    /// The new state (decides the colour).
    pub to: JobState,
}

/// Animation state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Anim {
    /// Animation time in milliseconds (spinners, shuttles).
    pub ms: u64,
    /// Rows flashing after a state change.
    pub flashes: HashMap<JobId, Flash>,
    /// Progress as drawn (eases towards the real value).
    pub shown: HashMap<JobId, f64>,
}

impl Anim {
    /// A job changed from `old` to `new`: flash its row.
    pub fn on_change(&mut self, old: &Job, new: &Job) {
        if old.state != new.state {
            self.flashes.insert(
                new.id,
                Flash {
                    left: FLASH,
                    to: new.state,
                },
            );
        }
    }

    /// Advance by `dt`.
    pub fn advance(&mut self, dt: Duration, jobs: &HashMap<JobId, Job>) {
        self.ms = self
            .ms
            .wrapping_add(u64::try_from(dt.as_millis()).unwrap_or(0));
        for f in self.flashes.values_mut() {
            f.left = f.left.saturating_sub(dt);
        }
        self.flashes
            .retain(|id, f| !f.left.is_zero() && jobs.contains_key(id));
        let k = 1.0 - (-dt.as_secs_f64() * 1000.0 / EASE_MS).exp();
        self.shown.retain(|id, _| jobs.contains_key(id));
        for j in jobs.values() {
            let target = target(j);
            let e = self.shown.entry(j.id).or_insert(target);
            if target < *e || (target - *e).abs() < 0.0005 {
                *e = target;
            } else {
                *e += (target - *e) * k;
            }
        }
    }

    /// Whether a bar is still easing or a row flashing.
    pub fn busy(&self, jobs: &HashMap<JobId, Job>) -> bool {
        !self.flashes.is_empty()
            || jobs.values().any(|j| {
                self.shown
                    .get(&j.id)
                    .is_some_and(|e| (target(j) - e).abs() >= 0.0005)
            })
    }

    /// The progress to draw for `job`.
    pub fn progress(&self, job: &Job) -> f64 {
        let t = target(job);
        self.shown.get(&job.id).map_or(t, |e| e.min(t))
    }

    /// Flash strength of a row (1 = just changed, 0 = none) and its state.
    pub fn flash(&self, id: JobId) -> Option<(f32, JobState)> {
        self.flashes.get(&id).map(|f| {
            // Ratio of two short durations; precision loss is irrelevant.
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let s = (f.left.as_millis() as f64 / FLASH.as_millis() as f64) as f32;
            (s, f.to)
        })
    }
}

fn target(j: &Job) -> f64 {
    if j.total_bytes == 0 {
        0.0
    } else {
        j.progress().clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{job, jobs_map};

    #[test]
    fn bars_ease_and_flashes_fade() {
        let mut a = job(1, "a", JobState::Active);
        a.total_bytes = 100;
        a.completed_bytes = 10;
        let mut jobs = jobs_map(vec![a.clone()]);
        let mut anim = Anim::default();
        anim.advance(Duration::from_millis(33), &jobs);
        assert!(!anim.busy(&jobs));
        let mut b = a.clone();
        b.completed_bytes = 90;
        b.state = JobState::Complete;
        anim.on_change(&a, &b);
        jobs.insert(b.id, b.clone());
        assert!(anim.busy(&jobs));
        anim.advance(Duration::from_millis(33), &jobs);
        let p = anim.progress(&b);
        assert!(p > 0.1 && p < 0.9, "{p}");
        assert!(anim.flash(b.id).is_some());
        for _ in 0..60 {
            anim.advance(Duration::from_millis(33), &jobs);
        }
        assert!((anim.progress(&b) - 0.9).abs() < 1e-9);
        assert!(anim.flash(b.id).is_none());
        assert!(!anim.busy(&jobs));
    }
}
