//! Open Library search via the public JSON API.
//!
//! Endpoint: `https://openlibrary.org/search.json`
//!
//! Only books that have an Internet Archive identifier (`ia` field) are
//! returned — those are the ones with freely downloadable full-text PDFs.

use reqwest::Client;
use serde::Deserialize;
use url::Url;
use crate::utils::slug::slug;
use super::types::FindResult;

const LIMIT: &str = "6";

#[derive(Deserialize)]
struct Resp { docs: Vec<Doc> }
#[derive(Deserialize)]
struct Doc {
    title: String,
    #[serde(default)]
    ia: Vec<String>,
}

/// Search Open Library for `query` and return books that have IA full-text.
pub async fn search(client: &Client, query: &str) -> Vec<FindResult> {
    let mut url = match Url::parse("https://openlibrary.org/search.json") {
        Ok(u)  => u,
        Err(_) => return vec![],
    };
    url.query_pairs_mut()
        .append_pair("title",        query)
        .append_pair("limit",        LIMIT)
        .append_pair("has_fulltext", "true");

    let resp = match client.get(url).send().await {
        Ok(r)  => r,
        Err(_) => return vec![],
    };
    let data: Resp = match resp.json().await {
        Ok(d)  => d,
        Err(_) => return vec![],
    };

    data.docs.into_iter().filter_map(|doc| {
        let ia_id = doc.ia.into_iter().next()?;
        let dl_url   = format!("https://archive.org/download/{ia}/{ia}.pdf", ia = ia_id);
        let filename = format!("{}.pdf", slug(&doc.title));
        Some(FindResult {
            source:   "openlibrary.org",
            title:    doc.title,
            url:      dl_url,
            format:   Some("pdf".to_owned()),
            score:    None,
            filename,
        })
    }).collect()
}
