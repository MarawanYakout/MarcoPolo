//! Internet Archive full-text search via the Advanced Search JSON API.
//!
//! Endpoint: `https://archive.org/advancedsearch.php`
//!
//! Results are limited to `mediatype:texts` items that have a known identifier.
//! The download URL is constructed as:
//!   `https://archive.org/download/{identifier}/{identifier}.pdf`

use reqwest::Client;
use serde::Deserialize;
use url::Url;
use crate::utils::slug::slug;
use super::types::FindResult;

const MAX_ROWS: &str = "8";

#[derive(Deserialize)]
struct Resp  { response: Inner }
#[derive(Deserialize)]
struct Inner { docs: Vec<Doc> }
#[derive(Deserialize)]
struct Doc {
    identifier: String,
    #[serde(default)]
    title: String,
}

/// Search Archive.org for items matching `query` and return up to 8 results.
pub async fn search(client: &Client, query: &str) -> Vec<FindResult> {
    let mut url = match Url::parse("https://archive.org/advancedsearch.php") {
        Ok(u)  => u,
        Err(_) => return vec![],
    };
    url.query_pairs_mut()
        .append_pair("q",      &format!("title:({query}) AND mediatype:texts"))
        .append_pair("fl[]",   "identifier,title")
        .append_pair("output", "json")
        .append_pair("rows",   MAX_ROWS);

    let resp = match client.get(url).send().await {
        Ok(r)  => r,
        Err(_) => return vec![],
    };
    let data: Resp = match resp.json().await {
        Ok(d)  => d,
        Err(_) => return vec![],
    };

    data.response.docs.into_iter().map(|doc| {
        let title    = if doc.title.is_empty() { doc.identifier.clone() } else { doc.title.clone() };
        let dl_url   = format!("https://archive.org/download/{id}/{id}.pdf", id = doc.identifier);
        let filename = format!("{}.pdf", slug(&title));
        FindResult {
            source:   "archive.org",
            title,
            url:      dl_url,
            format:   Some("pdf".to_owned()),
            score:    None,
            filename,
        }
    }).collect()
}
