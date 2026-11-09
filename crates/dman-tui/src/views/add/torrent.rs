//! A tiny bencode reader and `.torrent` summary (name, files, info-hash) for
//! the add preview. Only what the preview needs; the daemon does the real work.

use sha1::{Digest, Sha1};

const MAX_DEPTH: usize = 32;

/// A bencoded value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bencode {
    /// `i42e`
    Int(i64),
    /// `4:spam`
    Bytes(Vec<u8>),
    /// `l…e`
    List(Vec<Bencode>),
    /// `d…e` (keys in file order)
    Dict(Vec<(Vec<u8>, Bencode)>),
}

impl Bencode {
    fn get(&self, key: &str) -> Option<&Bencode> {
        match self {
            Bencode::Dict(d) => d.iter().find(|(k, _)| k == key.as_bytes()).map(|(_, v)| v),
            _ => None,
        }
    }

    fn int(&self) -> Option<i64> {
        match self {
            Bencode::Int(i) => Some(*i),
            _ => None,
        }
    }

    fn text(&self) -> Option<String> {
        match self {
            Bencode::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
            _ => None,
        }
    }
}

struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
    /// Byte range of the `info` value of the top-level dictionary.
    info: Option<(usize, usize)>,
}

impl Parser<'_> {
    fn peek(&self) -> Result<u8, String> {
        self.data
            .get(self.pos)
            .copied()
            .ok_or_else(|| "unexpected end of data".to_owned())
    }

    fn number(&mut self, end: u8) -> Result<i64, String> {
        let start = self.pos;
        while self.peek()? != end {
            self.pos += 1;
        }
        let s = std::str::from_utf8(&self.data[start..self.pos]).map_err(|e| e.to_string())?;
        self.pos += 1;
        s.parse::<i64>()
            .map_err(|_| format!("invalid number `{s}` at byte {start}"))
    }

    fn bytes(&mut self) -> Result<Vec<u8>, String> {
        let at = self.pos;
        let len = usize::try_from(self.number(b':')?)
            .map_err(|_| format!("negative length at byte {at}"))?;
        let end = self
            .pos
            .checked_add(len)
            .filter(|e| *e <= self.data.len())
            .ok_or_else(|| format!("string at byte {at} runs past the end"))?;
        let out = self.data[self.pos..end].to_vec();
        self.pos = end;
        Ok(out)
    }

    fn value(&mut self, depth: usize) -> Result<Bencode, String> {
        if depth > MAX_DEPTH {
            return Err("nested too deeply".into());
        }
        match self.peek()? {
            b'i' => {
                self.pos += 1;
                self.number(b'e').map(Bencode::Int)
            }
            b'l' => {
                self.pos += 1;
                let mut items = Vec::new();
                while self.peek()? != b'e' {
                    items.push(self.value(depth + 1)?);
                }
                self.pos += 1;
                Ok(Bencode::List(items))
            }
            b'd' => {
                self.pos += 1;
                let mut items = Vec::new();
                while self.peek()? != b'e' {
                    let key = self.bytes()?;
                    let start = self.pos;
                    let v = self.value(depth + 1)?;
                    if depth == 0 && key == b"info" {
                        self.info = Some((start, self.pos));
                    }
                    items.push((key, v));
                }
                self.pos += 1;
                Ok(Bencode::Dict(items))
            }
            b'0'..=b'9' => self.bytes().map(Bencode::Bytes),
            c => Err(format!("unexpected byte 0x{c:02x} at {}", self.pos)),
        }
    }
}

/// Decode one bencoded value; also returns the byte range of the top-level
/// `info` entry (for hashing).
///
/// # Errors
/// Malformed data, or trailing bytes.
pub fn decode(data: &[u8]) -> Result<(Bencode, Option<(usize, usize)>), String> {
    let mut p = Parser {
        data,
        pos: 0,
        info: None,
    };
    let v = p.value(0)?;
    if p.pos != data.len() {
        return Err(format!("{} trailing bytes", data.len() - p.pos));
    }
    Ok((v, p.info))
}

/// One file of a torrent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentFile {
    /// Path inside the torrent.
    pub path: String,
    /// Bytes.
    pub length: u64,
}

/// What the preview shows of a `.torrent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentMeta {
    /// Torrent name.
    pub name: String,
    /// Sum of file sizes.
    pub total: u64,
    /// Files in order (1-based index = position + 1).
    pub files: Vec<TorrentFile>,
    /// SHA-1 of the bencoded `info` dictionary, lower-case hex.
    pub info_hash: String,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Summarise a `.torrent` file.
///
/// # Errors
/// Not bencode, or no usable `info` dictionary.
pub fn parse_torrent(data: &[u8]) -> Result<TorrentMeta, String> {
    let (root, span) = decode(data)?;
    let info = root.get("info").ok_or("no `info` dictionary")?;
    let (a, b) = span.ok_or("no `info` dictionary")?;
    let name = info
        .get("name.utf-8")
        .or_else(|| info.get("name"))
        .and_then(Bencode::text)
        .ok_or("no name")?;
    let files = match info.get("files") {
        Some(Bencode::List(list)) => list
            .iter()
            .map(|f| {
                let length = f
                    .get("length")
                    .and_then(Bencode::int)
                    .and_then(|l| u64::try_from(l).ok())
                    .ok_or("a file has no length")?;
                let parts = match f.get("path.utf-8").or_else(|| f.get("path")) {
                    Some(Bencode::List(p)) => p.iter().filter_map(Bencode::text).collect(),
                    _ => Vec::new(),
                };
                if parts.is_empty() {
                    return Err("a file has no path");
                }
                Ok(TorrentFile {
                    path: parts.join("/"),
                    length,
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            let length = info
                .get("length")
                .and_then(Bencode::int)
                .and_then(|l| u64::try_from(l).ok())
                .ok_or("no length or files")?;
            vec![TorrentFile {
                path: name.clone(),
                length,
            }]
        }
    };
    Ok(TorrentMeta {
        name,
        total: files.iter().map(|f| f.length).sum(),
        files,
        info_hash: hex(&Sha1::digest(&data[a..b])),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_values() {
        let (v, _) = decode(b"d3:bari-3e3:fool1:a2:bcee").unwrap();
        assert_eq!(v.get("bar"), Some(&Bencode::Int(-3)));
        assert_eq!(
            v.get("foo"),
            Some(&Bencode::List(vec![
                Bencode::Bytes(b"a".to_vec()),
                Bencode::Bytes(b"bc".to_vec())
            ]))
        );
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            &b""[..],
            b"i12",
            b"5:abc",
            b"d3:fooe",
            b"x",
            b"i1ei2e",
            b"-1:a",
        ] {
            assert!(decode(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
        let deep = [b"l".repeat(100), b"e".repeat(100)].concat();
        assert!(decode(&deep).unwrap_err().contains("deeply"));
    }

    #[test]
    fn single_file_torrent_and_info_hash() {
        let data = b"d4:infod6:lengthi5e4:name3:foo12:piece lengthi16384e6:pieces20:AAAAAAAAAAAAAAAAAAAAee";
        let m = parse_torrent(data).unwrap();
        assert_eq!(m.name, "foo");
        assert_eq!(m.total, 5);
        assert_eq!(m.files.len(), 1);
        // sha1sum of the exact bytes of the `info` dictionary.
        assert_eq!(m.info_hash, "a6747b41249b13719eb36ec456c31b36de2e7c50");
    }

    #[test]
    fn multi_file_torrent() {
        let data = b"d4:infod5:filesld6:lengthi3e4:pathl1:a1:xeed6:lengthi4e4:pathl1:beee4:name3:dir12:piece lengthi16384e6:pieces40:AAAAAAAAAAAAAAAAAAAABBBBBBBBBBBBBBBBBBBBee";
        let m = parse_torrent(data).unwrap();
        assert_eq!(m.name, "dir");
        assert_eq!(m.total, 7);
        assert_eq!(m.files[0].path, "a/x");
        assert_eq!(m.files[1].path, "b");
        assert_eq!(m.info_hash, "21e7f7434d48f160bd729e70246fa7758a8bbd6f");
    }

    #[test]
    fn not_a_torrent() {
        assert!(parse_torrent(b"d3:fooi1ee").unwrap_err().contains("info"));
        assert!(parse_torrent(b"<html>").is_err());
    }
}
