//! Reading links from the system clipboard by running the platform's own
//! clipboard tool (no clipboard library, no clipboard access otherwise).

use std::time::Duration;

use crate::error::CliError;

/// How long one clipboard tool may take before the next one is tried.
const TOOL_TIMEOUT: Duration = Duration::from_secs(3);

/// The clipboard readers of this platform, in the order they are tried.
pub fn tools() -> Vec<(&'static str, &'static [&'static str])> {
    if cfg!(windows) {
        vec![(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", "Get-Clipboard"],
        )]
    } else if cfg!(target_os = "macos") {
        vec![("pbpaste", &[])]
    } else {
        vec![
            ("wl-paste", &["--no-newline"]),
            ("xclip", &["-o", "-selection", "clipboard"]),
            ("xsel", &["-b", "-o"]),
        ]
    }
}

/// What to install when no clipboard tool works.
fn hint() -> &'static str {
    if cfg!(windows) {
        "cannot read the clipboard: PowerShell (Get-Clipboard) is not available"
    } else if cfg!(target_os = "macos") {
        "cannot read the clipboard: pbpaste is not available"
    } else {
        "cannot read the clipboard: install wl-clipboard (Wayland) or xclip / xsel (X11); \
         over SSH paste the link as an argument instead"
    }
}

/// The text in the clipboard, from the first tool that answers.
pub async fn read() -> Result<String, CliError> {
    for (program, args) in tools() {
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let Ok(Ok(out)) = tokio::time::timeout(TOOL_TIMEOUT, cmd.output()).await else {
            continue;
        };
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        tracing::debug!(program, status = %out.status, "clipboard tool failed");
    }
    Err(CliError::new(dman_proto::ErrorCode::Refused, hint()))
}

/// Whether one line of text is something `dman add` takes from the
/// clipboard: a magnet link with an info hash, or an http(s)/ftp/sftp URL
/// with a host.
pub fn is_link(s: &str) -> bool {
    let t = s.trim();
    if t.contains(char::is_whitespace) || t.is_empty() {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("magnet:?") {
        return lower.contains("xt=urn:btih:") || lower.contains("xt=urn:btmh:");
    }
    url::Url::parse(t).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https" | "ftp" | "sftp")
            && u.host_str().is_some_and(|h| !h.is_empty())
    })
}

/// The links in clipboard text (one per line), or `None` when any non-blank
/// line is something else: then the clipboard holds ordinary text.
pub fn links(text: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() || !lines.iter().all(|l| is_link(l)) {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for l in lines {
        if !out.iter().any(|o| o == l) {
            out.push(l.to_owned());
        }
    }
    Some(out)
}

/// The links in the clipboard, or an error saying why there are none.
pub async fn read_links() -> Result<Vec<String>, CliError> {
    let text = read().await?;
    links(&text).ok_or_else(|| {
        CliError::invalid("the clipboard does not hold a URL or magnet link (one per line)")
    })
}

/// A link as shown to the user: credentials and query values hidden.
pub fn shown(link: &str) -> String {
    if link.to_ascii_lowercase().starts_with("magnet:") {
        // The display name is the readable part; the rest is hashes and trackers.
        let name = link
            .split_once('?')
            .map(|(_, q)| q)
            .unwrap_or_default()
            .split('&')
            .find_map(|kv| kv.strip_prefix("dn="))
            .map(|v| {
                url::form_urlencoded::parse(format!("v={v}").as_bytes())
                    .next()
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default()
            });
        return match name {
            Some(n) if !n.is_empty() => format!("magnet link \"{n}\""),
            _ => "magnet link".to_owned(),
        };
    }
    dman_proto::redact::redact_url(link)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_recognised() {
        assert!(is_link("https://example.org/a.iso"));
        assert!(is_link("  ftp://h/x  "));
        assert!(is_link("magnet:?xt=urn:btih:abc&dn=x"));
        assert!(!is_link("magnet:?dn=x"));
        assert!(!is_link("file:///etc/passwd"));
        assert!(!is_link("javascript:alert(1)"));
        assert!(!is_link("hello world"));
        assert!(!is_link("example.org"));
        assert!(!is_link("https://"));
    }

    #[test]
    fn clipboard_text_must_be_links_only() {
        assert_eq!(
            links("https://a/x\n\n magnet:?xt=urn:btih:abc \nhttps://a/x\n"),
            Some(vec![
                "https://a/x".to_owned(),
                "magnet:?xt=urn:btih:abc".to_owned()
            ])
        );
        assert_eq!(links("see https://a/x"), None);
        assert_eq!(links("https://a/x\nsome note"), None);
        assert_eq!(links("   \n"), None);
    }

    #[test]
    fn shown_links_hide_secrets() {
        assert_eq!(
            shown("magnet:?xt=urn:btih:abc&dn=Ubuntu%2024.04&tr=udp://t"),
            "magnet link \"Ubuntu 24.04\""
        );
        assert_eq!(shown("magnet:?xt=urn:btih:abc"), "magnet link");
        let s = shown("https://user:pw@example.org/a.iso?token=secret");
        assert!(!s.contains("pw") && !s.contains("secret"), "{s}");
    }

    #[test]
    fn every_platform_has_a_reader() {
        assert!(!tools().is_empty());
    }
}
