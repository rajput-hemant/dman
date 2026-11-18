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

/// Unescapes a JavaScript string literal into raw UTF-8 bytes and then a `String`.
/// Handles `\xHH`, `\uHHHH`, and standard escape sequences.
pub fn unescape_js_string(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('x') => {
                    let h1 = chars.next().unwrap_or('0');
                    let h2 = chars.next().unwrap_or('0');
                    let mut hex = [0u8; 2];
                    hex[0] = h1 as u8;
                    hex[1] = h2 as u8;
                    if let Ok(hex_str) = std::str::from_utf8(&hex)
                        && let Ok(b) = u8::from_str_radix(hex_str, 16)
                    {
                        bytes.push(b);
                    }
                }
                Some('u') => {
                    let mut hex = String::with_capacity(4);
                    for _ in 0..4 {
                        if let Some(h) = chars.next() {
                            hex.push(h);
                        }
                    }
                    if let Ok(cp) = u32::from_str_radix(&hex, 16)
                        && let Some(ch) = char::from_u32(cp)
                    {
                        let mut buf = [0u8; 4];
                        bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                }
                Some('n') => bytes.push(b'\n'),
                Some('r') => bytes.push(b'\r'),
                Some('t') => bytes.push(b'\t'),
                Some('\\') => bytes.push(b'\\'),
                Some('\'') => bytes.push(b'\''),
                Some('"') => bytes.push(b'"'),
                Some('/') => bytes.push(b'/'),
                Some(other) => {
                    let mut buf = [0u8; 4];
                    bytes.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
                }
                None => break,
            }
        } else {
            let mut buf = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
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
    let tag_name = html[start_tag + 1..]
        .split([' ', '>'])
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

    let ivd_pos = html.find("_DRIVE_ivd").ok_or(GdriveError::MissingBlob)?;
    let after_var = &html[ivd_pos..];
    let eq_pos = after_var.find('=').ok_or(GdriveError::MissingBlob)? + ivd_pos;
    let after_eq = &html[eq_pos + 1..];

    // Find opening quote
    let mut quote_char = None;
    let mut start_idx = 0;
    for (i, c) in after_eq.char_indices() {
        if c == '\'' || c == '"' {
            quote_char = Some(c);
            start_idx = eq_pos + 1 + i + 1;
            break;
        } else if !c.is_whitespace() {
            return Err(GdriveError::MissingBlob);
        }
    }
    let quote = quote_char.ok_or(GdriveError::MissingBlob)?;

    // Find closing quote
    let literal_slice = &html[start_idx..];
    let mut end_idx = 0;
    let mut escaped = false;
    for (i, c) in literal_slice.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            end_idx = start_idx + i;
            break;
        }
    }
    if end_idx == 0 {
        return Err(GdriveError::MissingBlob);
    }

    let raw = &html[start_idx..end_idx];
    Ok(unescape_js_string(raw))
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

/// Parses an HTML confirmation / virus scan warning page and extracts the download URL.
pub fn parse_confirm_page(html: &str) -> Result<String, GdriveError> {
    check_page_errors(html)?;

    // 1. Check for form #download-form
    if let Some(form_pos) = html.find("<form") {
        let form_slice = &html[form_pos..];
        if let Some(form_end) = form_slice.find("</form>") {
            let form_html = &form_slice[..form_end + 7];
            let is_confirm_form =
                form_html.contains("download-form") || form_html.contains("uc-download-link");
            let action_opt = if is_confirm_form {
                extract_attr(form_html, "action")
            } else {
                None
            };
            if let Some(action) = action_opt {
                let mut params = Vec::new();
                let mut scan = form_html;
                while let Some(input_pos) = scan.find("<input") {
                    let input_tail = &scan[input_pos..];
                    let input_end = input_tail.find('>').unwrap_or(input_tail.len());
                    let input_tag = &input_tail[..input_end];
                    let name = extract_attr(input_tag, "name");
                    let value = extract_attr(input_tag, "value");
                    if let (Some(n), Some(v)) = (name, value) {
                        params.push((n, v));
                    }
                    scan = &input_tail[input_end..];
                }
                if !params.is_empty() {
                    let query = params
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join("&");
                    let sep = if action.contains('?') { "&" } else { "?" };
                    return Ok(format!("{action}{sep}{query}"));
                }
            }
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
        {
            let cleaned = unescape_html(link);
            let full_url = if cleaned.starts_with('/') {
                format!("https://docs.google.com{cleaned}")
            } else {
                cleaned
            };
            return Ok(full_url);
        }
        scan = &tail[end..];
    }

    // 3. Check for JSON "downloadUrl":"..."
    if let Some(pos) = html.find("\"downloadUrl\":\"") {
        let tail = &html[pos + 15..];
        if let Some(end) = tail.find('"') {
            let u = &tail[..end];
            let unescaped = u.replace("\\u003d", "=").replace("\\u0026", "&");
            return Ok(unescaped);
        }
    }

    Err(GdriveError::ConfirmTokenNotFound)
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
