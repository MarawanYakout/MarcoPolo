//! Generic web scraper mode for marcopolo.
//!
//! Fetches an HTML page, extracts every PDF link found in `<a href>` attributes,
//! resolves relative URLs to absolute, and returns them as [`PdfSource`] items.
//! If no PDFs are found on the landing page, it performs **one level of crawl**:
//! it follows every same-origin `<a>` link on the page and scans those too.

use std::collections::HashSet;

use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::{PdfSource, Result};

pub async fn scrape_pdfs(client: &reqwest::Client, page_url: &str) -> Result<Vec<PdfSource>> {
    let base = Url::parse(page_url)?;
    let html  = fetch_html(client, page_url).await?;

    let mut found = extract_pdf_links(&html, &base);

    if found.is_empty() {
        let internal_links = extract_internal_links(&html, &base);
        let handles: Vec<_> = internal_links
            .into_iter()
            .map(|link| {
                let client = client.clone();
                tokio::spawn(async move {
                    let Ok(html) = fetch_html(&client, &link).await else { return vec![] };
                    let Ok(base) = Url::parse(&link)           else { return vec![] };
                    extract_pdf_links(&html, &base)
                })
            })
            .collect();

        let mut seen: HashSet<String> = HashSet::new();
        for h in handles {
            if let Ok(pdfs) = h.await {
                for p in pdfs {
                    if seen.insert(p.url.clone()) {
                        found.push(p);
                    }
                }
            }
        }
    }

    Ok(found)
}

async fn fetch_html(client: &reqwest::Client, url: &str) -> Result<String> {
    let text = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(text)
}

fn extract_pdf_links(html: &str, base: &Url) -> Vec<PdfSource> {
    let doc      = Html::parse_document(html);
    let selector = Selector::parse("a[href]").unwrap();
    let mut seen = HashSet::new();

    doc.select(&selector)
        .filter_map(|el: ElementRef| {
            let href    = el.value().attr("href")?;
            let abs     = base.join(href).ok()?;
            let url_str = abs.to_string();
            if !abs.path().to_lowercase().ends_with(".pdf") {
                return None;
            }
            if !seen.insert(url_str.clone()) {
                return None;
            }
            let name = abs
                .path_segments()?
                .last()
                .unwrap_or("unknown.pdf")
                .to_owned();
            Some(PdfSource { name, url: url_str })
        })
        .collect()
}

fn extract_internal_links(html: &str, base: &Url) -> Vec<String> {
    let doc      = Html::parse_document(html);
    let selector = Selector::parse("a[href]").unwrap();
    let origin   = format!("{}://{}", base.scheme(), base.host_str().unwrap_or(""));
    let mut seen = HashSet::new();

    doc.select(&selector)
        .filter_map(|el: ElementRef| {
            let href = el.value().attr("href")?;
            let abs  = base.join(href).ok()?;
            let url  = abs.to_string();
            if !url.starts_with(&origin) { return None; }
            if abs.path().to_lowercase().ends_with(".pdf") { return None; }
            if seen.insert(url.clone()) { Some(url) } else { None }
        })
        .collect()
}
