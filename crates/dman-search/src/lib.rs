//! Torrent search for dman: declarative provider definitions, parsers for
//! Torznab / RSS / Atom / HTML / JSON responses, a concurrent search engine
//! with per-provider isolation, and result aggregation.
//!
//! The searcher and its example are in `engine` (feature `engine`, on by default).
//!
//! Without the `engine` feature only the data model is built (provider definitions,
//! results, tracker-list parsing): enough for configuration handling, without the
//! HTTP client and the HTML/XML parsers.

pub mod aggregate;
pub mod def;
#[cfg(feature = "engine")]
pub mod engine;
pub mod error;
#[cfg(feature = "engine")]
pub mod feed;
#[cfg(feature = "engine")]
pub mod html;
pub mod json;
pub mod result;
#[cfg(feature = "engine")]
pub mod torznab;
pub mod trackers;
pub mod util;

#[cfg(feature = "engine")]
pub use aggregate::aggregate;
pub use aggregate::{Aggregator, SortKey, sort_by};
#[cfg(feature = "engine")]
pub use def::parse_response;
pub use def::{
    HtmlSpec, HttpMethod, JsonSpec, Kind, ProviderDef, builtin_providers, export, import,
};
#[cfg(feature = "engine")]
pub use engine::{ProviderEvent, Searcher, SearcherConfig, TestReport, test_provider};
pub use error::{DefError, ProviderError};
pub use result::{Query, SearchResult};
#[cfg(feature = "engine")]
pub use trackers::fetch_tracker_list;
pub use trackers::{DEFAULT_TRACKER_LIST_URL, parse_tracker_list};
