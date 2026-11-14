//! HTTP client and recursive folder expansion for Google Drive.

use std::collections::HashSet;
use std::path::PathBuf;

use super::error::GdriveError;
use super::parse::{FolderListing, parse_confirm_page, parse_folder_html, sanitize_name};

const DEFAULT_BASE_URL: &str = "https://drive.google.com";
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
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
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

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED
            || resp.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(GdriveError::AuthUnsupported);
        }

        resp.text()
            .await
            .map_err(|e| GdriveError::Network(e.to_string()))
    }

    /// Resolves the direct download URL for a file, following confirm tokens if necessary.
    pub async fn resolve_file_download_url(&self, file_id: &str) -> Result<String, GdriveError> {
        let url = format!("{}/uc?id={file_id}&export=download", self.base());
        let resp = self
            .http
            .get(&url)
            .header(reqwest::header::USER_AGENT, BROWSER_UA)
            .send()
            .await
            .map_err(|e| GdriveError::Network(e.to_string()))?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED
            || resp.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(GdriveError::AuthUnsupported);
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
            for file in listing.files {
                let file_name = sanitize_name(&file.name);
                let download_url = self.resolve_file_download_url(&file.id).await?;
                out.push(GdriveFileDownload {
                    rel_dir: current_dir.clone(),
                    file_name,
                    download_url,
                });
            }

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
