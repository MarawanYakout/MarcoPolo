//! Anna's Archive search via HTML scraping.
//!
//! Anna's Archive is the largest shadow-library catalogue aggregator.
//! Because it has no public JSON API, this module parses the HTML search
//! results page.  The CSS selectors may need updating if the site layout
//! changes — that's the only file you'd need to touch.
//!
//! ⚠️  This source is rate-limited and may return challenge pages.  It is
//! treated as a fallback: a parse failure is silently ignored and returns
//! an empty list rather than propagating an error.

use reqwest::Client;
use scraper::{Html, Selector};
use url::Url;
use crate::utils::slug::slug;
use super::types::FindResult;

const BASE: &str = "https://annas-archive.org";
const UA:   &str = "Mozilla/5.0 (compatible; marcopolo/0.4)";

/// Search Anna's Archive for `query` and return up to 5 results.
pub async fn search(client: &Client, query: &str) -> Vec<FindResult> {
    let mut url = match Url::parse(&format!("{BASE}/search")) {
        Ok(u)  => u,
        Err(_) => return vec![],
    };
    url.query_pairs_mut().append_pair("q", query);

    let resp = match client
        .get(url)
        .header("User-Agent", UA)
        .send()
        .await
    {
        Ok(r)  => r,
        Err(_) => return vec![],
    };
    let html = match resp.text().await {
        Ok(h)  => h,
        Err(_) => return vec![],
    };

    parse_results(&html)
}

// ── HTML parsing ──────────────────────────────────────────────────────────────

/// Parse search result rows from the Anna's Archive HTML response.
///
/// The selectors target the search-result `<a>` elements that link to book
/// detail pages.  Kept in a separate function so it can be unit-tested
/// with fixture HTML without making real HTTP requests.
fn parse_results(html: &str) -> Vec<FindResult> {
    // Anna's Archive result items — each is an <a> with a book detail href.
    let item_sel = match Selector::parse("a.js-vim-focus") {
        Ok(s)  => s,
        Err(_) => return vec![],
    };
    let title_sel = match Selector::parse(".line-clamp-2") {
        Ok(s)  => s,
        Err(_) => return vec![],
    };
    let meta_sel  = match Selector::parse(".text-sm.text-gray-500") {
        Ok(s)  => s,
        Err(_) => return vec![],
    };

    let doc  = Html::parse_document(html);
    let mut out = vec![];

    for el in doc.select(&item_sel).take(5) {
        let href = match el.value().attr("href") {
            Some(h) => h,
            None    => continue,
        };

        let title: String = el
            .select(&title_sel)
            .next()
            .map(|e| e.text().collect::<String>().trim().to_owned())
            .unwrap_or_default();
        if title.is_empty() { continue; }

        // Try to extract the format from the metadata line (e.g. "PDF, 4.2 MB")
        let format: Option<String> = el
            .select(&meta_sel)
            .next()
            .and_then(|e| {
                let text = e.text().collect::<String>().to_lowercase();
                ["pdf", "epub", "mobi", "djvu", "txt"]
                    .iter()
                    .find(|&&ext| text.contains(ext))
                    .map(|&ext| ext.to_owned())
            });

        let detail_url = if href.starts_with("http") {
            href.to_owned()
        } else {
            format!("{BASE}{href}")
        };

        let ext      = format.as_deref().unwrap_or("pdf");
        let filename = format!("{}.{ext}", slug(&title));

        out.push(FindResult {
            source:   "annas-archive.org",
            title,
            url:      detail_url,
            format,
            score:    None,
            filename,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::parse_results;

    /// Minimal fixture HTML that matches the selectors used in parse_results.
    const FIXTURE: &str = r#"
    <html><body>
        <a class="js-vim-focus" href="/md5/abc123">
            <div class="line-clamp-2">Clean Code</div>
            <div class="text-sm text-gray-500">PDF, 3.5 MB, English</div>
        </a>
        <a class="js-vim-focus" href="/md5/def456">
            <div class="line-clamp-2">The Pragmatic Programmer</div>
            <div class="text-sm text-gray-500">EPUB, 1.2 MB, English</div>
        </a>
        <!-- item without title should be skipped -->
        <a class="js-vim-focus" href="/md5/bad000">
            <div class="line-clamp-2"></div>
        </a>
    </body></html>
    "#;

    #[test]
    fn parses_two_results() {
        let results = parse_results(FIXTURE);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn first_result_title() {
        let results = parse_results(FIXTURE);
        assert_eq!(results[0].title, "Clean Code");
    }

    #[test]
    fn first_result_format_pdf() {
        let results = parse_results(FIXTURE);
        assert_eq!(results[0].format, Some("pdf".to_owned()));
    }

    #[test]
    fn second_result_format_epub() {
        let results = parse_results(FIXTURE);
        assert_eq!(results[1].format, Some("epub".to_owned()));
    }

    #[test]
    fn url_prefixed_with_base() {
        let results = parse_results(FIXTURE);
        assert!(results[0].url.starts_with("https://annas-archive.org"));
    }

    #[test]
    fn skips_empty_title_item() {
        // The fixture has 3 <a> elements but the third has no title.
        let results = parse_results(FIXTURE);
        assert!(!results.iter().any(|r| r.title.is_empty()));
    }

    #[test]
    fn empty_html_returns_empty_vec() {
        assert!(parse_results("<html></html>").is_empty());
    }
}
