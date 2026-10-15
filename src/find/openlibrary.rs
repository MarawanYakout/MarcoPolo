//! Open Library search via the public JSON API.
//!
//! Endpoint: `https://openlibrary.org/search.json`
//!
//! Only public-domain books (`ebook_access = "public"`) are returned —
//! borrowable ones have no free download. Each book lists many Internet
//! Archive scans, some of them lend-only, so the first few are checked
//! until one with a public PDF is found.

use reqwest::Client;
use serde::Deserialize;
use url::Url;
use crate::utils::slug::slug;
use super::archive::public_pdf_url;
use super::types::FindResult;

const LIMIT: &str = "6";

/// Scans checked per book before giving up on it.
const MAX_SCANS: usize = 3;

#[derive(Deserialize)]
struct Resp { docs: Vec<Doc> }
#[derive(Deserialize)]
struct Doc {
    title: String,
    #[serde(default)]
    ia: Vec<String>,
    #[serde(default)]
    ebook_access: String,
}

/// Search Open Library for `query` and return public books with a PDF scan.
pub async fn search(client: &Client, query: &str) -> Vec<FindResult> {
    let mut url = match Url::parse("https://openlibrary.org/search.json") {
        Ok(u)  => u,
        Err(_) => return vec![],
    };
    url.query_pairs_mut()
        .append_pair("title",        query)
        .append_pair("limit",        LIMIT)
        .append_pair("has_fulltext", "true")
        .append_pair("fields",       "title,ia,ebook_access");

    let resp = match client.get(url).send().await {
        Ok(r)  => r,
        Err(_) => return vec![],
    };
    let data: Resp = match resp.json().await {
        Ok(d)  => d,
        Err(_) => return vec![],
    };

    let found = futures::future::join_all(
        data.docs
            .into_iter()
            .filter(|doc| doc.ebook_access == "public")
            .map(|doc| async move {
                for id in doc.ia.iter().take(MAX_SCANS) {
                    if let Some(dl_url) = public_pdf_url(client, id, &doc.title).await {
                        return Some(FindResult {
                            source:   "openlibrary.org",
                            filename: format!("{}.pdf", slug(&doc.title)),
                            title:    doc.title,
                            url:      dl_url,
                            format:   Some("pdf".to_owned()),
                            score:    None,
                        });
                    }
                }
                None
            }),
    ).await;

    found.into_iter().flatten().collect()
}
