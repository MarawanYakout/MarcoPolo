//! Project Gutenberg search via the Gutendex JSON API.
//!
//! Endpoint: `https://gutendex.com/books`
//!
//! Gutendex is a self-hosted Gutenberg catalogue.  We pick the best
//! available format in this preference order: PDF > plain-text > HTML.

use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use url::Url;
use crate::utils::slug::slug;
use super::types::FindResult;

const RESULTS_FIELD: &str = "results";

#[derive(Deserialize)]
struct Resp { results: Vec<Book> }

#[derive(Deserialize)]
struct Book {
    title:   String,
    authors: Vec<Author>,
    formats: HashMap<String, String>,
}

#[derive(Deserialize)]
struct Author { name: String }

/// Format MIME types to probe, in preference order.
const FORMAT_PRIORITY: &[(&str, &str)] = &[
    ("application/pdf",        "pdf"),
    ("text/plain; charset=utf-8", "txt"),
    ("text/plain",              "txt"),
    ("text/html; charset=utf-8", "html"),
    ("text/html",               "html"),
];

/// Search Gutendex for `query` and return the best-format download link.
pub async fn search(client: &Client, query: &str) -> Vec<FindResult> {
    let _ = RESULTS_FIELD; // suppress unused-constant lint
    let mut url = match Url::parse("https://gutendex.com/books") {
        Ok(u)  => u,
        Err(_) => return vec![],
    };
    url.query_pairs_mut().append_pair("search", query);

    let resp = match client.get(url).send().await {
        Ok(r)  => r,
        Err(_) => return vec![],
    };
    let data: Resp = match resp.json().await {
        Ok(d)  => d,
        Err(_) => return vec![],
    };

    data.results.into_iter().filter_map(|book| {
        let (dl_url, fmt) = FORMAT_PRIORITY
            .iter()
            .find_map(|(mime, ext)| {
                book.formats.get(*mime).map(|u| (u.clone(), ext.to_string()))
            })?;

        let author   = book.authors.first().map(|a| a.name.as_str()).unwrap_or("Unknown");
        let full_title = format!("{} — {}", book.title, author);
        let filename = format!("{}.{fmt}", slug(&book.title));

        Some(FindResult {
            source:   "gutenberg.org",
            title:    full_title,
            url:      dl_url,
            format:   Some(fmt),
            score:    None,
            filename,
        })
    }).collect()
}
