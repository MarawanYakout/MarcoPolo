//! marcopolo — GitHub PDF hunter & downloader
//!
//! Discovers PDF files in a GitHub repository through **two channels**:
//!   1. Files committed directly to the repo (detected via the Git Trees API).
//!   2. HTTP(S) links inside the README that end in `.pdf`.
//!
//! For non-GitHub URLs, falls back to a generic web scraper that scans
//! the page (and one level of internal links) for PDF hrefs.
//!
//! All discovered PDFs are downloaded concurrently into `./downloads/`.
//!
//! # Usage
//! ```text
//! marcopolo <url>
//!
//! marcopolo https://github.com/owner/repo
//! marcopolo https://github.com/owner/repo?tab=readme-ov-file
//! marcopolo https://somesite.com/wiki/index
//! ```

mod web_scraper;

// ── Standard library ──────────────────────────────────────────────────────────
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

// ── Third-party ───────────────────────────────────────────────────────────────
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use colored::Colorize;
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use reqwest::Client;
use serde::Deserialize;
use url::Url;

// ── Error alias ───────────────────────────────────────────────────────────────
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// =============================================================================
// Data models
// =============================================================================

/// A PDF discovered either in the repo tree, via a README hyperlink,
/// or scraped from a generic web page.
#[derive(Debug, Clone)]
pub struct PdfSource {
    /// Filename used when saving to disk.
    pub name: String,
    /// Full URL from which the PDF will be fetched.
    pub url: String,
}

/// Response from `GET /repos/{owner}/{repo}/git/trees/{sha}?recursive=1`.
#[derive(Debug, Deserialize)]
struct GitTree {
    tree:      Vec<TreeEntry>,
    #[serde(default)]
    truncated: bool,
}

/// One node in the git tree (blob or tree).
#[derive(Debug, Deserialize)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

/// Response from `GET /repos/{owner}/{repo}/readme`.
#[derive(Debug, Deserialize)]
struct ReadmeResponse {
    content:  String, // base64-encoded, with embedded newlines
    encoding: String, // should be "base64"
}

/// Minimal repo metadata — only the default branch is needed.
#[derive(Debug, Deserialize)]
struct RepoInfo {
    default_branch: String,
}

// =============================================================================
// URL parsing
// =============================================================================

/// Extracts `(owner, repo)` from any GitHub repository URL.
///
/// Strips query strings (`?tab=readme-ov-file`) and fragments (`#section`)
/// before parsing so both plain and tab-parameterised URLs are accepted.
///
/// # Errors
/// Returns an error if the path has fewer than two non-empty segments.
fn parse_github_url(raw: &str) -> Result<(String, String)> {
    let no_query = raw.split('?').next().unwrap_or(raw);
    let no_frag  = no_query.split('#').next().unwrap_or(no_query);

    let parsed: Url = Url::parse(no_frag)?;
    let segs: Vec<&str> = parsed
        .path_segments()
        .ok_or("URL has no path segments")?
        .filter(|s| !s.is_empty())
        .collect();

    if segs.len() < 2 {
        return Err("Expected https://github.com/<owner>/<repo>".into());
    }
    Ok((segs[0].to_owned(), segs[1].to_owned()))
}

// =============================================================================
// GitHub API helpers
// =============================================================================

/// Fetches the repository's default branch (e.g. `"main"` or `"master"`).
async fn default_branch(client: &Client, owner: &str, repo: &str) -> Result<String> {
    let url  = format!("https://api.github.com/repos/{owner}/{repo}");
    let info: RepoInfo = client.get(&url).send().await?.error_for_status()?.json().await?;
    Ok(info.default_branch)
}

/// Returns every `.pdf` blob committed directly inside the repository.
///
/// Uses `?recursive=1` to traverse nested directories in a single request.
/// A warning is printed when GitHub truncates the response (repos > ~100k objects).
async fn repo_pdfs(
    client: &Client,
    owner:  &str,
    repo:   &str,
    branch: &str,
) -> Result<Vec<PdfSource>> {
    let url = format!(
        "https://api.github.com/repos/{owner}/{repo}/git/trees/{branch}?recursive=1"
    );
    let tree: GitTree = client.get(&url).send().await?.error_for_status()?.json().await?;

    if tree.truncated {
        eprintln!(
            "{} tree response was truncated — some deeply-nested PDFs may be missed.",
            "warning:".yellow()
        );
    }

    let sources = tree
        .tree
        .into_iter()
        .filter(|e| e.kind == "blob" && e.path.to_lowercase().ends_with(".pdf"))
        .map(|e| {
            let raw_url = format!(
                "https://raw.githubusercontent.com/{owner}/{repo}/{branch}/{}",
                e.path
            );
            let name = Path::new(&e.path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&e.path)
                .to_owned();
            PdfSource { name, url: raw_url }
        })
        .collect();

    Ok(sources)
}

/// Fetches the README and extracts every `.pdf` hyperlink it contains.
///
/// Returns an empty list (not an error) when the repo has no README.
async fn readme_pdfs(client: &Client, owner: &str, repo: &str) -> Result<Vec<PdfSource>> {
    let api_url = format!("https://api.github.com/repos/{owner}/{repo}/readme");
    let resp    = client.get(&api_url).send().await?;

    if resp.status() == 404 {
        return Ok(vec![]);
    }

    let readme: ReadmeResponse = resp.error_for_status()?.json().await?;

    if readme.encoding != "base64" {
        return Err(format!("unexpected README encoding: {}", readme.encoding).into());
    }

    let clean_b64 = readme.content.replace('\n', "");
    let bytes     = B64.decode(&clean_b64)?;
    let text      = String::from_utf8_lossy(&bytes);

    Ok(extract_pdf_links(&text))
}

/// Scans `text` for `http(s)://…pdf` URLs (bare or inside Markdown link syntax).
fn extract_pdf_links(text: &str) -> Vec<PdfSource> {
    let re = Regex::new(r#"https?://[^\s\)\]"'>]+\.(?i:pdf)(?:[^\s\)\]"'>]*)"#).unwrap();

    let mut seen = HashSet::new();
    re.find_iter(text)
        .filter_map(|m| {
            let url = m.as_str().to_owned();
            if !seen.insert(url.clone()) {
                return None;
            }
            let name = url
                .split('/')
                .last()
                .and_then(|s| s.split('?').next())
                .unwrap_or("unknown.pdf")
                .to_owned();
            Some(PdfSource { name, url })
        })
        .collect()
}

// =============================================================================
// Downloader
// =============================================================================

/// Downloads one PDF into `dir`, skipping it if the file already exists.
async fn download_pdf(
    client: &Client,
    src:    &PdfSource,
    dir:    &Path,
    pb:     &ProgressBar,
) -> Result<()> {
    let dest = dir.join(&src.name);

    if dest.exists() {
        pb.set_message(format!("{} {}", "skip".dimmed(), src.name.dimmed()));
        pb.inc(1);
        return Ok(());
    }

    pb.set_message(format!("{} {}", "↓".cyan(), src.name));

    let bytes = client
        .get(&src.url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    tokio::fs::write(&dest, &bytes).await?;
    pb.inc(1);
    Ok(())
}

// =============================================================================
// Entry point
// =============================================================================

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() != 2 {
        eprintln!("{} marcopolo <url>", "usage:".cyan().bold());
        std::process::exit(1);
    }

    println!("{} {}", "🧭 marcopolo →".cyan().bold(), args[1].yellow().bold());

    // ── Build HTTP client ──────────────────────────────────────────────────────
    let client = Client::builder()
        .user_agent("marcopolo/0.1 (github-pdf-downloader)")
        .build()
        .expect("failed to build HTTP client");

    // ── Discover PDFs — GitHub repo or generic web page ───────────────────────
    let all: Vec<PdfSource> = if args[1].contains("github.com") {

        println!("{} scanning repo tree and README …", "→".dimmed());

        let (owner, repo) = match parse_github_url(&args[1]) {
            Ok(pair) => pair,
            Err(e)   => { eprintln!("{} {e}", "error:".red().bold()); std::process::exit(1); }
        };
        let branch = match default_branch(&client, &owner, &repo).await {
            Ok(b)  => b,
            Err(e) => { eprintln!("{} {e}", "error:".red().bold()); std::process::exit(1); }
        };
        let (tree_result, readme_result) = tokio::join!(
            repo_pdfs(&client, &owner, &repo, &branch),
            readme_pdfs(&client, &owner, &repo),
        );
        let mut seen_urls: HashSet<String> = HashSet::new();
        let mut pdfs: Vec<PdfSource> = Vec::new();
        for pdf in tree_result.unwrap_or_default().into_iter()
            .chain(readme_result.unwrap_or_default())
        {
            if seen_urls.insert(pdf.url.clone()) { pdfs.push(pdf); }
        }
        pdfs

    } else {

        println!("{} scanning web page for PDFs …", "→".dimmed());
        match web_scraper::scrape_pdfs(&client, &args[1]).await {
            Ok(pdfs) => pdfs,
            Err(e)   => { eprintln!("{} {e}", "error:".red().bold()); std::process::exit(1); }
        }

    };

    // ── Guard: nothing found ───────────────────────────────────────────────────
    if all.is_empty() {
        println!("{}", "No PDF files found.".yellow());
        return;
    }

    println!(
        "{} {} PDF(s) discovered",
        "✓".green().bold(),
        all.len().to_string().green().bold(),
    );

    // ── Download ───────────────────────────────────────────────────────────────
    let dir = PathBuf::from("downloads");
    tokio::fs::create_dir_all(&dir).await.expect("cannot create ./downloads/");

    let pb = ProgressBar::new(all.len() as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.cyan} [{bar:45.cyan/blue}] {pos}/{len}  {msg}",
        )
        .unwrap()
        .progress_chars("█▓░"),
    );

    // Run up to 4 downloads concurrently — polite toward remote servers
    stream::iter(all.iter())
        .map(|src| download_pdf(&client, src, &dir, &pb))
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .for_each(|r| {
            if let Err(e) = r {
                eprintln!("{} {e}", "download error:".red());
            }
        });

    pb.finish_with_message("done ✓");
    println!(
        "\n{} all PDFs saved to {}",
        "✓".green().bold(),
        "./downloads/".cyan().bold(),
    );
}
