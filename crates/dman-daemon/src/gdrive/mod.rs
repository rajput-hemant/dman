//! Google Drive folder links: listing a public folder (recursively) and
//! resolving each file's direct download URL, so a folder becomes one aria2
//! job per file. Only link-shared folders work; there is no sign-in.

mod client;
mod error;
mod parse;

pub use client::{GdriveClient, GdriveFileDownload};
pub use error::GdriveError;
pub use parse::{
    FileItem, FolderListing, SubfolderItem, parse_confirm_page, parse_folder_html, parse_folder_id,
    sanitize_name,
};

/// Returns the folder ID if the given source is an `AddSource::Uris` with a single
/// Google Drive folder URL.
pub fn parse_folder_id_from_source(src: &dman_proto::AddSource) -> Option<String> {
    match src {
        dman_proto::AddSource::Uris { uris } if uris.len() == 1 => parse_folder_id(&uris[0]),
        _ => None,
    }
}
