//! The redb-backed [`Store`].

use std::path::Path;
use std::sync::Arc;

use dman_proto::JobId;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::{JobRecord, Result};

/// Current on-disk schema version.
pub const SCHEMA_VERSION: u64 = 1;

const JOBS: TableDefinition<'_, u64, &[u8]> = TableDefinition::new("jobs");
const BLOBS: TableDefinition<'_, u64, &[u8]> = TableDefinition::new("blobs");
const META: TableDefinition<'_, &str, u64> = TableDefinition::new("meta");
// key: (job id, per-job sequence number); value: JSON `RenameEntry`.
const RENAMES: TableDefinition<'_, (u64, u64), &[u8]> = TableDefinition::new("renames");

const K_SCHEMA: &str = "schema_version";
const K_NEXT_ID: &str = "next_job_id";
const K_NEXT_QUEUE: &str = "next_queue_key";
const K_DOWN: &str = "total_downloaded";
const K_UP: &str = "total_uploaded";

/// Errors from the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Any redb failure (open, transaction, table, storage, commit).
    #[error("database error: {0}")]
    Db(Box<redb::Error>),
    /// A record could not be (de)serialised.
    #[error("record serialisation error: {0}")]
    Json(#[from] serde_json::Error),
    /// Filesystem error while preparing the database location.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// The database was written by a newer dman.
    #[error(
        "store schema version {found} is newer than supported version {supported}; upgrade dman"
    )]
    SchemaTooNew {
        /// Version found on disk.
        found: u64,
        /// Highest version this build understands.
        supported: u64,
    },
}

macro_rules! from_redb {
    ($($t:ty),* $(,)?) => {$(
        impl From<$t> for StoreError {
            fn from(e: $t) -> Self {
                StoreError::Db(Box::new(redb::Error::from(e)))
            }
        }
    )*};
}

from_redb!(
    redb::Error,
    redb::DatabaseError,
    redb::StorageError,
    redb::TableError,
    redb::TransactionError,
    redb::CommitError,
);

/// One recorded rename of a job's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameEntry {
    /// Previous name.
    pub from: String,
    /// New name.
    pub to: String,
    /// Unix seconds when the rename happened.
    pub at: i64,
}

/// Handle to the persistent store. Cheap to clone; safe to share between threads.
#[derive(Clone)]
pub struct Store {
    db: Arc<Database>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    /// Opens (creating if needed) the store at `path`, creating parent directories.
    /// Refuses a database whose schema version is newer than [`SCHEMA_VERSION`].
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let db = Database::create(path)?;
        let store = Store { db: Arc::new(db) };
        store.init_schema()?;
        tracing::debug!(path = %path.display(), "store opened");
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut meta = tx.open_table(META)?;
            let found = meta.get(K_SCHEMA)?.map(|v| v.value());
            match found {
                Some(v) if v > SCHEMA_VERSION => {
                    return Err(crate::StoreError::SchemaTooNew {
                        found: v,
                        supported: SCHEMA_VERSION,
                    });
                }
                Some(_) => {}
                None => {
                    meta.insert(K_SCHEMA, SCHEMA_VERSION)?;
                }
            }
            tx.open_table(JOBS)?;
            tx.open_table(BLOBS)?;
            tx.open_table(RENAMES)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Allocates the next job id (persistent, monotonically increasing, from 1).
    pub fn next_job_id(&self) -> Result<JobId> {
        let tx = self.db.begin_write()?;
        let id = bump(&tx, K_NEXT_ID, 1)?;
        tx.commit()?;
        Ok(JobId(id))
    }

    /// Returns a queue key larger than every key handed out or stored so far.
    pub fn next_queue_key(&self) -> Result<u64> {
        let tx = self.db.begin_write()?;
        let key = bump(&tx, K_NEXT_QUEUE, 1)?;
        tx.commit()?;
        Ok(key)
    }

    /// Inserts or replaces one job record.
    pub fn put_job(&self, job: &JobRecord) -> Result<()> {
        self.put_jobs(std::slice::from_ref(job))
    }

    /// Inserts or replaces many records in a single transaction.
    pub fn put_jobs(&self, jobs: &[JobRecord]) -> Result<()> {
        if jobs.is_empty() {
            return Ok(());
        }
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(JOBS)?;
            let mut meta = tx.open_table(META)?;
            let mut next_queue = meta.get(K_NEXT_QUEUE)?.map_or(1, |v| v.value());
            let mut next_id = meta.get(K_NEXT_ID)?.map_or(1, |v| v.value());
            let (start_queue, start_id) = (next_queue, next_id);
            let mut buf = Vec::with_capacity(1024);
            for job in jobs {
                buf.clear();
                serde_json::to_writer(&mut buf, job)?;
                table.insert(job.id.0, buf.as_slice())?;
                next_queue = next_queue.max(job.queue_key.saturating_add(1));
                next_id = next_id.max(job.id.0.saturating_add(1));
            }
            if next_queue != start_queue {
                meta.insert(K_NEXT_QUEUE, next_queue)?;
            }
            if next_id != start_id {
                meta.insert(K_NEXT_ID, next_id)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Reads one job record.
    pub fn get_job(&self, id: JobId) -> Result<Option<JobRecord>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(JOBS)?;
        match table.get(id.0)? {
            Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
            None => Ok(None),
        }
    }

    /// Deletes a job record and its blob (not its files, not its rename log).
    pub fn delete_job(&self, id: JobId) -> Result<()> {
        self.purge(&[id])
    }

    /// Deletes records (and blobs) of the given jobs from history. Downloaded files
    /// are never touched.
    pub fn purge(&self, ids: &[JobId]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut jobs = tx.open_table(JOBS)?;
            let mut blobs = tx.open_table(BLOBS)?;
            for id in ids {
                jobs.remove(id.0)?;
                blobs.remove(id.0)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// All job records sorted by `queue_key`, then id.
    pub fn list_jobs(&self) -> Result<Vec<JobRecord>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(JOBS)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (_, v) = entry?;
            out.push(serde_json::from_slice::<JobRecord>(v.value())?);
        }
        out.sort_by_key(|j| (j.queue_key, j.id));
        Ok(out)
    }

    /// Stores the `.torrent` / `.metalink` bytes of a job.
    pub fn put_blob(&self, id: JobId, data: &[u8]) -> Result<()> {
        let tx = self.db.begin_write()?;
        tx.open_table(BLOBS)?.insert(id.0, data)?;
        tx.commit()?;
        Ok(())
    }

    /// Reads the stored blob of a job.
    pub fn get_blob(&self, id: JobId) -> Result<Option<Vec<u8>>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(BLOBS)?;
        Ok(table.get(id.0)?.map(|v| v.value().to_vec()))
    }

    /// Adds to the cumulative transferred-bytes counters (saturating).
    pub fn add_transferred(&self, down: u64, up: u64) -> Result<()> {
        if down == 0 && up == 0 {
            return Ok(());
        }
        let tx = self.db.begin_write()?;
        {
            let mut meta = tx.open_table(META)?;
            for (key, add) in [(K_DOWN, down), (K_UP, up)] {
                let cur = meta.get(key)?.map_or(0, |v| v.value());
                meta.insert(key, cur.saturating_add(add))?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Cumulative `(downloaded, uploaded)` bytes.
    pub fn totals(&self) -> Result<(u64, u64)> {
        let tx = self.db.begin_read()?;
        let meta = tx.open_table(META)?;
        let get = |k| -> Result<u64> { Ok(meta.get(k)?.map_or(0, |v| v.value())) };
        Ok((get(K_DOWN)?, get(K_UP)?))
    }

    /// Records that a job's output was renamed from `from` to `to` at unix time `at`.
    pub fn record_rename(&self, id: JobId, from: &str, to: &str, at: i64) -> Result<()> {
        let entry = RenameEntry {
            from: from.to_owned(),
            to: to.to_owned(),
            at,
        };
        let bytes = serde_json::to_vec(&entry)?;
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(RENAMES)?;
            let seq = match table.range((id.0, 0)..=(id.0, u64::MAX))?.next_back() {
                Some(e) => e?.0.value().1 + 1,
                None => 0,
            };
            table.insert((id.0, seq), bytes.as_slice())?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Renames recorded for a job, oldest first.
    pub fn renames(&self, id: JobId) -> Result<Vec<RenameEntry>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(RENAMES)?;
        let mut out = Vec::new();
        for e in table.range((id.0, 0)..=(id.0, u64::MAX))? {
            let (_, v) = e?;
            out.push(serde_json::from_slice(v.value())?);
        }
        Ok(out)
    }
}

/// Returns the current value of the counter `key` (initially `first`) and stores
/// the next one.
fn bump(tx: &redb::WriteTransaction, key: &str, first: u64) -> Result<u64> {
    let mut meta = tx.open_table(META)?;
    let cur = meta.get(key)?.map_or(first, |v| v.value());
    meta.insert(key, cur + 1)?;
    Ok(cur)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use dman_proto::{Engine, JobId, JobOptions, JobState, SourceKind};

    use super::*;
    use crate::{Desired, LiveFigures};

    fn rec(id: u64, queue_key: u64) -> JobRecord {
        JobRecord {
            id: JobId(id),
            engine: Engine::Aria2,
            kind: SourceKind::Http,
            name: format!("file{id}"),
            dir: "/tmp".into(),
            source: format!("http://example.com/{id}"),
            options: JobOptions::default(),
            desired: Desired::Queued,
            state: JobState::Waiting,
            gid: Some("abcd".into()),
            info_hash: None,
            total_bytes: 1000,
            completed_bytes: 10,
            uploaded_base: 100,
            uploaded_session: 20,
            added_at: 1,
            completed_at: None,
            queue_key,
            error: None,
            has_blob: false,
            extra: Default::default(),
        }
    }

    fn open(dir: &tempfile::TempDir) -> Store {
        Store::open(dir.path().join("sub/dman.redb")).unwrap()
    }

    #[test]
    fn id_counter_persists() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = open(&dir);
            assert_eq!(s.next_job_id().unwrap(), JobId(1));
            assert_eq!(s.next_job_id().unwrap(), JobId(2));
        }
        let s = open(&dir);
        assert_eq!(s.next_job_id().unwrap(), JobId(3));
    }

    #[test]
    fn put_get_list_ordering() {
        let dir = tempfile::tempdir().unwrap();
        let s = open(&dir);
        s.put_job(&rec(1, 30)).unwrap();
        s.put_job(&rec(2, 10)).unwrap();
        s.put_job(&rec(3, 10)).unwrap();
        assert_eq!(s.get_job(JobId(2)).unwrap().unwrap(), rec(2, 10));
        assert!(s.get_job(JobId(9)).unwrap().is_none());
        let ids: Vec<u64> = s.list_jobs().unwrap().iter().map(|j| j.id.0).collect();
        assert_eq!(ids, [2, 3, 1]);
        assert!(s.next_queue_key().unwrap() > 30);
        s.delete_job(JobId(2)).unwrap();
        assert_eq!(s.list_jobs().unwrap().len(), 2);
    }

    #[test]
    fn batch_put_is_fast() {
        let dir = tempfile::tempdir().unwrap();
        let s = open(&dir);
        let recs: Vec<_> = (1..=500).map(|i| rec(i, i)).collect();
        let t = Instant::now();
        s.put_jobs(&recs).unwrap();
        let el = t.elapsed();
        println!("put_jobs(500) took {el:?}");
        assert!(el.as_secs() < 2);
        assert_eq!(s.list_jobs().unwrap().len(), 500);
    }

    #[test]
    fn blob_round_trip_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let s = open(&dir);
        s.put_job(&rec(1, 1)).unwrap();
        s.put_blob(JobId(1), b"d8:announce").unwrap();
        assert_eq!(s.get_blob(JobId(1)).unwrap().unwrap(), b"d8:announce");
        s.delete_job(JobId(1)).unwrap();
        assert!(s.get_blob(JobId(1)).unwrap().is_none());
    }

    #[test]
    fn counters_accumulate_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = open(&dir);
            assert_eq!(s.totals().unwrap(), (0, 0));
            s.add_transferred(5, 1).unwrap();
        }
        let s = open(&dir);
        s.add_transferred(7, 2).unwrap();
        assert_eq!(s.totals().unwrap(), (12, 3));
    }

    #[test]
    fn newer_schema_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dman.redb");
        {
            let db = Database::create(&path).unwrap();
            let tx = db.begin_write().unwrap();
            tx.open_table(META)
                .unwrap()
                .insert(K_SCHEMA, SCHEMA_VERSION + 1)
                .unwrap();
            tx.commit().unwrap();
        }
        let err = Store::open(&path).unwrap_err();
        assert!(matches!(err, StoreError::SchemaTooNew { .. }), "{err}");
        assert!(err.to_string().contains("newer"));
    }

    #[test]
    fn forward_compat_record() {
        let json = r#"{"id":4,"engine":"aria2","kind":"http","desired":"paused",
            "state":"paused","future_field":{"a":1}}"#;
        let r: JobRecord = serde_json::from_str(json).unwrap();
        assert_eq!(r.desired, Desired::Paused);
        assert_eq!(r.gid, None);
        assert_eq!(r.total_bytes, 0);
        assert!(r.extra.contains_key("future_field"));
        let back: JobRecord = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn to_job_derives_fields() {
        let r = rec(1, 1);
        assert_eq!(r.uploaded_total(), 120);
        let j = r.to_job(None);
        assert_eq!(j.uploaded_bytes, 120);
        assert_eq!(j.queue_position, None);
        assert_eq!(j.download_speed, 0);
        let live = LiveFigures {
            download_speed: 9,
            upload_speed: 3,
            connections: 4,
            seeders: Some(2),
        };
        let j = r.to_job(Some(&live));
        assert_eq!((j.download_speed, j.upload_speed, j.connections), (9, 3, 4));
        assert_eq!(j.seeders, Some(2));
    }

    #[test]
    fn rename_log_and_purge() {
        let dir = tempfile::tempdir().unwrap();
        let s = open(&dir);
        s.put_job(&rec(1, 1)).unwrap();
        s.record_rename(JobId(1), "a", "b", 10).unwrap();
        s.record_rename(JobId(1), "b", "c", 20).unwrap();
        s.record_rename(JobId(2), "x", "y", 30).unwrap();
        let r = s.renames(JobId(1)).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(
            (r[1].from.as_str(), r[1].to.as_str(), r[1].at),
            ("b", "c", 20)
        );
        s.purge(&[JobId(1)]).unwrap();
        assert!(s.get_job(JobId(1)).unwrap().is_none());
    }
}
