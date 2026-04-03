//! Multi-source free-book search engine.
//!
//! Sources queried in parallel:
//! - [`archive`]   — Internet Archive (JSON API, most reliable)
//! - [`openlibrary`] — Open Library (JSON API, curated)
//! - [`gutenberg`] — Project Gutenberg via Gutendex (JSON API)
//! - [`annas`]     — Anna's Archive (HTML scrape, largest catalog)
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
pub mod types;

use reqwest::Client;
use crate::utils::dedup::dedup_by_key;
use types::FindResult;

/// Query all four sources in parallel and return a deduplicated result list.
///
/// Pass `source_filter` to restrict to a single source identifier
/// (`"archive"`, `"openlibrary"`, `"gutenberg"`, `"annas"`).
pub async fn search_all(
    client:        &Client,
    query:         &str,
    source_filter: Option<&str>,
) -> Vec<FindResult> {
    let filter = source_filter.map(|s| s.to_lowercase());

    macro_rules! run_source {
        ($name:expr, $fut:expr) => {{
            if filter.as_deref().map_or(true, |f| f == $name) {
                $fut.await
            } else {
                vec![]
            }
        }};
    }

    let (a, b, c, d) = tokio::join!(
        async { run_source!("archive",     archive::search(client, query)) },
        async { run_source!("openlibrary", openlibrary::search(client, query)) },
        async { run_source!("gutenberg",   gutenberg::search(client, query)) },
        async { run_source!("annas",       annas::search(client, query)) },
    );

    let all = [a, b, c, d].concat();
    dedup_by_key(all, |r| r.url.clone())
}
