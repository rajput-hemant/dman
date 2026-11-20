//! String and JSON parsing for Google Drive folder pages and confirmation forms.

use super::error::GdriveError;

/// A parsed file item from a folder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileItem {
    pub id: String,
    pub name: String,
    pub mime_type: Option<String>,
}

/// A parsed subfolder item from a folder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubfolderItem {
    pub id: String,
    pub name: String,
}

/// Contents parsed from a Google Drive folder page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderListing {
    pub folder_name: Option<String>,
    pub files: Vec<FileItem>,
    pub subfolders: Vec<SubfolderItem>,
}

const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

/// Extracts the folder ID from a URL, accepting `/drive/folders/` and
/// `/drive/u/<n>/folders/`. Returns `None` for `/file/d/` and other links.
pub use dman_proto::parse_drive_folder_id as parse_folder_id;

/// Sanitizes a file or folder name for safe filesystem usage across platforms.
pub fn sanitize_name(name: &str) -> String {
    let mut s = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '\0' => {}
            '/' | '\\' => s.push('_'),
            c if c.is_ascii_control() => s.push('_'),
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => s.push('_'),
            c => s.push(c),
        }
    }
    while s.contains("..") {
        s = s.replace("..", "__");
    }
    let trimmed = s.trim().trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return "_".to_string();
    }
    let stem = trimmed.split('.').next().unwrap_or("").trim_end();
    if is_reserved_stem(stem) {
        format!("_{trimmed}")
    } else {
        trimmed.to_string()
    }
}

fn is_reserved_stem(stem: &str) -> bool {
    let s = stem.to_ascii_uppercase();
    matches!(
        s.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// Unescapes common HTML entities without pulling in an HTML parser.
pub fn unescape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp_pos) = rest.find('&') {
        out.push_str(&rest[..amp_pos]);
        let tail = &rest[amp_pos..];
        if let Some(semi_pos) = tail.find(';') {
            let entity = &tail[1..semi_pos];
            let matched = match entity {
                "quot" => Some('"'),
                "amp" => Some('&'),
                "apos" => Some('\''),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "nbsp" => Some(' '),
                "ndash" => Some('\u{2013}'),
                "mdash" => Some('\u{2014}'),
                "minus" => Some('\u{2212}'),
                _ if entity.starts_with('#') => {
                    let num_str = &entity[1..];
                    if let Some(hex_str) = num_str.strip_prefix('x') {
                        u32::from_str_radix(hex_str, 16)
                            .ok()
                            .and_then(char::from_u32)
                    } else {
                        num_str.parse::<u32>().ok().and_then(char::from_u32)
                    }
                }
                _ => None,
            };
            if let Some(c) = matched {
                out.push(c);
                rest = &tail[semi_pos + 1..];
                continue;
            }
        }
        out.push('&');
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

/// Extracts the folder title from the `<title>` element.
pub fn extract_folder_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start_idx = lower.find("<title")?;
    let tag_end = html[start_idx..].find('>')? + start_idx + 1;
    let end_idx = lower[tag_end..].find("</title>")? + tag_end;
    let raw = &html[tag_end..end_idx];
    let unescaped = unescape_html(raw);
    let mut title = unescaped.trim();
    for suffix in [
        " - Google Drive",
        " \u{2013} Google Drive",
        " \u{2014} Google Drive",
        " \u{2212} Google Drive",
        "- Google Drive",
        "\u{2013} Google Drive",
        "\u{2014} Google Drive",
    ] {
        if let Some(stripped) = title.strip_suffix(suffix) {
            title = stripped.trim();
            break;
        }
    }
    let trimmed = title.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("google drive") {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Unescapes the body of a JavaScript string literal. `\xHH` and `\uHHHH`
/// are code points (not bytes) and a `😀` surrogate pair is one
/// character, as in JavaScript; malformed escapes are dropped.
pub fn unescape_js_string(s: &str) -> String {
    /// Consumes `digits` hex digits from `chars`, if they are all there.
    fn hex(chars: &mut std::str::Chars<'_>, digits: usize) -> Option<u32> {
        let v = u32::from_str_radix(chars.as_str().get(..digits)?, 16).ok()?;
        chars.nth(digits - 1);
        Some(v)
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(e) = chars.next() else { break };
        let decoded = match e {
            'x' => hex(&mut chars, 2).and_then(char::from_u32),
            'u' => hex(&mut chars, 4).and_then(|hi| {
                if !(0xD800..0xDC00).contains(&hi) {
                    return char::from_u32(hi);
                }
                // A high surrogate takes its low half from the next escape.
                let lo = u32::from_str_radix(chars.as_str().strip_prefix("\\u")?.get(..4)?, 16)
                    .ok()
                    .filter(|lo| (0xDC00..0xE000).contains(lo))?;
                chars.nth(5);
                char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00))
            }),
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            'b' => Some('\u{8}'),
            'f' => Some('\u{c}'),
            'v' => Some('\u{b}'),
            '0' => Some('\0'),
            other => Some(other),
        };
        out.extend(decoded);
    }
    out
}

/// Checks HTML content for Google Drive error responses.
pub fn check_page_errors(html: &str) -> Result<(), GdriveError> {
    if html.contains("Google Drive - Access Denied")
        || html.contains("Google Drive \u{2013} Access Denied")
        || html.contains("Sign in - Google Accounts")
        || html.contains("Sign in \u{2013} Google Accounts")
        || html.contains("You need permission")
        || html.contains("You need access")
        || html.contains("Sorry, the owner hasn't given you permission to download this file")
        || html.contains("Only the owner and editors can download this file")
    {
        return Err(GdriveError::AuthUnsupported);
    }

    if html.contains("Too many users have viewed or downloaded this file recently")
        || html.contains("download quota for this file has been exceeded")
        || html.contains("uc-error-subcaption")
    {
        let msg = extract_subcaption(html).unwrap_or_else(|| {
            "Too many users have viewed or downloaded this file recently".to_string()
        });
        return Err(GdriveError::QuotaExceeded { message: msg });
    }

    Ok(())
}

fn extract_subcaption(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let class_pos = lower.find("uc-error-subcaption")?;
    let start_tag = html[..class_pos].rfind('<')?;
    let tag_end = html[class_pos..].find('>')? + class_pos + 1;
    // Taken from `lower`, where the closing tag is searched for.
    let tag_name = lower[start_tag + 1..]
        .split(|c: char| c.is_ascii_whitespace() || c == '>')
        .next()
        .unwrap_or("p");
    let close_tag = format!("</{tag_name}>");
    let close_pos = lower[tag_end..].find(&close_tag)? + tag_end;
    let content = &html[tag_end..close_pos];
    Some(unescape_html(content.trim()))
}

/// Extracts the raw JSON string from the `_DRIVE_ivd` variable in the HTML.
pub fn extract_ivd_json(html: &str) -> Result<String, GdriveError> {
    check_page_errors(html)?;

    let var = &html[html.find("_DRIVE_ivd").ok_or(GdriveError::MissingBlob)?..];
    let value = var[var.find('=').ok_or(GdriveError::MissingBlob)? + 1..].trim_start();
    let quote = value
        .chars()
        .next()
        .filter(|c| matches!(c, '\'' | '"'))
        .ok_or(GdriveError::MissingBlob)?;
    let body = &value[1..];
    let mut escaped = false;
    let end = body
        .char_indices()
        .find_map(|(i, c)| {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == quote {
                return Some(i);
            }
            None
        })
        .ok_or(GdriveError::MissingBlob)?;
    Ok(unescape_js_string(&body[..end]))
}

/// Parses a Google Drive folder HTML page into a structured listing.
pub fn parse_folder_html(html: &str) -> Result<FolderListing, GdriveError> {
    let json_str = extract_ivd_json(html)?;
    let val: serde_json::Value =
        serde_json::from_str(&json_str).map_err(|e| GdriveError::Json(e.to_string()))?;

    let folder_name = extract_folder_title(html);

    let items_array = val
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_array());

    let mut files = Vec::new();
    let mut subfolders = Vec::new();

    if let Some(items) = items_array {
        for item in items {
            let Some(item_arr) = item.as_array() else {
                continue;
            };
            let id = item_arr.first().and_then(|v| v.as_str());
            let name = item_arr.get(2).and_then(|v| v.as_str());
            let mime = item_arr.get(3).and_then(|v| v.as_str());

            if let (Some(id), Some(name)) = (id, name) {
                if mime == Some(FOLDER_MIME) {
                    subfolders.push(SubfolderItem {
                        id: id.to_string(),
                        name: name.to_string(),
                    });
                } else {
                    files.push(FileItem {
                        id: id.to_string(),
                        name: name.to_string(),
                        mime_type: mime.map(ToString::to_string),
                    });
                }
            }
        }
    }

    Ok(FolderListing {
        folder_name,
        files,
        subfolders,
    })
}

/// Where relative links of the confirmation page point.
const CONFIRM_BASE: &str = "https://docs.google.com/";

/// Parses an HTML confirmation / virus scan warning page and extracts the download URL.
pub fn parse_confirm_page(html: &str) -> Result<String, GdriveError> {
    check_page_errors(html)?;

    // 1. The download form: its action plus its inputs as the query.
    let mut scan = html;
    while let Some(form_pos) = scan.find("<form") {
        let form_slice = &scan[form_pos..];
        let Some(form_end) = form_slice.find("</form>") else {
            break;
        };
        let form_html = &form_slice[..form_end];
        scan = &form_slice[form_end..];
        if !(form_html.contains("download-form") || form_html.contains("uc-download-link")) {
            continue;
        }
        let Some(mut url) = extract_attr(form_html, "action").and_then(|a| absolute(&a)) else {
            continue;
        };
        let mut params = Vec::new();
        let mut inputs = form_html;
        while let Some(input_pos) = inputs.find("<input") {
            let input_tail = &inputs[input_pos..];
            let input_end = input_tail.find('>').unwrap_or(input_tail.len());
            let input_tag = &input_tail[..input_end];
            if let (Some(n), Some(v)) = (
                extract_attr(input_tag, "name"),
                extract_attr(input_tag, "value"),
            ) {
                params.push((n, v));
            }
            inputs = &input_tail[input_end..];
        }
        if !params.is_empty() {
            // Encoded, so a token holding `+` or `&` reaches Drive intact.
            url.query_pairs_mut().extend_pairs(&params);
            return Ok(url.into());
        }
    }

    // 2. Check for link with confirm=
    let mut scan = html;
    while let Some(href_pos) = scan.find("href=\"") {
        let tail = &scan[href_pos + 6..];
        let end = tail.find('"').unwrap_or(0);
        let link = &tail[..end];
        if (link.contains("/uc?export=download") || link.contains("/download?"))
            && link.contains("confirm=")
            && let Some(url) = absolute(&unescape_html(link))
        {
            return Ok(url.into());
        }
        scan = &tail[end..];
    }

    // 3. Check for JSON "downloadUrl":"..."
    if let Some(pos) = html.find("\"downloadUrl\":\"") {
        let tail = &html[pos + 15..];
        if let Some(end) = tail.find('"') {
            return Ok(unescape_js_string(&tail[..end]));
        }
    }

    Err(GdriveError::ConfirmTokenNotFound)
}

/// `link` as an absolute URL, resolved against [`CONFIRM_BASE`] when relative.
fn absolute(link: &str) -> Option<url::Url> {
    url::Url::parse(CONFIRM_BASE).ok()?.join(link).ok()
}

fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    if let Some(pos) = tag.find(&needle) {
        let tail = &tag[pos + needle.len()..];
        let end = tail.find('"')?;
        return Some(unescape_html(&tail[..end]));
    }
    let needle_single = format!("{attr}='");
    if let Some(pos) = tag.find(&needle_single) {
        let tail = &tag[pos + needle_single.len()..];
        let end = tail.find('\'')?;
        return Some(unescape_html(&tail[..end]));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_escapes_are_code_points_and_surrogate_pairs_join() {
        assert_eq!(unescape_js_string(r"caf\xe9"), "café");
        assert_eq!(unescape_js_string(r"café"), "café");
        assert_eq!(unescape_js_string(r"😀.png"), "\u{1F600}.png");
        assert_eq!(unescape_js_string(r"\x5b\x22a\/b\x22\x5d"), r#"["a/b"]"#);
        // Malformed escapes are dropped, never a panic.
        assert_eq!(unescape_js_string(r"a\ud83db\x4"), "ab4");
        assert_eq!(unescape_js_string("end\\"), "end");
    }

    #[test]
    fn quota_message_is_read_from_an_upper_case_tag() {
        let html = r#"<P CLASS="uc-error-subcaption">Quota &amp; more</P>"#;
        assert_eq!(
            check_page_errors(html),
            Err(GdriveError::QuotaExceeded {
                message: "Quota & more".into()
            })
        );
    }

    #[test]
    fn confirm_form_values_are_encoded_and_relative_actions_resolved() {
        let html = r#"<form id="search"><input name="q" value="x"></form>
            <form id="download-form" action="/uc?export=download" method="get">
            <input type="hidden" name="id" value="f1">
            <input type="hidden" name="at" value="a+b&amp;c">
            </form>"#;
        assert_eq!(
            parse_confirm_page(html).unwrap(),
            "https://docs.google.com/uc?export=download&id=f1&at=a%2Bb%26c"
        );
    }
}
