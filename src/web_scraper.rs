//! Generic web scraper for marcopolo v0.//! Generic web scraper for marcopolo v0.3
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
    time::Duration,
};

use percent_encoding::percent_decode_str;
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::{matches_ext, FileSource, Result};

// ── Compile selectors once at startup so they are not rebuilt per page ────────
fn link_sel()   -> Selector { Selector::parse("a[href]").unwrap() }
fn src_sel()    -> Selector { Selector::parse("source[src], img[src], video[src]").unwrap() }

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

        // Fetch all pages at this depth level concurrently
        let fetched = futures::future::join_all(
            to_fetch.into_iter().map(|url| {
                let client = client.clone();
                async move {
                    let html = fetch_html_timeout(&client, &url)
                        .await
                        .unwrap_or_default();
                    (url, html)
                }
            })
        ).await;

        for (url, html) in &fetched {
            if html.is_empty() { continue; }
            let Ok(base) = Url::parse(url) else { continue };

            // Collect matching files on this page (hrefs + src attributes)
            for f in extract_file_links(html, &base, exts) {
                if seen_f.insert(f.url.clone()) { files.push(f); }
            }

            // Enqueue same-origin non-file links for the next level
            if depth < max_depth {
                for link in extract_internal_links(html, &base, exts) {
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
    let root = format!("{}://{}", base.scheme(), base.host_str().unwrap_or(""));

    let candidates = [
        format!("{root}/sitemap.xml"),
        format!("{root}/sitemap_index.xml"),
    ];

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
    let re = Regex::new(r"<loc>\s*(https?://[^\s<]+)\s*</loc>").unwrap();
    re.captures_iter(xml)
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

    if !resp.status().is_success() {
        return Ok(String::new());
    }

    Ok(resp.text().await.unwrap_or_default())
}

// =============================================================================
// HTML link extraction
// =============================================================================

/// Extracts every `<a href>`, `<source src>`, `<img src>`, and `<video src>`
/// whose resolved URL matches `exts`. URL-decodes filenames.
fn extract_file_links(html: &str, base: &Url, exts: &[&str]) -> Vec<FileSource> {
    let doc       = Html::parse_document(html);
    let mut seen  = HashSet::new();
    let mut found = Vec::new();

    // ── <a href> ──────────────────────────────────────────────────────────────
    let link_selector = link_sel();
    for el in doc.select(&link_selector) {
        if let Some(f) = resolve_attr(el, "href", base, exts, &mut seen) {
            found.push(f);
        }
    }

    // ── <source src>, <img src>, <video src> ──────────────────────────────────
    let src_selector = src_sel();
    for el in doc.select(&src_selector) {
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
    let url_str = strip_fragment(&abs.to_string());

    // Check extension against the path only (ignore query string)
    if !matches_ext(abs.path(), exts) { return None; }
    if !seen.insert(url_str.clone()) { return None; }

    let name = clean_filename(&url_str);
    Some(FileSource { name, url: url_str })
}

/// Returns all same-origin, non-file `<a href>` links for BFS expansion.
/// Strips fragments and skips anchors, mailto:, javascript:, and tel: links.
fn extract_internal_links(html: &str, base: &Url, exts: &[&str]) -> Vec<String> {
    let doc    = Html::parse_document(html);
    let sel    = link_sel();
    let origin = format!("{}://{}", base.scheme(), base.host_str().unwrap_or(""));
    let mut seen = HashSet::new();

    doc.select(&sel)
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
            let url  = strip_fragment(&abs.to_string());

            if !url.starts_with(&origin)         { return None; } // external
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

/// Derives a clean filename from a URL:
///   - Takes the last path segment
///   - Strips the query string
///   - URL-decodes percent-encoded characters (%20 → space)
///   - Falls back to "unknown" if the segment is empty
fn clean_filename(url: &str) -> String {
    let path_part = url.split('?').next().unwrap_or(url);
    let raw_name  = path_part.split('/').last().unwrap_or("unknown");
    let decoded   = percent_decode_str(raw_name)
        .decode_utf8()
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| raw_name.to_owned());
    if decoded.is_empty() { "unknown".to_owned() } else { decoded }
}
