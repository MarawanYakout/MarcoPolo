use reqwest::Client;
use serde::Deserialize;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use super::types::FindResult;

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
    #[serde(default)]
    truncated: bool,
}

#[derive(Deserialize)]
struct TreeItem {
    path: String,
    #[serde(rename = "type")]
    item_type: String,
}

pub async fn search(client: &Client, query: &str) -> Vec<FindResult> {
    let mut results = Vec::new();
    let encoded_query = utf8_percent_encode(query, NON_ALPHANUMERIC).to_string();
    let search_url = format!("https://api.github.com/search/repositories?q={}&per_page=3", encoded_query);

    let search_resp = match client.get(&search_url).send().await {
        Ok(r) => r,
        Err(_) => return results,
    };

    if !search_resp.status().is_success() {
        return results;
    }

    let search_data: RepoSearchResp = match search_resp.json().await {
        Ok(d) => d,
        Err(_) => return results,
    };

    for repo in search_data.items {
        let tree_url = format!(
            "https://api.github.com/repos/{}/git/trees/{}?recursive=1",
            repo.full_name, repo.default_branch
        );

        let tree_resp = match client.get(&tree_url).send().await {
            Ok(r) => r,
            Err(_) => continue,
        };

        if !tree_resp.status().is_success() {
            continue;
        }

        let tree_data: TreeResp = match tree_resp.json().await {
            Ok(d) => d,
            Err(_) => continue,
        };

        for item in tree_data.tree {
            if item.item_type == "blob" && item.path.to_lowercase().ends_with(".pdf") {
                let filename = item.path.split('/').last().unwrap_or(&item.path).to_string();
                let url = format!(
                    "https://raw.githubusercontent.com/{}/{}/{}",
                    repo.full_name, repo.default_branch, item.path
                );
                
                results.push(FindResult {
                    source: "github.com",
                    title: format!("{} - {}", repo.full_name, filename),
                    url,
                    format: Some("pdf".to_string()),
                    score: None,
                    filename,
                });
            }
        }
    }

    results
}
