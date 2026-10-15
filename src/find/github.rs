//! GitHub search: finds repositories matching the query, then the PDFs
//! inside them whose file names match the query.

use reqwest::Client;
use serde::Deserialize;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use crate::utils::parsing::encode_path;
use crate::utils::relevance::relevance;
use super::types::FindResult;

/// Book-collection repos can hold hundreds of PDFs; only keep files whose
/// name covers at least half the query words, and at most this many.
const MIN_SCORE:   f32   = 0.5;
const MAX_RESULTS: usize = 8;

#[derive(Deserialize)]
struct RepoSearchResp {
    items: Vec<RepoItem>,
}

#[derive(Deserialize)]
struct RepoItem {
    full_name: String,
    default_branch: String,
}

#[derive(Deserialize)]
struct TreeResp {
    tree: Vec<TreeItem>,
}

#[derive(Deserialize)]
struct TreeItem {
    path: String,
    #[serde(rename = "type")]
    item_type: String,
}

pub async fn search(client: &Client, query: &str) -> Vec<FindResult> {
    let encoded_query = utf8_percent_encode(query, NON_ALPHANUMERIC).to_string();
    let search_url = format!("https://api.github.com/search/repositories?q={}&per_page=3", encoded_query);

    let search_resp = match client.get(&search_url).send().await {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    if !search_resp.status().is_success() {
        return vec![];
    }

    let search_data: RepoSearchResp = match search_resp.json().await {
        Ok(d) => d,
        Err(_) => return vec![],
    };

    // Fetch the repo trees concurrently rather than one after another.
    let per_repo = futures::future::join_all(
        search_data.items.iter().map(|repo| repo_pdfs(client, repo, query)),
    ).await;

    let mut results: Vec<(f32, FindResult)> = per_repo.into_iter().flatten().collect();
    results.sort_by(|a, b| b.0.total_cmp(&a.0));
    results.into_iter().take(MAX_RESULTS).map(|(_, r)| r).collect()
}

/// PDFs in `repo` whose file name matches `query`, with their scores.
async fn repo_pdfs(client: &Client, repo: &RepoItem, query: &str) -> Vec<(f32, FindResult)> {
    let tree_url = format!(
        "https://api.github.com/repos/{}/git/trees/{}?recursive=1",
        repo.full_name,
        encode_path(&repo.default_branch)
    );

    let tree_resp = match client.get(&tree_url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return vec![],
    };
    let tree_data: TreeResp = match tree_resp.json().await {
        Ok(d) => d,
        Err(_) => return vec![],
    };

    tree_data
        .tree
        .into_iter()
        .filter(|item| item.item_type == "blob" && item.path.to_lowercase().ends_with(".pdf"))
        .filter_map(|item| {
            let filename = item.path.rsplit('/').next().unwrap_or(&item.path).to_string();
            let score = relevance(query, &filename);
            if score < MIN_SCORE {
                return None;
            }
            let url = format!(
                "https://raw.githubusercontent.com/{}/{}/{}",
                repo.full_name,
                encode_path(&repo.default_branch),
                encode_path(&item.path)
            );
            Some((score, FindResult {
                source: "github.com",
                title: format!("{} - {}", repo.full_name, filename),
                url,
                format: Some("pdf".to_string()),
                score: None,
                filename,
            }))
        })
        .collect()
}
