//! In-memory speed history: one global ring buffer and one per running job.
//! One sample is recorded per poll tick; nothing here is persisted.

use std::collections::{HashMap, VecDeque};

use dman_proto::{JobId, SpeedSample};

/// Samples kept for the global history.
pub const GLOBAL_CAPACITY: usize = 600;
/// Samples kept per job.
pub const JOB_CAPACITY: usize = 300;

/// Ring buffers of speed samples.
#[derive(Debug, Default)]
pub struct History {
    global: VecDeque<SpeedSample>,
    jobs: HashMap<JobId, VecDeque<SpeedSample>>,
}

fn push(buf: &mut VecDeque<SpeedSample>, cap: usize, s: SpeedSample) {
    while buf.len() >= cap {
        buf.pop_front();
    }
    buf.push_back(s);
}

impl History {
    /// Records one tick at unix-millisecond time `t`: the global speeds and
    /// `(id, down, up)` for every running job.
    pub fn record(&mut self, t: i64, global: (u64, u64), running: &[(JobId, u64, u64)]) {
        push(
            &mut self.global,
            GLOBAL_CAPACITY,
            SpeedSample {
                t,
                down: global.0,
                up: global.1,
            },
        );
        for &(id, down, up) in running {
            push(
                self.jobs.entry(id).or_default(),
                JOB_CAPACITY,
                SpeedSample { t, down, up },
            );
        }
    }

    /// Drops the buffers of jobs for which `keep` is false (stopped or gone).
    pub fn retain_jobs(&mut self, keep: impl Fn(JobId) -> bool) {
        self.jobs.retain(|id, _| keep(*id));
    }

    /// Global samples, oldest first.
    pub fn global(&self) -> Vec<SpeedSample> {
        self.global.iter().copied().collect()
    }

    /// Samples of one job, oldest first.
    pub fn job(&self, id: JobId) -> Vec<SpeedSample> {
        self.jobs
            .get(&id)
            .map(|b| b.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Number of jobs with a buffer.
    pub fn job_buffers(&self) -> usize {
        self.jobs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_is_a_ring() {
        let mut h = History::default();
        for i in 0..(GLOBAL_CAPACITY as i64 + 50) {
            h.record(i, (i as u64, 0), &[]);
        }
        let g = h.global();
        assert_eq!(g.len(), GLOBAL_CAPACITY);
        assert_eq!(g[0].t, 50);
        assert_eq!(g.last().unwrap().t, GLOBAL_CAPACITY as i64 + 49);
        assert!(g.windows(2).all(|w| w[0].t < w[1].t));
    }

    #[test]
    fn job_buffers_are_bounded_and_dropped() {
        let mut h = History::default();
        let (a, b) = (JobId(1), JobId(2));
        for i in 0..(JOB_CAPACITY as i64 + 10) {
            h.record(i, (0, 0), &[(a, 1, 2), (b, 3, 4)]);
        }
        assert_eq!(h.job(a).len(), JOB_CAPACITY);
        assert_eq!(h.job(a)[0].t, 10);
        assert_eq!(
            h.job(b)[0],
            SpeedSample {
                t: 10,
                down: 3,
                up: 4
            }
        );
        assert_eq!(h.job_buffers(), 2);
        // Job a stops: its buffer goes, b's stays.
        h.retain_jobs(|id| id == b);
        assert!(h.job(a).is_empty());
        assert_eq!(h.job(b).len(), JOB_CAPACITY);
        assert_eq!(h.job_buffers(), 1);
        assert_eq!(h.job(JobId(99)), vec![]);
    }
}
