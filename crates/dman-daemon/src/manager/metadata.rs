//! Keeping the `.torrent` metadata of magnet jobs.
//!
//! A magnet job only knows its info hash until aria2 has fetched the metadata
//! from peers. aria2 then saves it as `<dir>/<infohash>.torrent`
//! (`bt-save-metadata`, set in the generated aria2 configuration) and continues
//! under a follow-up gid. dman reads that file once, checks that it really
//! describes the job's info hash and stores its bytes as the job's blob, like
//! the `.torrent` of a torrent job. A later re-add (daemon restart, engine
//! restart, retry, restore) then starts from the stored metadata instead of the
//! magnet link, so a seeding or unfinished magnet job comes back even when no
//! peer is left to serve the metadata again.
//!
//! The saved file is only read, never removed: it is aria2's output in the
//! user's download directory.

use std::path::{Path, PathBuf};

use dman_proto::{JobId, SourceKind};
use serde_json::Value;

use super::Manager;

/// Attempts (one per tick) to find the saved metadata before giving up.
const MAX_TRIES: u8 = 30;

/// Whether a freshly loaded record still needs its metadata captured: an
/// unfinished magnet job without a blob whose metadata aria2 already had.
pub(crate) fn wanted_at_load(rec: &dman_store::JobRecord) -> bool {
    rec.kind == SourceKind::Magnet
        && !rec.has_blob
        && !rec.state.is_stopped()
        && rec.extra.get("details_final").and_then(Value::as_bool) == Some(true)
}

/// Candidate paths of the metadata aria2 saved for `info_hash` in `dir`.
fn candidates(dir: &Path, info_hash: &str) -> [PathBuf; 2] {
    [
        dir.join(format!("{}.torrent", info_hash.to_ascii_lowercase())),
        dir.join(format!("{}.torrent", info_hash.to_ascii_uppercase())),
    ]
}

/// Outcome of one attempt to read the saved metadata.
#[derive(Debug)]
pub(crate) enum Read {
    /// The bytes of a `.torrent` with the expected info hash.
    Found(Vec<u8>),
    /// Not there (yet).
    Missing,
    /// There but unusable; retrying will not help.
    Invalid(String),
}

/// Reads `<dir>/<info_hash>.torrent` and checks that it describes `info_hash`.
pub(crate) fn read_saved(dir: &Path, info_hash: &str) -> Read {
    if info_hash.len() != 40 || !info_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Read::Invalid(format!("unexpected info hash `{info_hash}`"));
    }
    for path in candidates(dir, info_hash) {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Read::Invalid(format!("cannot read {}: {e}", path.display())),
        };
        return match dman_torrent::parse_torrent(&bytes) {
            Ok(meta) if meta.info_hash.eq_ignore_ascii_case(info_hash) => Read::Found(bytes),
            Ok(meta) => Read::Invalid(format!(
                "{} describes info hash {}, not {info_hash}",
                path.display(),
                meta.info_hash
            )),
            Err(e) => Read::Invalid(format!("{} is not a valid torrent: {e}", path.display())),
        };
    }
    Read::Missing
}

impl Manager {
    /// Stores the metadata of magnet jobs that have it on disk now. Every tick;
    /// only touches jobs flagged by a follow-up or by final details.
    pub(super) async fn capture_metadata(&mut self) {
        let todo: Vec<(JobId, PathBuf, String)> = self
            .jobs
            .values()
            .filter(|e| e.need_metadata && !e.rec.has_blob)
            .filter_map(|e| {
                let hash = e.rec.info_hash.clone()?;
                Some((e.rec.id, PathBuf::from(&e.rec.dir), hash))
            })
            .collect();
        for (id, dir, hash) in todo {
            let read = {
                let hash = hash.clone();
                tokio::task::spawn_blocking(move || read_saved(&dir, &hash))
                    .await
                    .unwrap_or_else(|e| Read::Invalid(e.to_string()))
            };
            let give_up = |why: &str| {
                tracing::warn!(
                    %id,
                    "cannot keep the torrent metadata of this magnet job ({why}); \
                     after a restart it is fetched again from the magnet link"
                );
            };
            match read {
                Read::Found(bytes) => {
                    if let Err(e) = self.db(move |s| s.put_blob(id, &bytes)).await {
                        give_up(&e.message);
                        if let Some(e) = self.jobs.get_mut(&id) {
                            e.need_metadata = false;
                        }
                        continue;
                    }
                    if let Some(e) = self.jobs.get_mut(&id) {
                        e.rec.has_blob = true;
                        e.need_metadata = false;
                        e.dirty = true;
                    }
                    self.urgent = true;
                    tracing::info!(%id, "stored the torrent metadata of a magnet job");
                }
                Read::Missing => {
                    let Some(e) = self.jobs.get_mut(&id) else {
                        continue;
                    };
                    e.metadata_tries = e.metadata_tries.saturating_add(1);
                    if e.metadata_tries >= MAX_TRIES {
                        e.need_metadata = false;
                        give_up("aria2 did not save it in the download directory");
                    }
                }
                Read::Invalid(why) => {
                    if let Some(e) = self.jobs.get_mut(&id) {
                        e.need_metadata = false;
                    }
                    give_up(&why);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn torrent() -> (Vec<u8>, String) {
        let info = b"d6:lengthi3e4:name1:a12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";
        let mut t = b"d4:info".to_vec();
        t.extend_from_slice(info);
        t.push(b'e');
        let hash = dman_torrent::parse_torrent(&t).unwrap().info_hash;
        (t, hash)
    }

    #[test]
    fn reads_only_matching_metadata() {
        let d = tempfile::tempdir().unwrap();
        let (t, hash) = torrent();
        assert!(matches!(read_saved(d.path(), &hash), Read::Missing));
        std::fs::write(d.path().join(format!("{hash}.torrent")), &t).unwrap();
        assert!(matches!(read_saved(d.path(), &hash), Read::Found(b) if b == t));
        // Upper-case hash in the record still finds the lower-case file.
        assert!(matches!(
            read_saved(d.path(), &hash.to_ascii_uppercase()),
            Read::Found(_)
        ));
        // A file under the name of another hash is refused.
        let other = "0".repeat(40);
        std::fs::write(d.path().join(format!("{other}.torrent")), &t).unwrap();
        assert!(matches!(read_saved(d.path(), &other), Read::Invalid(_)));
        let junk = "1".repeat(40);
        std::fs::write(d.path().join(format!("{junk}.torrent")), b"junk").unwrap();
        assert!(matches!(read_saved(d.path(), &junk), Read::Invalid(_)));
        // Never a path: only 40 hex digits are looked up.
        assert!(matches!(read_saved(d.path(), "../x"), Read::Invalid(_)));
    }
}
