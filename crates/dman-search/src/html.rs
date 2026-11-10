//! HTML scraping provider: CSS selectors applied per result row.
//!
//! Selector strings are `css` (text content) or `css @attr` (attribute value);
//! an empty css part (`@href`) addresses the row element itself.

use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::{def::HtmlSpec, error::ProviderError, result::SearchResult, util};

/// A compiled `css [@attr]` selector.
#[derive(Debug, Clone)]
pub struct Sel {
    pub raw: String,
    css: Option<Selector>,
    attr: Option<String>,
}

/// Split `css @attr` into its parts without compiling.
pub fn split_sel(raw: &str) -> (&str, Option<&str>) {
    let raw = raw.trim();
    if let Some(a) = raw.strip_prefix('@')
        && is_attr_name(a)
    {
        return ("", Some(a));
    }
    if let Some((css, attr)) = raw.rsplit_once(" @")
        && is_attr_name(attr.trim())
    {
        return (css.trim(), Some(attr.trim()));
    }
    (raw, None)
}

fn is_attr_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
}

impl Sel {
    /// Compile, reporting selector syntax errors with context.
    pub fn compile(raw: &str) -> Result<Self, String> {
        let (css, attr) = split_sel(raw);
        let css_sel = if css.is_empty() {
            if attr.is_none() {
                return Err("empty selector".into());
            }
            None
        } else {
            Some(Selector::parse(css).map_err(|e| format!("bad CSS selector `{css}`: {e}"))?)
        };
        Ok(Self {
            raw: raw.trim().to_string(),
            css: css_sel,
            attr: attr.map(str::to_string),
        })
    }

    /// First match's text (or attribute) inside `scope`.
    pub fn extract(&self, scope: ElementRef<'_>) -> Option<String> {
        let target = match &self.css {
            Some(s) => scope.select(s).next()?,
            None => scope,
        };
        let v = match &self.attr {
            Some(a) => target.value().attr(a)?.trim().to_string(),
            None => util::collapse_ws(&target.text().collect::<Vec<_>>().join(" ")),
        };
        (!v.is_empty()).then_some(v)
    }
}

/// All selectors of an [`HtmlSpec`], compiled.
#[derive(Debug, Clone)]
pub struct Compiled {
    pub row: Selector,
    pub row_raw: String,
    pub title: Sel,
    pub magnet: Option<Sel>,
    pub info_hash: Option<Sel>,
    pub detail_link: Option<Sel>,
    pub size: Option<Sel>,
    pub seeders: Option<Sel>,
    pub leechers: Option<Sel>,
    pub date: Option<Sel>,
    pub detail_magnet: Option<Sel>,
}

impl Compiled {
    /// Compile every selector; the error names the offending field.
    pub fn compile(spec: &HtmlSpec) -> Result<Self, (&'static str, String)> {
        fn opt(
            name: &'static str,
            v: &Option<String>,
        ) -> Result<Option<Sel>, (&'static str, String)> {
            v.as_deref()
                .map(|s| Sel::compile(s).map_err(|e| (name, e)))
                .transpose()
        }
        let row = Selector::parse(spec.row.trim())
            .map_err(|e| ("row", format!("bad CSS selector `{}`: {e}", spec.row)))?;
        Ok(Self {
            row,
            row_raw: spec.row.clone(),
            title: Sel::compile(&spec.title).map_err(|e| ("title", e))?,
            magnet: opt("magnet", &spec.magnet)?,
            info_hash: opt("info_hash", &spec.info_hash)?,
            detail_link: opt("detail_link", &spec.detail_link)?,
            size: opt("size", &spec.size)?,
            seeders: opt("seeders", &spec.seeders)?,
            leechers: opt("leechers", &spec.leechers)?,
            date: opt("date", &spec.date)?,
            detail_magnet: opt("detail_magnet", &spec.detail_magnet)?,
        })
    }
}

fn resolve(base: &Url, href: &str) -> String {
    if href.starts_with("magnet:") {
        return href.to_string();
    }
    base.join(href)
        .map_or_else(|_| href.to_string(), |u| u.to_string())
}

/// Parse a search-result page. `base` is the page URL (relative links are
/// resolved against it). With `strict`, zero matching rows is an error.
pub fn parse(
    spec: &HtmlSpec,
    body: &str,
    base: &Url,
    strict: bool,
) -> Result<Vec<SearchResult>, ProviderError> {
    let c =
        Compiled::compile(spec).map_err(|(f, e)| ProviderError::Definition(format!("{f}: {e}")))?;
    let doc = Html::parse_document(body);
    let rows: Vec<ElementRef<'_>> = doc.select(&c.row).collect();
    if rows.is_empty() {
        if strict {
            return Err(ProviderError::Parse(format!(
                "row selector `{}` matched 0 elements (page is {} bytes; the selector may be outdated or the site served a block/challenge page)",
                c.row_raw,
                body.len()
            )));
        }
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let Some(title) = c.title.extract(*row) else {
            continue;
        };
        let mut r = SearchResult {
            title,
            ..SearchResult::default()
        };
        r.magnet = c.magnet.as_ref().and_then(|s| s.extract(*row));
        r.info_hash = c
            .info_hash
            .as_ref()
            .and_then(|s| s.extract(*row))
            .and_then(|t| util::find_info_hash(&t));
        r.detail_url = c
            .detail_link
            .as_ref()
            .and_then(|s| s.extract(*row))
            .map(|h| resolve(base, &h));
        r.size = c
            .size
            .as_ref()
            .and_then(|s| s.extract(*row))
            .and_then(|t| util::parse_size(&t));
        r.seeders = c
            .seeders
            .as_ref()
            .and_then(|s| s.extract(*row))
            .and_then(|t| util::parse_count(&t));
        r.leechers = c
            .leechers
            .as_ref()
            .and_then(|s| s.extract(*row))
            .and_then(|t| util::parse_count(&t));
        r.date = c.date.as_ref().and_then(|s| s.extract(*row)).and_then(|t| {
            util::parse_date_format(&t, spec.date_format.as_deref().unwrap_or("auto"))
        });
        out.push(r);
    }
    if out.is_empty() {
        return Err(ProviderError::Parse(format!(
            "title selector `{}` matched nothing in any of the {} rows",
            c.title.raw,
            rows.len()
        )));
    }
    Ok(out)
}

/// Extract the magnet link from a detail page (`detail_magnet` selector).
pub fn parse_detail_magnet(spec: &HtmlSpec, body: &str) -> Result<String, ProviderError> {
    let raw = spec
        .detail_magnet
        .as_deref()
        .ok_or_else(|| ProviderError::Definition("detail_magnet is not configured".into()))?;
    let sel = Sel::compile(raw).map_err(ProviderError::Definition)?;
    let doc = Html::parse_document(body);
    sel.extract(doc.root_element())
        .filter(|m| m.starts_with("magnet:"))
        .ok_or_else(|| {
            ProviderError::Parse(format!(
                "detail_magnet selector `{raw}` found no magnet link"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split() {
        assert_eq!(split_sel("a.x @href"), ("a.x", Some("href")));
        assert_eq!(split_sel("td.size"), ("td.size", None));
        assert_eq!(split_sel("@data-x"), ("", Some("data-x")));
        assert_eq!(split_sel("a[title='a @b']"), ("a[title='a @b']", None));
    }

    #[test]
    fn compile_errors() {
        assert!(Sel::compile("div >> p").is_err());
        assert!(Sel::compile("").is_err());
        assert!(Sel::compile("@href").is_ok());
    }
}
