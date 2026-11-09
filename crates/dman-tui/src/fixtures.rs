//! Test fixtures: jobs in every state and a ready-made state.

use std::collections::HashMap;

use dman_proto::{Engine, GlobalStats, Job, JobId, JobState, Limits, SourceKind};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// A minimal job.
pub fn job(id: u64, name: &str, state: JobState) -> Job {
    Job {
        id: JobId(id),
        engine: Engine::Aria2,
        kind: SourceKind::Http,
        name: name.into(),
        state,
        dir: "/home/me/Downloads".into(),
        source: format!("https://example.org/{name}"),
        total_bytes: 0,
        completed_bytes: 0,
        uploaded_bytes: 0,
        download_speed: 0,
        upload_speed: 0,
        connections: 0,
        seeders: None,
        info_hash: None,
        added_at: 1_700_000_000 + i64::try_from(id).unwrap_or(0),
        completed_at: None,
        queue_position: None,
        error: None,
    }
}

/// Index jobs by id.
pub fn jobs_map(jobs: Vec<Job>) -> HashMap<JobId, Job> {
    jobs.into_iter().map(|j| (j.id, j)).collect()
}

/// One job per interesting state, for snapshots.
pub fn sample_jobs() -> Vec<Job> {
    let mut active = job(1, "ubuntu-24.04.1-desktop-amd64.iso", JobState::Active);
    active.kind = SourceKind::Torrent;
    active.total_bytes = 6 * GIB;
    active.completed_bytes = 4 * GIB;
    active.download_speed = 12 * MIB;
    active.upload_speed = 800 * 1024;
    active.uploaded_bytes = GIB;
    active.connections = 42;
    active.seeders = Some(118);

    let mut http = job(2, "big-dataset-2026.tar.zst", JobState::Active);
    http.total_bytes = 20 * GIB;
    http.completed_bytes = 2 * GIB;
    http.download_speed = 3 * MIB;
    http.connections = 8;

    let mut seeding = job(3, "debian-13.2.0-amd64-netinst.iso", JobState::Seeding);
    seeding.kind = SourceKind::Torrent;
    seeding.total_bytes = 700 * MIB;
    seeding.completed_bytes = 700 * MIB;
    seeding.uploaded_bytes = 1050 * MIB;
    seeding.upload_speed = 256 * 1024;
    seeding.connections = 5;
    seeding.seeders = Some(40);

    let mut waiting = job(4, "Big Buck Bunny (1080p).mp4", JobState::Waiting);
    waiting.engine = Engine::YtDlp;
    waiting.kind = SourceKind::Video;
    waiting.queue_position = Some(0);

    let mut paused = job(5, "archlinux-2026.11.01-x86_64.iso", JobState::Paused);
    paused.kind = SourceKind::Magnet;
    paused.total_bytes = 1200 * MIB;
    paused.completed_bytes = 300 * MIB;
    paused.queue_position = Some(1);
    paused.seeders = Some(0);

    let mut complete = job(6, "report-q3.pdf", JobState::Complete);
    complete.total_bytes = 3 * MIB;
    complete.completed_bytes = 3 * MIB;
    complete.completed_at = Some(1_700_000_100);

    let mut error = job(7, "missing-file.zip", JobState::Error);
    error.error = Some("404 Not Found".into());
    error.completed_at = Some(1_700_000_050);

    vec![active, http, seeding, waiting, paused, complete, error]
}

/// Stats matching [`sample_jobs`].
pub fn sample_stats() -> GlobalStats {
    GlobalStats {
        download_speed: 15 * MIB,
        upload_speed: 1056 * 1024,
        num_active: 3,
        num_waiting: 2,
        num_stopped: 2,
        disk_free: Some(250 * GIB),
        total_downloaded: 0,
        total_uploaded: 0,
    }
}

/// Limits for snapshots.
pub fn sample_limits() -> Limits {
    Limits {
        download: 0,
        upload: 2 * MIB,
        max_concurrent: 5,
    }
}
