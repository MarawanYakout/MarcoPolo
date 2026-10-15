//! Generic web scraper for marcopolo
//!
//! Strategy:
//!   1. Check `/sitemap.xml` at the root domain — fast discovery path.
//!   2. Also check `/sitemap_index.xml` — many sites use a sitemap index
//!      that points to multiple child sitemaps; we follow them one level deep.
//!   3. BFS-crawl from the landing page up to `max_depth` levels, collecting
//!      every matching `<a href>` and `<source src>` / `<img src>` along the way.
//!   4. Filenames are URL-decoded so `%20` becomes a space, etc.
//!   5. Duplicate URLs are dropped at every stage (dedup by canonical URL).
//!   6. Only same-origin links are followed — never crawls external sites.
//!   7. Fragments (#section) are stripped before dedup so the same page
//!      is not fetched twice under different fragment identifiers.

use std::{
    collections::HashSet,
    sync::LazyLock,
    time::Duration,
};

use futures::stream::{self, StreamExt};
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::utils::parsing::filename_from_url;
use crate::{matches_ext, FileSource, Result};

// ── Compiled once, not rebuilt for every page ─────────────────────────────────
static LINK_SEL: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("a[href]").unwrap());
static SRC_SEL: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("source[src], img[src], video[src]").unwrap());
static LOC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<loc>\s*(https?://[^\s<]+)\s*</loc>").unwrap());

// =============================================================================
// Public entry point
// =============================================================================

/// Discovers all files matching `exts` reachable from `page_url`,
/// scanning up to `max_depth` BFS levels deep.
pub async fn scrape_files(
    client:    &reqwest::Client,
    page_url:  &str,
    max_depth: usize,
    exts:      &[&str],
) -> Result<Vec<FileSource>> {
    let base_url = Url::parse(page_url)?;

    let mut files:   Vec<FileSource>    = Vec::new();
    let mut seen_f:  HashSet<String>    = HashSet::new(); // dedup by URL
    let mut seen_pg: HashSet<String>    = HashSet::new(); // dedup pages visited

    // ── Step 1: sitemap fast-path (runs in parallel with BFS start) ───────────
    let sitemap_files = scrape_all_sitemaps(client, &base_url, exts).await;
    for f in sitemap_files {
        if seen_f.insert(f.url.clone()) { files.push(f); }
    }

    // ── Step 2: BFS crawl ─────────────────────────────────────────────────────
    // Strip fragment from seed URL before inserting into the frontier
    let seed = strip_fragment(page_url);
    let mut frontier: Vec<String> = vec![seed];

    for depth in 0..=max_depth {
        if frontier.is_empty() { break; }

        // Drain frontier, skip already-visited pages
        let to_fetch: Vec<String> = frontier
            .drain(..)
            .filter(|u| seen_pg.insert(u.clone()))
            .collect();

        if to_fetch.is_empty() { continue; }

        // Fetch pages at this depth level concurrently, at most 8 in flight
        // so a link-heavy page doesn't fire hundreds of requests at once.
        // `buffered` keeps discovery order stable between runs.
        let fetched: Vec<(String, String)> = stream::iter(to_fetch)
            .map(|url| async move {
                let html = fetch_html_timeout(client, &url)
                    .await
                    .unwrap_or_default();
                (url, html)
            })
            .buffered(8)
            .collect()
            .await;

        for (url, html) in &fetched {
            if html.is_empty() { continue; }
            let Ok(base) = Url::parse(url) else { continue };
            // Parse once and reuse the DOM for both passes below.
            let doc = Html::parse_document(html);

            // Collect matching files on this page (hrefs + src attributes)
            for f in extract_file_links(&doc, &base, exts) {
                if seen_f.insert(f.url.clone()) { files.push(f); }
            }

            // Enqueue same-origin non-file links for the next level
            if depth < max_depth {
                for link in extract_internal_links(&doc, &base, exts) {
                    let clean = strip_fragment(&link);
                    if !seen_pg.contains(&clean) {
                        frontier.push(clean);
                    }
                }
            }
        }
    }

    Ok(files)
}

// =============================================================================
// Sitemap discovery
// =============================================================================

/// Tries both `/sitemap.xml` and `/sitemap_index.xml`.
/// For an index file, follows each child sitemap URL one level deep.
async fn scrape_all_sitemaps(
    client: &reqwest::Client,
    base:   &Url,
    exts:   &[&str],
) -> Vec<FileSource> {
    // `join` keeps scheme, host *and* port (e.g. http://localhost:8080).
    let candidates: Vec<String> = ["/sitemap.xml", "/sitemap_index.xml"]
        .iter()
        .filter_map(|p| base.join(p).ok())
        .map(String::from)
        .collect();

    let mut all: Vec<FileSource> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for url in &candidates {
        let Ok(resp) = client.get(url).send().await else { continue };
        if !resp.status().is_success() { continue; }
        let Ok(xml) = resp.text().await else { continue };

        let locs = parse_sitemap_locs(&xml);

        // Check if this is a sitemap index (locs are child sitemap URLs)
        let is_index = xml.contains("<sitemapindex") || xml.contains("<sitemap>");

        if is_index {
            // Fetch each child sitemap and extract file locs
            let child_results = futures::future::join_all(
                locs.iter().map(|child_url| {
                    let client = client.clone();
                    let u = child_url.clone();
                    async move {
                        let Ok(r) = client.get(&u).send().await else { return vec![] };
                        if !r.status().is_success() { return vec![]; }
                        let Ok(body) = r.text().await else { return vec![] };
                        parse_sitemap_locs(&body)
                    }
                })
            ).await;

            for child_locs in child_results {
                for loc in child_locs {
                    if matches_ext(&loc, exts) {
                        let name = clean_filename(&loc);
                        if seen.insert(loc.clone()) {
                            all.push(FileSource { name, url: loc });
                        }
                    }
                }
            }
        } else {
            for loc in locs {
                if matches_ext(&loc, exts) {
                    let name = clean_filename(&loc);
                    if seen.insert(loc.clone()) {
                        all.push(FileSource { name, url: loc });
                    }
                }
            }
        }
    }

    all
}

/// Extracts all `<loc>` text values from a sitemap XML body.
fn parse_sitemap_locs(xml: &str) -> Vec<String> {
    LOC_RE.captures_iter(xml)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().trim().to_owned())
        .collect()
}

// =============================================================================
// HTTP fetch with timeout
// =============================================================================

/// Fetches a page's HTML body with a 15-second timeout.
/// Returns an empty string on any error so the caller can skip gracefully.
async fn fetch_html_timeout(client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = tokio::time::timeout(
        Duration::from_secs(15),
        client.get(url).send(),
    )
    .await
    .map_err(|_| format!("timeout fetching {url}"))??;

    // Skip non-HTML responses (archives, media linked from the page) instead
    // of buffering a possibly huge binary just to find no links in it.
    let is_html = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|ct| ct.contains("html"));
    if !resp.status().is_success() || !is_html {
        return Ok(String::new());
    }

    Ok(resp.text().await.unwrap_or_default())
}

// =============================================================================
// HTML link extraction
// =============================================================================

/// Extracts every `<a href>`, `<source src>`, `<img src>`, and `<video src>`
/// whose resolved URL matches `exts`. URL-decodes filenames.
fn extract_file_links(doc: &Html, base: &Url, exts: &[&str]) -> Vec<FileSource> {
    let mut seen  = HashSet::new();
    let mut found = Vec::new();

    // ── <a href> ──────────────────────────────────────────────────────────────
    for el in doc.select(&LINK_SEL) {
        if let Some(f) = resolve_attr(el, "href", base, exts, &mut seen) {
            found.push(f);
        }
    }

    // ── <source src>, <img src>, <video src> ──────────────────────────────────
    for el in doc.select(&SRC_SEL) {
        if let Some(f) = resolve_attr(el, "src", base, exts, &mut seen) {
            found.push(f);
        }
    }

    found
}

/// Resolves a single element attribute to an absolute URL, checks extension,
/// deduplicates, and returns a `FileSource` if it qualifies.
fn resolve_attr(
    el:   ElementRef,
    attr: &str,
    base: &Url,
    exts: &[&str],
    seen: &mut HashSet<String>,
) -> Option<FileSource> {
    let raw     = el.value().attr(attr)?;
    let abs     = base.join(raw).ok()?;
    let url_str = strip_fragment(abs.as_str());

    // Check extension against the path only (ignore query string)
    if !matches_ext(abs.path(), exts) { return None; }
    if !seen.insert(url_str.clone()) { return None; }

    let name = clean_filename(&url_str);
    Some(FileSource { name, url: url_str })
}

/// Returns all same-origin, non-file `<a href>` links for BFS expansion.
/// Strips fragments and skips anchors, mailto:, javascript:, and tel: links.
fn extract_internal_links(doc: &Html, base: &Url, exts: &[&str]) -> Vec<String> {
    let origin = base.origin();
    let mut seen = HashSet::new();

    doc.select(&LINK_SEL)
        .filter_map(|el: ElementRef| {
            let raw  = el.value().attr("href")?;

            // Skip non-navigable schemes
            if raw.starts_with("mailto:")
                || raw.starts_with("javascript:")
                || raw.starts_with("tel:")
                || raw.starts_with('#')
            {
                return None;
            }

            let abs  = base.join(raw).ok()?;
            let url  = strip_fragment(abs.as_str());

            // Compare real origins: a string prefix check let
            // `https://example.com.evil.org/` through for `https://example.com`.
            if abs.origin() != origin            { return None; } // external
            if matches_ext(abs.path(), exts)     { return None; } // it's a file, not a page
            if seen.insert(url.clone()) { Some(url) } else { None }
        })
        .collect()
}

// =============================================================================
// Utilities
// =============================================================================

/// Removes the fragment (`#section`) from a URL string.
fn strip_fragment(url: &str) -> String {
    match url.find('#') {
        Some(i) => url[..i].to_owned(),
        None    => url.to_owned(),
    }
}

/// Derives a clean, URL-decoded filename from a URL (`%20` → space),
/// falling back to "unknown".
fn clean_filename(url: &str) -> String {
    filename_from_url(url).unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_links_stay_on_same_origin() {
        let base = Url::parse("https://example.com/docs/").unwrap();
        let html = r#"
            <a href="/about">same</a>
            <a href="https://example.com.evil.org/x">lookalike</a>
            <a href="https://example.community/y">prefix</a>
            <a href="http://example.com/z">other scheme</a>
        "#;
        assert_eq!(
            extract_internal_links(&Html::parse_document(html), &base, &["pdf"]),
            vec!["https://example.com/about".to_owned()]
        );
    }
}
