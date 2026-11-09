//! RSS 2.0 / Atom / Torznab parsing with quick-xml.
//!
//! Elements are matched by *local* name, so `nyaa:seeders`, `torznab:attr`
//! and plain `seeders` are all handled the same way.

use quick_xml::{Reader, events::Event};

use crate::{error::ProviderError, result::SearchResult, util};

/// A `<link>` / `<enclosure>` element's attributes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Link {
    pub href: String,
    pub rel: String,
    pub mime: String,
    pub length: Option<String>,
}

/// One `<item>` / `<entry>` with everything we care about, unmapped.
#[derive(Debug, Clone, Default)]
pub struct RawItem {
    /// Direct child elements: lowercase local name -> text (first occurrence wins).
    pub fields: Vec<(String, String)>,
    /// `<link href=..>` (Atom) elements.
    pub links: Vec<Link>,
    /// `<enclosure url=.. length=.. type=..>`.
    pub enclosure: Option<Link>,
    /// `<torznab:attr name=.. value=..>` / `<newznab:attr ..>`.
    pub attrs: Vec<(String, String)>,
    pub categories: Vec<String>,
}

impl RawItem {
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, v)| k == name && !v.trim().is_empty())
            .map(|(_, v)| v.trim())
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, v)| k.eq_ignore_ascii_case(name) && !v.is_empty())
            .map(|(_, v)| v.as_str())
    }
}

fn xml_err(r: &Reader<&[u8]>, e: impl std::fmt::Display) -> ProviderError {
    ProviderError::Parse(format!("invalid XML at byte {}: {e}", r.buffer_position()))
}

fn attrs_of(e: &quick_xml::events::BytesStart<'_>) -> Vec<(String, String)> {
    e.attributes()
        .flatten()
        .filter_map(|a| {
            let key = a.key.local_name().as_ref().to_ascii_lowercase();
            let val = a
                .normalized_value(quick_xml::XmlVersion::default())
                .ok()?
                .into_owned();
            Some((key, val))
        })
        .collect()
}

fn get<'a>(attrs: &'a [(String, String)], key: &str) -> &'a str {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map_or("", |(_, v)| v.as_str())
}

fn local(e: &quick_xml::events::BytesStart<'_>) -> String {
    e.local_name().as_ref().to_ascii_lowercase()
}

/// Record a finished (or empty) direct child element of an item.
fn record(item: &mut RawItem, name: &str, attrs: &[(String, String)], text: String) {
    match name {
        "attr" if attrs.iter().any(|(k, _)| k == "name") => {
            item.attrs.push((
                get(attrs, "name").to_string(),
                get(attrs, "value").to_string(),
            ));
        }
        "enclosure" => {
            item.enclosure = Some(Link {
                href: get(attrs, "url").to_string(),
                rel: "enclosure".into(),
                mime: get(attrs, "type").to_string(),
                length: Some(get(attrs, "length").to_string()).filter(|s| !s.is_empty()),
            });
        }
        "link" if attrs.iter().any(|(k, _)| k == "href") => item.links.push(Link {
            href: get(attrs, "href").to_string(),
            rel: get(attrs, "rel").to_string(),
            mime: get(attrs, "type").to_string(),
            length: Some(get(attrs, "length").to_string()).filter(|s| !s.is_empty()),
        }),
        "category" => {
            let t = if text.trim().is_empty() {
                get(attrs, "term").to_string()
            } else {
                text.trim().to_string()
            };
            if !t.is_empty() {
                item.categories.push(t);
            }
        }
        _ => item.fields.push((name.to_string(), text)),
    }
}

type Child = (String, Vec<(String, String)>, String);

/// Parse all items of an RSS/Atom/Torznab document.
///
/// Fails when the document is not XML, not a feed (e.g. an HTML challenge
/// page), or is a Torznab `<error code=.. description=..>` response.
pub fn parse_items(xml: &str) -> Result<Vec<RawItem>, ProviderError> {
    let mut reader = Reader::from_str(xml);
    let mut items = Vec::new();
    let mut root: Option<String> = None;
    let mut depth = 0usize;
    let mut item_depth = 0usize; // depth of the open <item>, 0 = none
    let mut cur: Option<RawItem> = None;
    let mut child: Option<Child> = None;

    loop {
        let ev = reader.read_event().map_err(|e| xml_err(&reader, e))?;
        match ev {
            Event::Start(e) => {
                depth += 1;
                let name = local(&e);
                if root.is_none() {
                    root = Some(name.clone());
                    if name == "error" {
                        let a = attrs_of(&e);
                        return Err(remote_error(&a));
                    }
                } else if cur.is_none() && (name == "item" || name == "entry") {
                    cur = Some(RawItem::default());
                    item_depth = depth;
                } else if cur.is_some() && depth == item_depth + 1 {
                    child = Some((name, attrs_of(&e), String::new()));
                }
            }
            Event::Empty(e) => {
                let name = local(&e);
                if root.is_none() {
                    root = Some(name.clone());
                    if name == "error" {
                        return Err(remote_error(&attrs_of(&e)));
                    }
                } else if let Some(item) = cur.as_mut()
                    && depth == item_depth
                {
                    record(item, &name, &attrs_of(&e), String::new());
                }
            }
            Event::Text(t) => {
                if let Some((_, _, buf)) = child.as_mut()
                    && depth == item_depth + 1
                {
                    buf.push_str(&t.xml10_content());
                }
            }
            Event::CData(t) => {
                if let Some((_, _, buf)) = child.as_mut()
                    && depth == item_depth + 1
                {
                    buf.push_str(&t.xml10_content());
                }
            }
            Event::GeneralRef(r) => {
                if let Some((_, _, buf)) = child.as_mut()
                    && depth == item_depth + 1
                {
                    if let Ok(Some(c)) = r.resolve_char_ref() {
                        buf.push(c);
                    } else {
                        match &*r as &str {
                            "amp" => buf.push('&'),
                            "lt" => buf.push('<'),
                            "gt" => buf.push('>'),
                            "quot" => buf.push('"'),
                            "apos" => buf.push('\''),
                            _ => {}
                        }
                    }
                }
            }
            Event::End(_) => {
                if cur.is_some() && depth == item_depth + 1 {
                    if let (Some(item), Some((name, attrs, text))) = (cur.as_mut(), child.take()) {
                        record(item, &name, &attrs, text);
                    }
                } else if cur.is_some() && depth == item_depth {
                    items.extend(cur.take());
                    item_depth = 0;
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    match root.as_deref() {
        Some("rss" | "feed" | "rdf") => Ok(items),
        Some(other) => Err(ProviderError::Parse(format!(
            "not an RSS/Atom feed (root element <{other}>)"
        ))),
        None => Err(ProviderError::Parse("empty or non-XML response".into())),
    }
}

fn remote_error(a: &[(String, String)]) -> ProviderError {
    let code = get(a, "code");
    let desc = get(a, "description");
    ProviderError::Remote(format!("code {code}: {desc}").replace("code : ", ""))
}

fn classify(url: &str, r: &mut SearchResult, prefer_torrent: bool) {
    let u = url.trim();
    if u.is_empty() {
        return;
    }
    let lower = u.to_ascii_lowercase();
    let path = lower.split(['?', '#']).next().unwrap_or("");
    if lower.starts_with("magnet:") {
        r.magnet.get_or_insert_with(|| u.to_string());
    } else if path.ends_with(".torrent") || path.contains("/download") || prefer_torrent {
        r.torrent_url.get_or_insert_with(|| u.to_string());
    } else if lower.starts_with("http") {
        r.detail_url.get_or_insert_with(|| u.to_string());
    }
}

/// Map a raw item to a normalized result (works for RSS, Atom and Torznab).
pub fn item_to_result(item: &RawItem) -> SearchResult {
    let mut r = SearchResult {
        title: item.field("title").unwrap_or("").to_string(),
        ..SearchResult::default()
    };

    if let Some(e) = &item.enclosure {
        let is_torrent =
            e.mime.contains("bittorrent") || e.href.to_ascii_lowercase().contains(".torrent");
        classify(&e.href, &mut r, is_torrent);
    }
    for l in &item.links {
        match l.rel.as_str() {
            "enclosure" => classify(&l.href, &mut r, l.mime.contains("bittorrent")),
            _ => classify(&l.href, &mut r, false),
        }
    }
    if let Some(l) = item.field("link") {
        classify(l, &mut r, false);
    }
    if let Some(g) = item.field("guid").or_else(|| item.field("id")) {
        classify(g, &mut r, false);
    }
    if let Some(m) = item.attr("magneturl").or_else(|| item.field("magneturl")) {
        r.magnet = Some(m.to_string());
    }

    r.info_hash = item
        .attr("infohash")
        .or_else(|| item.field("infohash"))
        .or_else(|| item.field("info_hash"))
        .map(str::to_string);

    r.size = item
        .attr("size")
        .or_else(|| item.field("size"))
        .or_else(|| item.field("contentlength"))
        .or_else(|| item.enclosure.as_ref().and_then(|e| e.length.as_deref()))
        .or_else(|| {
            item.links
                .iter()
                .find(|l| l.rel == "enclosure")
                .and_then(|l| l.length.as_deref())
        })
        .and_then(util::parse_size)
        .filter(|&s| s > 0);

    r.seeders = item
        .attr("seeders")
        .or_else(|| item.field("seeders"))
        .and_then(util::parse_count);
    let peers = item
        .attr("peers")
        .or_else(|| item.field("peers"))
        .and_then(util::parse_count);
    // Torznab: `peers` = seeders + leechers.
    r.leechers = item
        .attr("leechers")
        .or_else(|| item.field("leechers"))
        .and_then(util::parse_count)
        .or_else(|| peers.map(|p| p.saturating_sub(r.seeders.unwrap_or(0))));

    r.date = ["pubdate", "published", "updated", "date"]
        .iter()
        .find_map(|k| item.field(k))
        .and_then(util::parse_date);

    r.category = item.categories.first().cloned();
    r
}

/// Parse an RSS/Atom/Torznab document into results.
pub fn parse(xml: &str) -> Result<Vec<SearchResult>, ProviderError> {
    Ok(parse_items(xml)?.iter().map(item_to_result).collect())
}
