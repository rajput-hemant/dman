//! HTTP client and recursive folder expansion for Google Drive.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use futures_util::{StreamExt as _, TryStreamExt as _};

use super::error::GdriveError;
use super::parse::{FolderListing, parse_confirm_page, parse_folder_html, sanitize_name};

const DEFAULT_BASE_URL: &str = "https://drive.google.com";
/// A stalled Drive connection must not keep `dman add` waiting forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Resolving every file at once exhausts the connection pool on large folders.
/// Results keep the listing order, which becomes the queue order of the jobs.
const MAX_CONCURRENT_RESOLVES: usize = 8;
const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// A resolved file ready for download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GdriveFileDownload {
    /// Directory relative to the download target, e.g. `<folder>` or `<folder>/<subfolder>`.
    pub rel_dir: PathBuf,
    /// Output file name (sanitized).
    pub file_name: String,
    /// Resolved direct download URL.
    pub download_url: String,
}

/// Client for discovering and expanding Google Drive folders.
pub struct GdriveClient {
    http: reqwest::Client,
    no_redirect_http: reqwest::Client,
    base_url: Option<String>,
}

impl Default for GdriveClient {
    fn default() -> Self {
        Self::new()
    }
}

impl GdriveClient {
    /// Creates a new Google Drive client with the default crypto provider installed.
    pub fn new() -> Self {
        crate::notify::install_crypto_provider();
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let no_redirect_http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            no_redirect_http,
            base_url: None,
        }
    }

    /// Sets an alternate base URL (used in tests with mock servers).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    fn base(&self) -> &str {
        self.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL)
    }

    /// Fetches the HTML page of a folder.
    pub async fn fetch_folder_page(&self, folder_id: &str) -> Result<String, GdriveError> {
        let url = format!("{}/drive/folders/{folder_id}", self.base());
        let resp = self
            .http
            .get(&url)
            .header(reqwest::header::USER_AGENT, BROWSER_UA)
            .send()
            .await
            .map_err(|e| GdriveError::Network(e.to_string()))?;

        if is_sign_in_wall(&resp) {
            return Err(GdriveError::AuthUnsupported);
        }
        if !resp.status().is_success() {
            // A deleted folder or a mistyped id, not a private one.
            return Err(GdriveError::Network(format!(
                "Google Drive answered {} for folder {folder_id}",
                resp.status()
            )));
        }

        resp.text()
            .await
            .map_err(|e| GdriveError::Network(e.to_string()))
    }

    /// Resolves the direct download URL for a file, following confirm tokens if necessary.
    pub async fn resolve_file_download_url(&self, file_id: &str) -> Result<String, GdriveError> {
        let url = format!("{}/uc?id={file_id}&export=download", self.base());
        let resp = self
            .no_redirect_http
            .get(&url)
            .header(reqwest::header::USER_AGENT, BROWSER_UA)
            .send()
            .await
            .map_err(|e| GdriveError::Network(e.to_string()))?;

        if is_sign_in_wall(&resp) {
            return Err(GdriveError::AuthUnsupported);
        }

        // Direct file downloads redirect to drive.usercontent.google.com with 302/303.
        // Return the Location header immediately to avoid downloading file bodies here.
        if resp.status().is_redirection() {
            if let Some(loc_str) = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|l| l.to_str().ok())
            {
                if loc_str.starts_with("https://accounts.google.") {
                    return Err(GdriveError::AuthUnsupported);
                }
                return Ok(loc_str.to_string());
            }
            return Ok(url);
        }

        let headers = resp.headers();
        let content_type = headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();

        if headers.contains_key(reqwest::header::CONTENT_DISPOSITION)
            || !content_type.starts_with("text/html")
        {
            return Ok(resp.url().to_string());
        }

        let text = resp
            .text()
            .await
            .map_err(|e| GdriveError::Network(e.to_string()))?;
        parse_confirm_page(&text)
    }

    /// Recursively expands a folder into individual file download jobs.
    pub async fn expand_folder(
        &self,
        root_folder_id: &str,
    ) -> Result<Vec<GdriveFileDownload>, GdriveError> {
        let mut visited = HashSet::new();
        visited.insert(root_folder_id.to_string());

        let html = self.fetch_folder_page(root_folder_id).await?;
        let listing = parse_folder_html(&html)?;

        let root_title = listing.folder_name.as_deref().unwrap_or(root_folder_id);
        let root_dir_name = sanitize_name(root_title);
        let current_dir = PathBuf::from(root_dir_name);

        let mut out = Vec::new();
        self.collect_folder(listing, current_dir, &mut visited, &mut out)
            .await?;

        if out.is_empty() {
            return Err(GdriveError::EmptyFolder);
        }

        dedupe_names(&mut out);
        Ok(out)
    }

    fn collect_folder<'a>(
        &'a self,
        listing: FolderListing,
        current_dir: PathBuf,
        visited: &'a mut HashSet<String>,
        out: &'a mut Vec<GdriveFileDownload>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), GdriveError>> + Send + 'a>>
    {
        Box::pin(async move {
            let file_futs = listing.files.into_iter().map(|file| {
                let current_dir = current_dir.clone();
                async move {
                    let file_name = sanitize_name(&file.name);
                    let download_url = self.resolve_file_download_url(&file.id).await?;
                    Ok::<_, GdriveError>(GdriveFileDownload {
                        rel_dir: current_dir,
                        file_name,
                        download_url,
                    })
                }
            });
            let resolved_files = futures_util::stream::iter(file_futs)
                .buffered(MAX_CONCURRENT_RESOLVES)
                .try_collect::<Vec<_>>()
                .await?;
            out.extend(resolved_files);

            for subfolder in listing.subfolders {
                if !visited.insert(subfolder.id.clone()) {
                    continue; // Guard cycles
                }
                let sub_name = sanitize_name(&subfolder.name);
                let next_dir = current_dir.join(sub_name);
                let sub_html = self.fetch_folder_page(&subfolder.id).await?;
                let sub_listing = parse_folder_html(&sub_html)?;
                self.collect_folder(sub_listing, next_dir, visited, out)
                    .await?;
            }

            Ok(())
        })
    }
}

/// Drive answers a private item with 401/403 or a redirect to the sign-in page.
fn is_sign_in_wall(resp: &reqwest::Response) -> bool {
    matches!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) || resp
        .url()
        .domain()
        .is_some_and(|d| d.starts_with("accounts.google."))
}

/// Two Drive names can sanitize to the same file name in one folder, which would
/// give two aria2 jobs the same `dir` and `out`. Suffix the later ones.
pub fn dedupe_names(files: &mut [GdriveFileDownload]) {
    let mut taken: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    for file in files.iter_mut() {
        let set = taken.entry(file.rel_dir.clone()).or_default();
        if set.insert(file.file_name.to_lowercase()) {
            continue;
        }
        let (stem, ext) = match file.file_name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{ext}")),
            _ => (file.file_name.clone(), String::new()),
        };
        let mut n = 2;
        loop {
            let candidate = format!("{stem} ({n}){ext}");
            if set.insert(candidate.to_lowercase()) {
                file.file_name = candidate;
                break;
            }
            n += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &str, name: &str) -> GdriveFileDownload {
        GdriveFileDownload {
            rel_dir: PathBuf::from(dir),
            file_name: name.to_owned(),
            download_url: String::new(),
        }
    }

    #[test]
    fn dedupe_names_keeps_names_unique_per_directory() {
        let mut files = vec![
            file("f", "a_b.txt"),
            file("f", "a_b (2).txt"),
            file("f", "a_b.txt"),
            file("f/sub", "a_b.txt"),
        ];
        dedupe_names(&mut files);
        assert_eq!(files[0].file_name, "a_b.txt");
        assert_eq!(files[1].file_name, "a_b (2).txt");
        assert_eq!(files[2].file_name, "a_b (3).txt");
        assert_eq!(files[3].file_name, "a_b.txt", "other dirs are independent");
    }

    #[test]
    fn dedupe_names_is_case_insensitive() {
        let mut files = vec![file("f", "Readme.md"), file("f", "readme.md")];
        dedupe_names(&mut files);
        assert_eq!(files[1].file_name, "readme (2).md");
    }
}
