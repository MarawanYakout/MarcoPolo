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
//! let results = find::search_all(&client, &client, "Clean Code", None).await;
//! ```

pub mod annas;
pub mod archive;
pub mod gutenberg;
pub mod openlibrary;
pub mod github;
pub mod googlescholar;
pub mod duckduckgo;
pub mod types;

use std::time::Duration;

use reqwest::Client;
use crate::utils::dedup::dedup_by_key;
use crate::utils::relevance::relevance;
use crate::utils::validation::Source;
use types::FindResult;

/// A source that hasn't answered by then is skipped, so one slow or
/// blocked site can't stall the whole search.
const SOURCE_TIMEOUT: Duration = Duration::from_secs(12);

/// Query all sources in parallel and return a deduplicated result list,
/// best matches first.
///
/// Pass `source_filter` to restrict to a single source identifier.
/// `gh_client` (which may carry a GitHub token) is used only for the GitHub
/// API; every other source gets the plain `client`.
pub async fn search_all(
    client:        &Client,
    gh_client:     &Client,
    query:         &str,
    source_filter: Option<Source>,
) -> Vec<FindResult> {
    macro_rules! run_source {
        ($variant:path, $fut:expr) => {{
            if source_filter.is_none_or(|f| f == $variant) {
                tokio::time::timeout(SOURCE_TIMEOUT, $fut).await.unwrap_or_default()
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
        async { run_source!(Source::Github,        github::search(gh_client, query)) },
        async { run_source!(Source::Googlescholar, googlescholar::search(client, query)) },
        async { run_source!(Source::Duckduckgo,    duckduckgo::search(client, query)) },
    );

    let mut all = dedup_by_key([a, b, c, d, e, f, g].concat(), |r| r.url.clone());

    // Rank by how well each title matches the query and drop results that
    // share no word with it. The sort is stable, so ties keep source order.
    for r in &mut all {
        r.score = Some(relevance(query, &r.title));
    }
    all.retain(|r| r.score.unwrap_or(0.0) > 0.0);
    all.sort_by(|a, b| b.score.unwrap_or(0.0).total_cmp(&a.score.unwrap_or(0.0)));
    all
}
