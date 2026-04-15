//! Multi-source free-book search engine.
//!
//! Sources queried in parallel:
//! - [`archive`]   — Internet Archive (JSON API, most reliable)
//! - [`openlibrary`] — Open Library (JSON API, curated)
//! - [`gutenberg`] — Project Gutenberg via Gutendex (JSON API)
//! - [`annas`]     — Anna's Archive (HTML scrape, largest catalog)
//! - [`github`]    — GitHub repository code search
//! - [`googlescholar`] — Google Scholar search
//! - [`duckduckgo`] — DuckDuckGo filetype:pdf search
//!
//! # Quick start
//! ```no_run
//! let client  = reqwest::Client::new();
//! let results = find::search_all(&client, "Clean Code", None).await;
//! ```

pub mod annas;
pub mod archive;
pub mod gutenberg;
pub mod openlibrary;
pub mod github;
pub mod googlescholar;
pub mod duckduckgo;
pub mod types;

use reqwest::Client;
use crate::utils::dedup::dedup_by_key;
use crate::utils::validation::Source;
use types::FindResult;

/// Query all sources in parallel and return a deduplicated result list.
///
/// Pass `source_filter` to restrict to a single source identifier.
pub async fn search_all(
    client:        &Client,
    query:         &str,
    source_filter: Option<Source>,
) -> Vec<FindResult> {
    macro_rules! run_source {
        ($variant:path, $fut:expr) => {{
            if source_filter.map_or(true, |f| f == $variant) {
                $fut.await
            } else {
                vec![]
            }
        }};
    }

    let (a, b, c, d, e, f, g) = tokio::join!(
        async { run_source!(Source::Archive,       archive::search(client, query)) },
        async { run_source!(Source::Openlibrary,   openlibrary::search(client, query)) },
        async { run_source!(Source::Gutenberg,     gutenberg::search(client, query)) },
        async { run_source!(Source::Annas,         annas::search(client, query)) },
        async { run_source!(Source::Github,        github::search(client, query)) },
        async { run_source!(Source::Googlescholar, googlescholar::search(client, query)) },
        async { run_source!(Source::Duckduckgo,    duckduckgo::search(client, query)) },
    );

    let all = [a, b, c, d, e, f, g].concat();
    dedup_by_key(all, |r| r.url.clone())
}
