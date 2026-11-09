//! Queue decisions (decision D5: dman owns the queue) as pure functions.

use std::collections::HashMap;

use dman_proto::JobId;
use dman_proto::method::Reorder;

/// Outcome of one scheduling pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct StartPlan {
    /// Jobs to unpause now, in order.
    pub start: Vec<JobId>,
    /// Jobs that would have started but are held back by the disk guard.
    pub blocked: Vec<JobId>,
}

/// Picks the jobs to start. `running` counts downloading jobs (seeding jobs do
/// not count, `bt-detach-seed-only` semantics); `candidates` are startable
/// jobs in queue order; `disk_ok` is asked only for jobs that would start. A
/// job held back by the disk guard does not use up a slot, so a later job on
/// another volume can still start.
pub fn plan_starts(
    max_concurrent: u32,
    running: u32,
    candidates: &[JobId],
    mut disk_ok: impl FnMut(JobId) -> bool,
) -> StartPlan {
    let mut plan = StartPlan::default();
    let mut slots = max_concurrent.saturating_sub(running);
    for &id in candidates {
        if slots == 0 {
            break;
        }
        if disk_ok(id) {
            plan.start.push(id);
            slots -= 1;
        } else {
            plan.blocked.push(id);
        }
    }
    plan
}

/// Running jobs beyond `max_concurrent` that should be paused again (e.g.
/// downloads aria2 resumed by itself after an engine restart). `running` is
/// `(id, queue_key)`; the jobs latest in the queue are preempted.
pub fn plan_preempt(max_concurrent: u32, running: &[(JobId, u64)]) -> Vec<JobId> {
    let max = max_concurrent as usize;
    if running.len() <= max {
        return Vec::new();
    }
    let mut sorted = running.to_vec();
    sorted.sort_by_key(|&(id, key)| (key, id));
    sorted[max..].iter().map(|&(id, _)| id).collect()
}

/// 0-based queue positions of the given waiting jobs (`(id, queue_key)`).
pub fn queue_positions(waiting: &[(JobId, u64)]) -> HashMap<JobId, u32> {
    let mut sorted = waiting.to_vec();
    sorted.sort_by_key(|&(id, key)| (key, id));
    sorted
        .into_iter()
        .enumerate()
        .map(|(i, (id, _))| (id, u32::try_from(i).unwrap_or(u32::MAX)))
        .collect()
}

/// Moves `id` within `queue` (`(id, queue_key)` of all queued jobs) and returns
/// the new keys of every job whose key changed, plus the moved job's new
/// 0-based position. The existing keys are reused as slots, so the order
/// relative to jobs outside `queue` is preserved and no new keys are needed.
/// `None` when `id` is not in `queue`.
pub fn reorder(
    queue: &[(JobId, u64)],
    id: JobId,
    to: Reorder,
) -> Option<(Vec<(JobId, u64)>, usize)> {
    let mut sorted = queue.to_vec();
    sorted.sort_by_key(|&(id, key)| (key, id));
    let slots: Vec<u64> = sorted.iter().map(|&(_, k)| k).collect();
    let mut order: Vec<JobId> = sorted.iter().map(|&(id, _)| id).collect();
    let from = order.iter().position(|&j| j == id)?;
    let last = order.len() - 1;
    let target = match to {
        Reorder::Top => 0,
        Reorder::Bottom => last,
        Reorder::To(n) => (n as usize).min(last),
        Reorder::By(d) => {
            let t = from as i64 + i64::from(d);
            t.clamp(0, last as i64) as usize
        }
    };
    order.remove(from);
    order.insert(target, id);
    // Keys must be strictly increasing for a stable order; duplicates (should
    // not happen) are made unique by bumping.
    let mut changed = Vec::new();
    let mut prev: Option<u64> = None;
    for (i, job) in order.iter().enumerate() {
        let mut key = slots[i];
        if let Some(p) = prev
            && key <= p
        {
            key = p + 1;
        }
        prev = Some(key);
        if sorted.iter().any(|&(j, k)| j == *job && k != key) {
            changed.push((*job, key));
        }
    }
    Some((changed, target))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[u64]) -> Vec<JobId> {
        v.iter().map(|&i| JobId(i)).collect()
    }

    #[test]
    fn starts_up_to_max_concurrent() {
        let c = ids(&[1, 2, 3]);
        assert_eq!(plan_starts(1, 0, &c, |_| true).start, ids(&[1]));
        assert_eq!(plan_starts(2, 1, &c, |_| true).start, ids(&[1]));
        assert_eq!(plan_starts(3, 0, &c, |_| true).start, ids(&[1, 2, 3]));
        assert!(plan_starts(2, 2, &c, |_| true).start.is_empty());
        assert!(plan_starts(1, 5, &c, |_| true).start.is_empty());
    }

    #[test]
    fn seeding_does_not_count_so_slots_stay_free() {
        // Two seeding jobs are not passed as `running`: a slot is free.
        let c = ids(&[4]);
        assert_eq!(plan_starts(1, 0, &c, |_| true).start, ids(&[4]));
    }

    #[test]
    fn disk_guard_blocks_without_using_a_slot() {
        let c = ids(&[1, 2, 3]);
        let p = plan_starts(1, 0, &c, |id| id != JobId(1));
        assert_eq!(p.start, ids(&[2]));
        assert_eq!(p.blocked, ids(&[1]));
        let p = plan_starts(2, 0, &c, |_| false);
        assert!(p.start.is_empty());
        assert_eq!(p.blocked, ids(&[1, 2, 3]));
        // Without free slots the disk is not even checked.
        let p = plan_starts(1, 1, &c, |_| panic!("not asked"));
        assert_eq!(p, StartPlan::default());
    }

    #[test]
    fn preempts_latest_jobs() {
        let r = [(JobId(1), 30), (JobId(2), 10), (JobId(3), 20)];
        assert_eq!(plan_preempt(1, &r), ids(&[3, 1]));
        assert!(plan_preempt(3, &r).is_empty());
    }

    #[test]
    fn positions() {
        let p = queue_positions(&[(JobId(5), 9), (JobId(6), 3)]);
        assert_eq!(p[&JobId(6)], 0);
        assert_eq!(p[&JobId(5)], 1);
    }

    fn apply(q: &[(JobId, u64)], changed: &[(JobId, u64)]) -> Vec<u64> {
        let mut q: Vec<(JobId, u64)> = q
            .iter()
            .map(|&(id, k)| {
                let k = changed
                    .iter()
                    .find(|(c, _)| *c == id)
                    .map_or(k, |&(_, nk)| nk);
                (id, k)
            })
            .collect();
        q.sort_by_key(|&(_, k)| k);
        q.into_iter().map(|(id, _)| id.0).collect()
    }

    #[test]
    fn reorder_renumbers() {
        let q = [
            (JobId(1), 10),
            (JobId(2), 20),
            (JobId(3), 30),
            (JobId(4), 40),
        ];
        let (c, pos) = reorder(&q, JobId(4), Reorder::Top).unwrap();
        assert_eq!(pos, 0);
        assert_eq!(apply(&q, &c), vec![4, 1, 2, 3]);
        // Keys reused as slots.
        assert!(c.iter().all(|(_, k)| [10, 20, 30, 40].contains(k)));

        let (c, _) = reorder(&q, JobId(1), Reorder::Bottom).unwrap();
        assert_eq!(apply(&q, &c), vec![2, 3, 4, 1]);
        let (c, _) = reorder(&q, JobId(1), Reorder::By(2)).unwrap();
        assert_eq!(apply(&q, &c), vec![2, 3, 1, 4]);
        let (c, _) = reorder(&q, JobId(3), Reorder::By(-10)).unwrap();
        assert_eq!(apply(&q, &c), vec![3, 1, 2, 4]);
        let (c, pos) = reorder(&q, JobId(2), Reorder::To(99)).unwrap();
        assert_eq!(pos, 3);
        assert_eq!(apply(&q, &c), vec![1, 3, 4, 2]);
        let (c, _) = reorder(&q, JobId(2), Reorder::To(1)).unwrap();
        assert!(c.is_empty());
        assert!(reorder(&q, JobId(9), Reorder::Top).is_none());
    }

    #[test]
    fn reorder_handles_duplicate_keys() {
        let q = [(JobId(1), 5), (JobId(2), 5), (JobId(3), 5)];
        let (c, _) = reorder(&q, JobId(3), Reorder::Top).unwrap();
        assert_eq!(apply(&q, &c), vec![3, 1, 2]);
    }
}
