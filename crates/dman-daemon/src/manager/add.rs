//! Adding jobs.

use base64::Engine as _;
use dman_aria2::{Aria2Client, DownloadStatus, Options, job_options_to_aria2};
use dman_proto::{
    AddOutcome, AddParams, AddSource, Engine, ErrorCode, JobId, JobOptions, JobState, RpcError,
    SourceKind,
};
use dman_store::{Desired, JobRecord};
use serde_json::json;

use crate::tick::{self, Counters};

use super::util::{engine_err, now, resolve_dir, unavailable};
use super::{Entry, Manager};

/// A `.torrent` file or a magnet link.
fn is_bittorrent(src: &AddSource) -> bool {
    match src {
        AddSource::Torrent { .. } => true,
        AddSource::Uris { uris } => uris
            .first()
            .is_some_and(|u| u.trim_start().to_ascii_lowercase().starts_with("magnet:")),
        AddSource::Metalink { .. } | AddSource::Video { .. } => false,
    }
}

impl Manager {
    // ----- add ---------------------------------------------------------------

    pub(super) async fn add(&mut self, mut p: AddParams) -> Result<Vec<AddOutcome>, RpcError> {
        super::util::check_cookie_file(&mut p.options, self.config_dir.as_deref())?;
        if p.sources.is_empty() {
            return Err(RpcError::invalid("no sources given"));
        }
        let videos = p
            .sources
            .iter()
            .filter(|s| matches!(s, AddSource::Video { .. }))
            .count();
        // A request the missing engine cannot serve at all fails as a whole;
        // mixed requests report per source.
        if videos == p.sources.len() && self.tools.ytdlp.is_none() {
            return Err(crate::video::unavailable());
        }
        let client = self.client.clone();
        if videos < p.sources.len() && client.is_none() {
            return Err(unavailable(
                self.engine.reason.as_deref().unwrap_or("not running"),
            ));
        }
        let dir = resolve_dir(p.options.dir.as_deref(), &self.default_dir)?;
        tokio::fs::create_dir_all(&dir).await.map_err(|e| {
            RpcError::new(
                ErrorCode::General,
                format!("cannot create {}: {e}", dir.display()),
            )
        })?;
        let dir_s = dir.display().to_string();
        let make = |o: &JobOptions| -> Result<(Options, JobOptions), RpcError> {
            let mut aopts =
                job_options_to_aria2(o).map_err(|e| RpcError::invalid(e.to_string()))?;
            aopts.set("dir", dir_s.clone());
            aopts.set("pause", "true");
            let mut options = o.clone();
            options.dir = Some(dir_s.clone());
            Ok((aopts, options))
        };
        let (aopts, options) = make(&p.options)?;
        // The public tracker list (when enabled) joins the trackers of new
        // torrents and magnets; the user's own come first, no duplicates.
        let extra = self.trackers.attach_list();
        let with_trackers = if extra.is_empty() {
            None
        } else {
            let mut o = p.options.clone();
            let merged = crate::search::merge_trackers(
                o.engine_options.get("bt-tracker").map(String::as_str),
                &extra,
            );
            o.engine_options.insert("bt-tracker".into(), merged);
            Some(make(&o)?)
        };
        let mut out = Vec::with_capacity(p.sources.len());
        for src in p.sources {
            if matches!(src, AddSource::Video { .. }) {
                let r = self.add_video(src, &options, &dir_s, p.paused).await;
                out.push(match r {
                    Ok(mut ids) => AddOutcome::Added {
                        id: ids.remove(0),
                        extra_ids: ids,
                    },
                    Err(error) => AddOutcome::Failed { error },
                });
                continue;
            }
            let Some(client) = &client else {
                continue; // unreachable: checked above
            };
            let (a, o) = match &with_trackers {
                Some(t) if is_bittorrent(&src) => t,
                _ => &(aopts.clone(), options.clone()),
            };
            let r = self.add_one(client, src, a, o, &dir_s, p.paused).await;
            out.push(match r {
                Ok(mut ids) => {
                    let id = ids.remove(0);
                    AddOutcome::Added { id, extra_ids: ids }
                }
                Err(error) => AddOutcome::Failed { error },
            });
        }
        Ok(out)
    }

    pub(super) async fn add_one(
        &mut self,
        client: &Aria2Client,
        src: AddSource,
        aopts: &Options,
        options: &JobOptions,
        dir: &str,
        paused: bool,
    ) -> Result<Vec<JobId>, RpcError> {
        let (kind, name, source, uris, blob) = match src {
            AddSource::Uris { uris } => {
                let uris: Vec<String> = uris
                    .into_iter()
                    .map(|u| u.trim().to_owned())
                    .filter(|u| !u.is_empty())
                    .collect();
                let first = uris
                    .first()
                    .cloned()
                    .ok_or_else(|| RpcError::invalid("empty URI list"))?;
                let mut kind = None;
                for u in &uris {
                    if u.contains(['\n', '\r', '\0']) {
                        return Err(RpcError::invalid("URI contains a line break"));
                    }
                    let k = tick::kind_of_uri(u).ok_or_else(|| {
                        RpcError::invalid(format!(
                            "unsupported URI `{u}` (use http, https, ftp, sftp or magnet)"
                        ))
                    })?;
                    if (k == SourceKind::Magnet || kind == Some(SourceKind::Magnet))
                        && uris.len() > 1
                    {
                        return Err(RpcError::invalid(
                            "a magnet link cannot be combined with other URIs",
                        ));
                    }
                    kind.get_or_insert(k);
                }
                let kind = kind.unwrap_or(SourceKind::Http);
                let name = options
                    .out
                    .clone()
                    .unwrap_or_else(|| tick::name_of_uri(&first));
                (kind, name, first, uris, None)
            }
            AddSource::Torrent { name, data_base64 } => {
                let data = base64::engine::general_purpose::STANDARD
                    .decode(data_base64.trim())
                    .map_err(|e| RpcError::invalid(format!("torrent data is not base64: {e}")))?;
                let display = name.strip_suffix(".torrent").unwrap_or(&name).to_owned();
                (SourceKind::Torrent, display, name, Vec::new(), Some(data))
            }
            AddSource::Metalink { name, data_base64 } => {
                let data = base64::engine::general_purpose::STANDARD
                    .decode(data_base64.trim())
                    .map_err(|e| RpcError::invalid(format!("metalink data is not base64: {e}")))?;
                let display = name
                    .strip_suffix(".meta4")
                    .or_else(|| name.strip_suffix(".metalink"))
                    .unwrap_or(&name)
                    .to_owned();
                (SourceKind::Metalink, display, name, Vec::new(), Some(data))
            }
            AddSource::Video { .. } => {
                return Err(RpcError::invalid("video sources are added by add_video"));
            }
        };

        // Persist first (the store is authoritative): a crash after this point
        // leaves a record without gid, which reconciliation re-adds.
        let (id, key) = self
            .db(|s| Ok((s.next_job_id()?, s.next_queue_key()?)))
            .await?;
        let mut rec = JobRecord {
            id,
            engine: Engine::Aria2,
            kind,
            name,
            dir: dir.to_owned(),
            source,
            options: options.clone(),
            desired: if paused {
                Desired::Paused
            } else {
                Desired::Queued
            },
            state: if paused {
                JobState::Paused
            } else {
                JobState::Waiting
            },
            gid: None,
            info_hash: None,
            total_bytes: 0,
            completed_bytes: 0,
            uploaded_base: 0,
            uploaded_session: 0,
            added_at: now(),
            completed_at: None,
            queue_key: key,
            error: None,
            has_blob: blob.is_some(),
            extra: serde_json::Map::new(),
        };
        if uris.len() > 1 {
            rec.extra.insert("uris".into(), json!(uris));
        }
        // Note whether aria2's metadata copy (`<info hash>.torrent`) is already
        // there, so only one that appears later is ever removed.
        let hash = match (&blob, kind) {
            (Some(b), SourceKind::Torrent) => {
                dman_torrent::parse_torrent(b).ok().map(|m| m.info_hash)
            }
            (None, SourceKind::Magnet) => super::artefacts::magnet_hash(&rec.source),
            _ => None,
        };
        super::artefacts::note_torrent_file(&mut rec, hash);
        {
            let rec = rec.clone();
            let blob = blob.clone();
            self.db(move |s| {
                s.put_job(&rec)?;
                if let Some(b) = blob {
                    s.put_blob(rec.id, &b)?;
                }
                Ok(())
            })
            .await?;
        }

        let mut aopts = aopts.clone();
        super::seeding::apply_policy(&mut aopts, options, &self.torrents, kind);
        let aopts = &aopts;
        let added = match (kind, &blob) {
            (SourceKind::Torrent, Some(b)) => client
                .add_torrent(b, &[], aopts, None)
                .await
                .map(|g| vec![g]),
            (SourceKind::Metalink, Some(b)) => client.add_metalink(b, aopts, None).await,
            _ => client.add_uri(&uris, aopts, None).await.map(|g| vec![g]),
        };
        let gids = match added {
            Ok(g) if !g.is_empty() => g,
            Ok(_) => {
                let _ = self.db(move |s| s.purge(&[id])).await;
                return Err(RpcError::new(ErrorCode::General, "aria2 added nothing"));
            }
            Err(e) => {
                let _ = self.db(move |s| s.purge(&[id])).await;
                return Err(match e {
                    dman_aria2::Error::Rpc { message, .. } => {
                        RpcError::invalid(format!("aria2 rejected the source: {message}"))
                    }
                    other => engine_err(other),
                });
            }
        };

        // One job per gid (metalinks may describe several files).
        let mut records = Vec::with_capacity(gids.len());
        for (i, gid) in gids.iter().enumerate() {
            let mut r = rec.clone();
            if i > 0 {
                let (nid, nkey) = self
                    .db(|s| Ok((s.next_job_id()?, s.next_queue_key()?)))
                    .await?;
                r.id = nid;
                r.queue_key = nkey;
            }
            if gids.len() > 1 {
                r.extra.insert("metalink_index".into(), json!(i));
            }
            r.gid = Some(gid.as_str().to_owned());
            records.push(r);
        }
        let ids: Vec<JobId> = records.iter().map(|r| r.id).collect();
        {
            let recs = records.clone();
            let blob = blob.clone();
            self.db(move |s| {
                s.put_jobs(&recs)?;
                if let Some(b) = blob {
                    for r in recs.iter().skip(1) {
                        s.put_blob(r.id, &b)?;
                    }
                }
                Ok(())
            })
            .await?;
        }
        for r in records {
            let gid = r.gid.clone().unwrap_or_default();
            tracing::info!(id = %r.id, gid = %gid, kind = ?r.kind, "job added");
            let mut e = Entry::new(r);
            e.aria2 = Some(DownloadStatus::Paused);
            e.counters = Some(Counters::fresh(&gid));
            self.gids.insert(gid, e.rec.id);
            self.jobs.insert(e.rec.id, e);
        }
        Ok(ids)
    }
}
