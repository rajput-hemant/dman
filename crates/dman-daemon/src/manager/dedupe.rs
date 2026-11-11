//! Duplicate detection when adding: a source whose info hash (magnets,
//! torrents) or URL (HTTP, FTP, video pages) matches a job that is not removed
//! is refused, naming that job. A duplicate torrent gets the new source's
//! trackers instead, as other torrent clients do.

use base64::Engine as _;
use dman_aria2::{Gid, Options};
use dman_proto::{AddSource, DuplicateInfo, JobState, RpcError, SourceKind};
use dman_store::JobRecord;

use super::Manager;

/// What identifies a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Key {
    /// Lower-case hex BitTorrent info hash (v1).
    Hash(String),
    /// A normalised URL.
    Url(String),
}

/// The BitTorrent v1 info hash of a magnet link, lower-case hex. Accepts the
/// 40-digit hex and the 32-character base32 forms.
pub(crate) fn magnet_btih(uri: &str) -> Option<String> {
    let u = uri.trim();
    if !u.get(..7)?.eq_ignore_ascii_case("magnet:") {
        return None;
    }
    let query = u[7..].trim_start_matches('?');
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        if !k.eq_ignore_ascii_case("xt") {
            return None;
        }
        let v = v
            .get(..9)
            .filter(|p| p.eq_ignore_ascii_case("urn:btih:"))
            .map(|_| &v[9..])?;
        if v.len() == 40 && v.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(v.to_ascii_lowercase());
        }
        if v.len() == 32 {
            return base32_to_hex(v);
        }
        None
    })
}

/// RFC 4648 base32 (no padding) to lower-case hex.
fn base32_to_hex(s: &str) -> Option<String> {
    let mut bits: u64 = 0;
    let mut n = 0u32;
    let mut out = String::with_capacity(s.len() * 5 / 4);
    for c in s.bytes() {
        let v = match c.to_ascii_uppercase() {
            c @ b'A'..=b'Z' => c - b'A',
            c @ b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        bits = (bits << 5) | u64::from(v);
        n += 5;
        if n >= 8 {
            n -= 8;
            out.push_str(&format!("{:02x}", (bits >> n) & 0xff));
        }
    }
    Some(out)
}

/// Percent-decoding of a query value (`+` is a space).
fn percent_decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("v={s}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// The `tr=` trackers of a magnet link.
pub(super) fn magnet_trackers(uri: &str) -> Vec<String> {
    let Some((_, query)) = uri.trim().split_once('?') else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for kv in query.split('&') {
        if let Some((k, v)) = kv.split_once('=')
            && (k.eq_ignore_ascii_case("tr") || k.to_ascii_lowercase().starts_with("tr."))
        {
            let t = percent_decode(v).trim().to_owned();
            if !t.is_empty() && !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

/// The trackers of a `bt-tracker` value (comma or whitespace separated).
pub(super) fn split_trackers(v: &str) -> Vec<String> {
    v.split([',', '\n', '\r', ' ', '\t'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A URL in a form where trivial differences (case of scheme and host, a
/// default port, the fragment) do not matter.
pub(super) fn normalize_url(u: &str) -> String {
    let t = u.trim();
    match url::Url::parse(t) {
        Ok(mut p) => {
            p.set_fragment(None);
            p.to_string()
        }
        Err(_) => t.to_owned(),
    }
}

/// The decoded bytes of a `.torrent` source.
fn torrent_bytes(src: &AddSource) -> Option<Vec<u8>> {
    match src {
        AddSource::Torrent { data_base64, .. } => base64::engine::general_purpose::STANDARD
            .decode(data_base64.trim())
            .ok(),
        _ => None,
    }
}

/// What identifies `src`, if anything does (metalinks: nothing).
pub(super) fn key_of(src: &AddSource) -> Option<Key> {
    match src {
        AddSource::Uris { uris } => {
            let first = uris.iter().map(|u| u.trim()).find(|u| !u.is_empty())?;
            if first.get(..7)?.eq_ignore_ascii_case("magnet:") {
                magnet_btih(first).map(Key::Hash)
            } else {
                Some(Key::Url(normalize_url(first)))
            }
        }
        AddSource::Torrent { .. } => {
            let b = torrent_bytes(src)?;
            dman_torrent::parse_torrent(&b)
                .ok()
                .map(|m| Key::Hash(m.info_hash))
        }
        AddSource::Video { url, .. } => Some(Key::Url(normalize_url(url))),
        AddSource::Metalink { .. } => None,
    }
}

/// The trackers a new source brings along.
pub(super) fn trackers_of(src: &AddSource) -> Vec<String> {
    match src {
        AddSource::Uris { uris } => uris.first().map(|u| magnet_trackers(u)).unwrap_or_default(),
        AddSource::Torrent { .. } => torrent_bytes(src)
            .map(|b| dman_torrent::trackers(&b))
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The info hash of an existing job, when known.
pub(super) fn job_hash(rec: &JobRecord) -> Option<String> {
    if let Some(h) = &rec.info_hash
        && h.len() == 40
    {
        return Some(h.to_ascii_lowercase());
    }
    if let Some(h) = rec
        .extra
        .get(super::artefacts::KEY)
        .and_then(|v| v.get("hash"))
        .and_then(serde_json::Value::as_str)
    {
        return Some(h.to_ascii_lowercase());
    }
    magnet_btih(&rec.source)
}

/// Whether `rec` downloads `url` (its source or one of its mirrors).
pub(super) fn job_has_url(rec: &JobRecord, url: &str) -> bool {
    if normalize_url(&rec.source) == url {
        return true;
    }
    rec.extra
        .get("uris")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|a| {
            a.iter()
                .filter_map(serde_json::Value::as_str)
                .any(|u| normalize_url(u) == url)
        })
}

/// The refusal for a duplicate.
pub(super) fn duplicate_error(info: &DuplicateInfo, running: bool) -> RpcError {
    let state = state_word(info.state);
    let mut msg = format!("already in your downloads as job {} ({state})", info.id);
    match info.trackers_added {
        0 => {}
        n => {
            let s = if n == 1 { "" } else { "s" };
            msg.push_str(&format!("; added {n} new tracker{s} to it"));
            if running {
                msg.push_str(" (used from its next start)");
            }
        }
    }
    let mut e = RpcError::refused(msg);
    e.data = Some(info.to_data());
    e
}

fn state_word(s: JobState) -> &'static str {
    match s {
        JobState::Waiting => "waiting",
        JobState::Active => "downloading",
        JobState::Seeding => "seeding",
        JobState::Paused => "paused",
        JobState::Complete => "complete",
        JobState::Error => "failed",
        JobState::Removed => "removed",
    }
}

impl Manager {
    /// Refuses `src` when an existing, not removed job already has it. A
    /// duplicate torrent first receives the trackers of `src` it lacks.
    pub(super) async fn check_duplicate(&mut self, src: &AddSource) -> Result<(), RpcError> {
        let Some(key) = key_of(src) else {
            return Ok(());
        };
        let found = self
            .jobs
            .values()
            .filter(|e| e.rec.state != JobState::Removed)
            .filter(|e| match &key {
                Key::Hash(h) => job_hash(&e.rec).as_deref() == Some(h.as_str()),
                Key::Url(u) => job_has_url(&e.rec, u),
            })
            .map(|e| e.rec.id)
            .min();
        let Some(id) = found else {
            return Ok(());
        };
        let mut added = 0;
        let mut running = false;
        if matches!(key, Key::Hash(_)) {
            (added, running) = self.merge_trackers(id, &trackers_of(src)).await;
        }
        let Some(e) = self.jobs.get(&id) else {
            return Ok(());
        };
        let info = DuplicateInfo {
            id,
            state: e.rec.state,
            trackers_added: added,
        };
        tracing::info!(%id, trackers_added = added, "refused a duplicate add");
        Err(duplicate_error(&info, running))
    }

    /// Adds the trackers of `new` that job `id` does not have yet. Returns how
    /// many were added and whether the job is running in the engine (aria2
    /// then uses them from the next start of the download).
    async fn merge_trackers(&mut self, id: dman_proto::JobId, new: &[String]) -> (u32, bool) {
        let Some(e) = self.jobs.get(&id) else {
            return (0, false);
        };
        if new.is_empty() || !matches!(e.rec.kind, SourceKind::Torrent | SourceKind::Magnet) {
            return (0, false);
        }
        let configured = e.rec.options.engine_options.get("bt-tracker").cloned();
        let mut known = configured
            .as_deref()
            .map(split_trackers)
            .unwrap_or_default();
        known.extend(magnet_trackers(&e.rec.source));
        if e.rec.has_blob
            && let Ok(Some(blob)) = self.db(move |s| s.get_blob(id)).await
        {
            known.extend(dman_torrent::trackers(&blob));
        }
        let fresh: Vec<String> = new
            .iter()
            .filter(|t| !t.contains(['\n', '\r', '\0', ',']) && !known.contains(t))
            .fold(Vec::new(), |mut v, t| {
                if !v.contains(t) {
                    v.push(t.clone());
                }
                v
            });
        if fresh.is_empty() {
            return (0, false);
        }
        let mut list = configured
            .as_deref()
            .map(split_trackers)
            .unwrap_or_default();
        list.extend(fresh.iter().cloned());
        let value = list.join(",");
        let Some(e) = self.jobs.get_mut(&id) else {
            return (0, false);
        };
        e.rec
            .options
            .engine_options
            .insert("bt-tracker".into(), value.clone());
        let running =
            e.running_in_engine() || matches!(e.rec.state, JobState::Active | JobState::Seeding);
        let gid = e.rec.gid.clone().filter(|_| e.aria2.is_some());
        if let (Some(c), Some(gid)) = (self.client.clone(), gid) {
            // A running download accepts the change and applies it when it
            // next starts; a paused or waiting one applies it at once.
            let opts = Options::new().with("bt-tracker", value);
            if let Err(err) = c.change_option(&Gid::new(gid), &opts).await {
                tracing::debug!(%id, "could not pass the new trackers to aria2: {err}");
            }
        }
        self.mark(id, true);
        self.flush(true).await;
        (u32::try_from(fresh.len()).unwrap_or(u32::MAX), running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn magnet_hashes_hex_and_base32() {
        assert_eq!(
            magnet_btih(&format!("magnet:?dn=x&xt=urn:btih:{}", HEX.to_uppercase())),
            Some(HEX.to_owned())
        );
        // base32 of 0x00..0x13 (20 bytes).
        let b32 = "AAAQEAYEAUDAOCAJBIFQYDIOB4IBCEQT";
        assert_eq!(
            magnet_btih(&format!("MAGNET:?XT=URN:BTIH:{b32}")),
            Some("000102030405060708090a0b0c0d0e0f10111213".to_owned())
        );
        assert_eq!(magnet_btih("magnet:?xt=urn:btih:short"), None);
        assert_eq!(magnet_btih("http://x/?xt=urn:btih:{HEX}"), None);
    }

    #[test]
    fn magnet_trackers_are_decoded() {
        let m = format!(
            "magnet:?xt=urn:btih:{HEX}&tr=udp%3A%2F%2Fa%3A1&dn=x&tr=udp://b:2&tr=udp%3A%2F%2Fa%3A1"
        );
        assert_eq!(magnet_trackers(&m), ["udp://a:1", "udp://b:2"]);
        assert!(magnet_trackers("magnet:?xt=urn:btih:x").is_empty());
    }

    #[test]
    fn keys_of_sources() {
        let m = AddSource::Uris {
            uris: vec![format!("magnet:?xt=urn:btih:{HEX}")],
        };
        assert_eq!(key_of(&m), Some(Key::Hash(HEX.into())));
        let h = AddSource::Uris {
            uris: vec!["HTTP://Example.org:80/a.iso#x".into()],
        };
        assert_eq!(
            key_of(&h),
            Some(Key::Url("http://example.org/a.iso".into()))
        );
        let v = AddSource::Video {
            url: "https://youtu.be/x".into(),
            video: Box::default(),
        };
        assert_eq!(key_of(&v), Some(Key::Url("https://youtu.be/x".into())));
        let ml = AddSource::Metalink {
            name: "a.meta4".into(),
            data_base64: String::new(),
        };
        assert_eq!(key_of(&ml), None);
        let t = b"d8:announce5:udp:a4:infod6:lengthi1e4:name1:x12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee";
        let src = AddSource::Torrent {
            name: "x.torrent".into(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(t),
        };
        let hash = dman_torrent::parse_torrent(t).unwrap().info_hash;
        assert_eq!(key_of(&src), Some(Key::Hash(hash)));
        assert_eq!(trackers_of(&src), ["udp:a"]);
    }

    #[test]
    fn duplicate_message_names_the_job_and_state() {
        let info = DuplicateInfo {
            id: dman_proto::JobId(12),
            state: JobState::Paused,
            trackers_added: 0,
        };
        let e = duplicate_error(&info, false);
        assert_eq!(e.code, dman_proto::ErrorCode::Refused);
        assert_eq!(e.message, "already in your downloads as job 12 (paused)");
        assert_eq!(DuplicateInfo::from_error(&e), Some(info.clone()));
        let merged = DuplicateInfo {
            trackers_added: 2,
            state: JobState::Seeding,
            ..info
        };
        assert_eq!(
            duplicate_error(&merged, true).message,
            "already in your downloads as job 12 (seeding); added 2 new trackers to it (used from its next start)"
        );
    }
}
