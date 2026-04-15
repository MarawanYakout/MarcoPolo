use reqwest::Client;
use scraper::{Html, Selector};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use super::types::FindResult;

pub async fn search(client: &Client, query: &str) -> Vec<FindResult> {
    let mut results = Vec::new();
    let encoded_query = utf8_percent_encode(query, NON_ALPHANUMERIC).to_string();
    let url = format!("https://scholar.google.com/scholar?q={}", encoded_query);

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
    let row_selector = Selector::parse(".gs_r.gs_or.gs_scl").unwrap();
    let title_selector = Selector::parse(".gs_rt a").unwrap();
    let pdf_link_selector = Selector::parse(".gs_or_ggsm a").unwrap();

    for row in document.select(&row_selector) {
        if let Some(pdf_elem) = row.select(&pdf_link_selector).next() {
            if let Some(href) = pdf_elem.value().attr("href") {
                if href.to_lowercase().ends_with(".pdf") || pdf_elem.text().collect::<String>().contains("[PDF]") {
                    let title = if let Some(title_elem) = row.select(&title_selector).next() {
                        title_elem.text().collect::<Vec<_>>().join(" ").trim().to_string()
                    } else {
                        "Google Scholar PDF".to_string()
                    };

                    let clean_title = title.replace(|c: char| !c.is_alphanumeric() && c != ' ', "_");
                    let filename = format!("{}.pdf", clean_title);

                    results.push(FindResult {
                        source: "scholar.google.com",
                        title,
                        url: href.to_string(),
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
