//! Torrent search for dman: declarative provider definitions, parsers for
//! Torznab / RSS / Atom / HTML / JSON responses, a concurrent search engine
//! with per-provider isolation, and result aggregation.
//!
//! Typical use:
//!
//! ```no_run
//! use dman_search::{builtin_providers, Aggregator, ProviderEvent, Query, Searcher, SearcherConfig};
//! use futures_util::StreamExt;
//!
//! # async fn demo() -> Result<(), dman_search::ProviderError> {
//! let searcher = Searcher::new(SearcherConfig::default())?;
//! let mut defs = builtin_providers();
//! defs[2].enabled = true; // Nyaa
//! searcher.set_providers(defs);
//! let mut agg = Aggregator::default();
//! let mut stream = Box::pin(searcher.search(&Query::new("ubuntu")));
//! while let Some(event) = stream.next().await {
//!     if let ProviderEvent::Results { results, .. } = event {
//!         agg.add(results);
//!     }
//! }
//! let ranked = agg.results();
//! # let _ = ranked;
//! # Ok(()) }
//! ```

pub mod aggregate;
pub mod def;
pub mod engine;
pub mod error;
pub mod feed;
pub mod html;
pub mod json;
pub mod result;
pub mod torznab;
pub mod trackers;
pub mod util;

pub use aggregate::{Aggregator, SortKey, aggregate, sort_by};
pub use def::{
    HtmlSpec, HttpMethod, JsonSpec, Kind, ProviderDef, builtin_providers, export, import,
    parse_response,
};
pub use engine::{ProviderEvent, Searcher, SearcherConfig, TestReport, test_provider};
pub use error::{DefError, ProviderError};
pub use result::{Query, SearchResult};
pub use trackers::{fetch_tracker_list, parse_tracker_list};
