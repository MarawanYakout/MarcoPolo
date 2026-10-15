//! Internet Archive full-text search via the Advanced Search JSON API.
//!
//! Endpoint: `https://archive.org/advancedsearch.php`
//!
//! Results are limited to public `mediatype:texts` items (lend-only
//! collections are excluded — their PDFs return 403). Search results only
//! carry an item identifier, so the download URL starts as the guess
//! `https://archive.org/download/{identifier}/{identifier}.pdf` and is
//! swapped for the item's real public PDF by [`resolve_download_url`] just
//! before downloading.

use reqwest::Client;
use serde::Deserialize;
use url::Url;
use crate::utils::parsing::encode_path;
use crate::utils::relevance::relevance;
use crate::utils::slug::slug;
use super::types::FindResult;

const MAX_ROWS: &str = "8";

/// Lend-only collections: their PDFs are private or DRM-encrypted.
const NOT_RESTRICTED: &str = "NOT collection:(inlibrary OR printdisabled)";

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
    // Lucene operators in a raw title (`C++`, `Design: Patterns`, quotes)
    // break the query, so search on plain words only.
    let terms = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if terms.is_empty() { return vec![]; }

    let mut url = match Url::parse("https://archive.org/advancedsearch.php") {
        Ok(u)  => u,
        Err(_) => return vec![],
    };
    url.query_pairs_mut()
        .append_pair("q",      &format!("title:({terms}) AND mediatype:texts AND {NOT_RESTRICTED}"))
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

// ── Item metadata ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Meta {
    #[serde(default)]
    files: Vec<MetaFile>,
}

#[derive(Deserialize)]
struct MetaFile {
    name: String,
    #[serde(default)]
    format: String,
    /// Present (as `"true"`) on lend-only files, which return 403.
    #[serde(default)]
    private: Option<serde_json::Value>,
}

/// Find a publicly downloadable PDF inside archive item `id`: `{id}.pdf`
/// when present, otherwise the PDF whose name best matches `hint` (items
/// can hold dozens of PDFs). `None` when the item has no public PDF.
pub async fn public_pdf_url(client: &Client, id: &str, hint: &str) -> Option<String> {
    let meta: Meta = client
        .get(format!("https://archive.org/metadata/{id}"))
        .send().await.ok()?
        .json().await.ok()?;

    let pdfs: Vec<&MetaFile> = meta
        .files
        .iter()
        .filter(|f| {
            f.name.to_lowercase().ends_with(".pdf")
                && f.private.is_none()
                && !f.format.contains("Encrypted")
        })
        .collect();

    let own  = format!("{id}.pdf");
    let best = pdfs.iter().find(|f| f.name == own).or_else(|| {
        // `rev` so that among equal scores the first file wins.
        pdfs.iter().rev().max_by(|a, b| relevance(hint, &a.name).total_cmp(&relevance(hint, &b.name)))
    })?;

    Some(format!("https://archive.org/download/{id}/{}", encode_path(&best.name)))
}

/// Swap a guessed `archive.org/download/{id}/{id}.pdf` URL for the item's
/// real public PDF. Any other URL — or a lookup failure — is returned as is.
pub async fn resolve_download_url(client: &Client, url: &str, hint: &str) -> String {
    let Some((id, file)) = url
        .strip_prefix("https://archive.org/download/")
        .and_then(|rest| rest.split_once('/'))
    else {
        return url.to_owned();
    };
    if file != format!("{id}.pdf") {
        return url.to_owned();
    }
    public_pdf_url(client, id, hint).await.unwrap_or_else(|| url.to_owned())
}
