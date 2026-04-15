use reqwest::Client;
use scraper::{Html, Selector};
use percent_encoding::{utf8_percent_encode, percent_decode_str, NON_ALPHANUMERIC};
use super::types::FindResult;

pub async fn search(client: &Client, query: &str) -> Vec<FindResult> {
    let mut results = Vec::new();
    let full_query = format!("{} filetype:pdf", query);
    let encoded_query = utf8_percent_encode(&full_query, NON_ALPHANUMERIC).to_string();
    let url = format!("https://html.duckduckgo.com/html/?q={}", encoded_query);

    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(_) => return results,
    };

    if !resp.status().is_success() {
        return results;
    }

    let html = match resp.text().await {
        Ok(t) => t,
        Err(_) => return results,
    };

    let document = Html::parse_document(&html);
    let result_selector = Selector::parse(".result__url").unwrap();
    let title_selector = Selector::parse(".result__title").unwrap();
    let item_selector = Selector::parse(".result").unwrap();

    for item in document.select(&item_selector) {
        if let Some(link_elem) = item.select(&result_selector).next() {
            if let Some(href) = link_elem.value().attr("href") {
                let url_str = href.to_string();
                let actual_url = if url_str.contains("uddg=") {
                    url_str.split("uddg=").nth(1)
                           .and_then(|s| s.split('&').next())
                           .map(|s| percent_decode_str(s).decode_utf8_lossy().into_owned())
                           .unwrap_or(url_str)
                } else {
                    url_str
                };

                if actual_url.to_lowercase().ends_with(".pdf") {
                    let title = if let Some(title_elem) = item.select(&title_selector).next() {
                        title_elem.text().collect::<Vec<_>>().join(" ").trim().to_string()
                    } else {
                        "DuckDuckGo PDF".to_string()
                    };

                    let clean_title = title.replace(|c: char| !c.is_alphanumeric() && c != ' ', "_");
                    let filename = format!("{}.pdf", clean_title);

                    results.push(FindResult {
                        source: "duckduckgo.com",
                        title,
                        url: actual_url,
                        format: Some("pdf".to_string()),
                        score: None,
                        filename,
                    });
                }
            }
        }
    }

    results
}
