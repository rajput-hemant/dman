use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Downloading,
    Finished,
    Error,
    Other(String),
}

/// Position inside a playlist download ("Downloading item N of M").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemProgress {
    pub index: u32,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub downloaded: u64,
    /// `total_bytes`, else `total_bytes_estimate`.
    pub total: Option<u64>,
    /// Bytes per second.
    pub speed: Option<f64>,
    /// Seconds.
    pub eta: Option<u64>,
    pub status: Status,
    pub filename: Option<String>,
    pub fragment_index: Option<u64>,
    pub fragment_count: Option<u64>,
    pub item: Option<ItemProgress>,
}

/// Parse one `--progress-template "download:%(progress)j"` line. Returns `None` for anything
/// that is not a JSON object with a `status`.
pub fn parse_progress_line(line: &str) -> Option<Progress> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    let status = match v.get("status")?.as_str()? {
        "downloading" => Status::Downloading,
        "finished" => Status::Finished,
        "error" => Status::Error,
        other => Status::Other(other.to_string()),
    };
    let num = |k: &str| v.get(k).and_then(Value::as_f64).filter(|n| *n >= 0.0);
    let int = |k: &str| num(k).map(|n| n as u64);
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Progress {
        downloaded: int("downloaded_bytes").unwrap_or(0),
        total: int("total_bytes").or_else(|| int("total_bytes_estimate")),
        speed: num("speed"),
        eta: int("eta"),
        status,
        filename: text("filename").or_else(|| text("tmpfilename")),
        fragment_index: int("fragment_index"),
        fragment_count: int("fragment_count"),
        item: None,
    })
}

/// Parse "[download] Downloading item 3 of 10".
pub fn parse_item_line(line: &str) -> Option<ItemProgress> {
    let rest = line.trim().strip_prefix("[download] Downloading item ")?;
    let (n, m) = rest.split_once(" of ")?;
    Some(ItemProgress {
        index: n.trim().parse().ok()?,
        count: m.trim().parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spike_line() {
        let l = r#"{"status": "downloading", "downloaded_bytes": 4189002, "total_bytes": 38337223, "tmpfilename": "/d/a.mp4.part", "filename": "/d/a.mp4", "eta": 16, "speed": 2093181.0, "elapsed": 2.0}"#;
        let p = parse_progress_line(l).unwrap();
        assert_eq!(p.downloaded, 4189002);
        assert_eq!(p.total, Some(38337223));
        assert_eq!(p.eta, Some(16));
        assert_eq!(p.speed, Some(2093181.0));
        assert_eq!(p.status, Status::Downloading);
        assert_eq!(p.filename.as_deref(), Some("/d/a.mp4"));
        assert_eq!(p.item, None);
    }

    #[test]
    fn early_nulls_and_estimates_and_fragments() {
        let l = r#"{"status":"downloading","downloaded_bytes":0,"total_bytes":null,"total_bytes_estimate":1000.5,"eta":null,"speed":null,"fragment_index":2,"fragment_count":10}"#;
        let p = parse_progress_line(l).unwrap();
        assert_eq!(p.total, Some(1000));
        assert_eq!(p.eta, None);
        assert_eq!(p.speed, None);
        assert_eq!(p.fragment_index, Some(2));
        assert_eq!(p.fragment_count, Some(10));
    }

    #[test]
    fn finished_and_non_progress() {
        let p = parse_progress_line(r#"{"status":"finished","downloaded_bytes":5}"#).unwrap();
        assert_eq!(p.status, Status::Finished);
        assert!(parse_progress_line("[download] Destination: x").is_none());
        assert!(parse_progress_line("{broken").is_none());
        assert!(parse_progress_line(r#"{"no":"status"}"#).is_none());
    }

    #[test]
    fn item_lines() {
        assert_eq!(
            parse_item_line("[download] Downloading item 3 of 10"),
            Some(ItemProgress {
                index: 3,
                count: 10
            })
        );
        assert_eq!(parse_item_line("[download] Downloading playlist: x"), None);
    }
}
