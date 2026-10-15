//! marcopolo — multi-type file hunter & downloader + free-book search engine
//!
//! # Subcommands
//! ```text
//! marcopolo scrape <URL> [OPTIONS]   — crawl a GitHub repo or any website
//! marcopolo find   <QUERY> [OPTIONS] — search Archive.org, Open Library,
//!                                      Gutenberg, Anna's Archive, GitHub,
//!                                      Google Scholar, and DuckDuckGo
//! ```

mod cli;
mod download;
mod find;
mod utils;
mod web_scraper;

// ── Standard library ──────────────────────────────────────────────────────────
use std::{
    collections::HashSet,
    io::{self, Write},
    path::Path,
    sync::LazyLock,
    time::Duration,
};

// ── Third-party ───────────────────────────────────────────────────────────────
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use clap::{Parser, ValueEnum};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use reqwest::{header, Client};
use serde::Deserialize;
use url::Url;

use download::{download_all, DownloadOpts};
use utils::parsing::{encode_path, filename_from_url, safe_filename, unique_filename};

// ── Error alias ───────────────────────────────────────────────────────────────
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// =============================================================================
// File kinds
// =============================================================================

/// File categories the user can request.
#[derive(Debug, Clone, PartialEq, Eq, ValueEnum)]
pub enum FileKind {
    /// PDF documents (.pdf)
    Pdf,
    /// Text & document formats (.txt .md .epub .doc .docx .csv .rst)
    Text,
    /// Image formats (.jpg .jpeg .png .gif .svg .webp .bmp .ico)
    Img,
    /// Video formats (.mp4 .mkv .avi .mov .webm .flv .m4v)
    Video,
}

impl FileKind {
    pub fn extensions(&self) -> &'static [&'static str] {
        match self {
            Self::Pdf   => &["pdf"],
            Self::Text  => &["txt", "md", "epub", "doc", "docx", "csv", "rst"],
            Self::Img   => &["jpg", "jpeg", "png", "gif", "svg", "webp", "bmp", "ico"],
            Self::Video => &["mp4", "mkv", "avi", "mov", "webm", "flv", "m4v"],
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Pdf   => "PDF",
            Self::Text  => "text",
            Self::Img   => "image",
            Self::Video => "video",
        }
    }
}

/// Flattens a slice of `FileKind` into a deduplicated list of extension strings.
/// Uses a HashSet to correctly remove duplicates regardless of ordering.
pub fn all_extensions(kinds: &[FileKind]) -> Vec<&'static str> {
    let mut seen = std::collections::HashSet::new();
    kinds
        .iter()
        .flat_map(|k| k.extensions().iter().copied())
        .filter(|e| seen.insert(*e))
        .collect()
}

/// Returns `true` if `path` (or a URL) ends with one of the supplied extensions.
pub fn matches_ext(path: &str, exts: &[&str]) -> bool {
    let lower = path.to_lowercase();
    let clean = lower.split(['?', '#']).next().unwrap_or(&lower);
    exts.iter().any(|e| clean.ends_with(&format!(".{e}")))
}

// =============================================================================
// Data models
// =============================================================================

/// A file discovered from any source — repo tree, README, release, or web page.
#[derive(Debug, Clone)]
pub struct FileSource {
    pub name: String,
    pub url:  String,
}

#[derive(Debug, Deserialize)]
struct GitTree {
    tree:      Vec<TreeEntry>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Debug, Deserialize)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct ReadmeResponse {
    content:  String,
    encoding: String,
}

#[derive(Debug, Deserialize)]
struct RepoInfo {
    default_branch: String,
}

#[derive(Debug, Deserialize)]
struct Release {
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct ReleaseAsset {
    name:                 String,
    browser_download_url: String,
}

// =============================================================================
// URL parsing
// =============================================================================

/// `true` only when the URL's host really is GitHub — a substring check would
/// also match `https://example.com/?ref=github.com`.
fn is_github_url(raw: &str) -> bool {
    Url::parse(raw)
        .ok()
        .and_then(|u| u.host_str().map(|h| h == "github.com" || h == "www.github.com"))
        .unwrap_or(false)
}

/// Parses a GitHub URL and returns (owner, repo, subpath, branch_from_url).
fn parse_github_url(raw: &str) -> Result<(String, String, Option<String>, Option<String>)> {
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

    let owner = segs[0].to_owned();
    let repo  = segs[1].strip_suffix(".git").unwrap_or(segs[1]).to_owned();

    let branch_from_url = if segs.len() > 3 && segs[2] == "tree" {
        Some(segs[3].to_owned())
    } else {
        None
    };

    let subpath = if segs.len() > 4 && segs[2] == "tree" {
        Some(segs[4..].join("/"))
    } else {
        None
    };

    Ok((owner, repo, subpath, branch_from_url))
}

// =============================================================================
// GitHub API helpers
// =============================================================================

/// GitHub answers 403/429 when the unauthenticated limit (60 req/hr) is used
/// up; say how to fix it instead of showing a bare status code.
fn github_error(e: reqwest::Error) -> Box<dyn std::error::Error> {
    match e.status().map(|s| s.as_u16()) {
        Some(403 | 429) => format!(
            "{e} — likely the GitHub API rate limit; set GITHUB_TOKEN to raise it to 5,000 req/hr"
        ).into(),
        _ => e.into(),
    }
}

async fn default_branch(client: &Client, owner: &str, repo: &str) -> Result<String> {
    let url  = format!("https://api.github.com/repos/{owner}/{repo}");
    let info: RepoInfo = client
        .get(&url)
        .send().await?
        .error_for_status().map_err(github_error)?
        .json().await?;
    Ok(info.default_branch)
}

async fn file_content(
    client: &Client,
    owner:  &str,
    repo:   &str,
    path:   &str,
) -> Result<String> {
    #[derive(Deserialize)]
    struct FileResp { content: String, encoding: String }

    let url  = format!("https://api.github.com/repos/{owner}/{repo}/contents/{path}");
    let resp = client.get(&url).send().await?;
    if resp.status() == 404 { return Ok(String::new()); }

    let f: FileResp = resp.error_for_status()?.json().await?;
    if f.encoding != "base64" { return Ok(String::new()); }

    let bytes = B64.decode(f.content.replace('\n', ""))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn repo_files(
    client:  &Client,
    owner:   &str,
    repo:    &str,
    branch:  &str,
    exts:    &[&str],
    subpath: Option<&str>,
) -> Result<Vec<FileSource>> {
    let branch = encode_path(branch);
    let url  = format!(
        "https://api.github.com/repos/{owner}/{repo}/git/trees/{branch}?recursive=1"
    );
    let tree: GitTree = client
        .get(&url)
        .send().await?
        .error_for_status().map_err(github_error)?
        .json().await?;

    if tree.truncated {
        eprintln!("{} tree was truncated — some files may be missed.", "warning:".yellow());
    }

    let sources = tree
        .tree
        .into_iter()
        .filter(|e| {
            let in_scope = match subpath {
                Some(sp) => e.path.starts_with(&format!("{sp}/")),
                None     => true,
            };
            e.kind == "blob" && in_scope && matches_ext(&e.path, exts)
        })
        .map(|e| {
            let raw_url = format!(
                "https://raw.githubusercontent.com/{owner}/{repo}/{branch}/{}",
                encode_path(&e.path)
            );
            let name = Path::new(&e.path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&e.path)
                .to_owned();
            FileSource { name, url: raw_url }
        })
        .collect();

    Ok(sources)
}

async fn readme_files(
    client:  &Client,
    owner:   &str,
    repo:    &str,
    exts:    &[&str],
    subpath: Option<&str>,
) -> Result<Vec<FileSource>> {
    let root_text = async {
        let url  = format!("https://api.github.com/repos/{owner}/{repo}/readme");
        let resp = client.get(&url).send().await?;
        if resp.status() == 404 { return Ok::<String, Box<dyn std::error::Error>>(String::new()); }

        let readme: ReadmeResponse = resp.error_for_status().map_err(github_error)?.json().await?;
        if readme.encoding != "base64" { return Ok(String::new()); }

        let bytes = B64.decode(readme.content.replace('\n', ""))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };

    let sub_text = async {
        match subpath {
            Some(sp) => file_content(client, owner, repo, &format!("{sp}/README.md")).await,
            None     => Ok(String::new()),
        }
    };

    let (root, sub) = tokio::join!(root_text, sub_text);

    let mut seen: HashSet<String> = HashSet::new();
    let mut all:  Vec<FileSource> = Vec::new();
    for text in [root.unwrap_or_default(), sub.unwrap_or_default()] {
        for f in extract_file_links(&text, exts) {
            if seen.insert(f.url.clone()) { all.push(f); }
        }
    }
    Ok(all)
}

async fn release_files(
    client: &Client,
    owner:  &str,
    repo:   &str,
    exts:   &[&str],
) -> Result<Vec<FileSource>> {
    // The API returns 30 releases per page by default; page through at the
    // 100 maximum (capped at 1,000 releases) so older assets aren't missed.
    const PER_PAGE: usize = 100;
    let mut files = Vec::new();
    for page in 1..=10 {
        let url = format!(
            "https://api.github.com/repos/{owner}/{repo}/releases?per_page={PER_PAGE}&page={page}"
        );
        let releases: Vec<Release> = client
            .get(&url)
            .send().await?
            .error_for_status().map_err(github_error)?
            .json().await?;
        let last_page = releases.len() < PER_PAGE;

        files.extend(
            releases
                .into_iter()
                .flat_map(|r| r.assets)
                .filter(|a| matches_ext(&a.name, exts))
                .map(|a| FileSource { name: a.name, url: a.browser_download_url }),
        );
        if last_page { break; }
    }

    Ok(files)
}

static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s\)\]"'<>]+"#).unwrap());

pub fn extract_file_links(text: &str, exts: &[&str]) -> Vec<FileSource> {
    // Grab every URL, then keep those whose *path* ends in a wanted extension.
    // Matching the extension inside the regex also hit `.md` in
    // `https://x.mdn.org/…` or `.doc` in `https://a.docs.rs/…`.
    let mut seen = HashSet::new();

    URL_RE.find_iter(text)
        .filter_map(|m| {
            // Drop sentence punctuation that trails a bare URL in prose.
            let raw    = m.as_str().trim_end_matches(['.', ',', ';', ':', '!']);
            let parsed = Url::parse(raw).ok()?;
            if !matches_ext(parsed.path(), exts) { return None; }

            let url = github_raw_url(&parsed).unwrap_or_else(|| raw.to_owned());
            if !seen.insert(url.clone()) { return None; }
            let name = filename_from_url(&url).unwrap_or_else(|| "unknown".to_owned());
            Some(FileSource { name, url })
        })
        .collect()
}

/// README links usually point at GitHub's HTML file viewer
/// (`github.com/o/r/blob/<ref>/<path>`); rewrite them to the raw file so we
/// download the PDF itself rather than the web page around it.
fn github_raw_url(url: &Url) -> Option<String> {
    if !matches!(url.host_str()?, "github.com" | "www.github.com") { return None; }
    let segs: Vec<&str> = url.path_segments()?.collect();
    if segs.len() < 5 || !matches!(segs[2], "blob" | "raw") { return None; }
    Some(format!(
        "https://raw.githubusercontent.com/{}/{}/{}",
        segs[0], segs[1], segs[3..].join("/"),
    ))
}

// =============================================================================
// Interactive selection helper
// =============================================================================

fn parse_selection(input: &str, count: usize) -> Vec<usize> {
    let trimmed = input.trim().to_lowercase();
    if trimmed == "a" || trimmed == "all" {
        return (0..count).collect();
    }

    let normalised = trimmed.replace(',', " ");
    let mut indices = Vec::new();

    for token in normalised.split_whitespace() {
        if let Some((lo, hi)) = token.split_once('-') {
            if let (Ok(a), Ok(b)) = (lo.trim().parse::<usize>(), hi.trim().parse::<usize>()) {
                for n in a..=b {
                    if n >= 1 && n <= count { indices.push(n - 1); }
                }
            }
        } else if let Ok(n) = token.parse::<usize>() {
            if n >= 1 && n <= count { indices.push(n - 1); }
        }
    }

    let mut seen = HashSet::new();
    indices.retain(|i| seen.insert(*i));
    indices
}

fn read_line_stdin() -> String {
    let mut buf = String::new();
    let _ = io::stdin().read_line(&mut buf);
    buf
}

// =============================================================================
// Entry point
// =============================================================================

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{} {e}", "error:".red().bold());
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    use cli::{Cli, Command};

    let mut args: Vec<String> = std::env::args().collect();
    if args.len() > 1
        && !args[1].starts_with('-')
        && args[1] != "scrape"
        && args[1] != "find"
        && args[1] != "help"
    {
        args.insert(1, "scrape".to_string());
    }
    let cli = Cli::parse_from(args);

    // GitHub token comes from the environment only — CLI args are visible to
    // every user via `ps aux`.
    let token = std::env::var("MARCOPOLO_GITHUB_TOKEN")
        .or_else(|_| std::env::var("GITHUB_TOKEN"))
        .ok()
        .filter(|t| !t.trim().is_empty());

    // Connect/read timeouts instead of a total timeout: a total timeout aborts
    // any download that legitimately takes longer than 30 s.
    let base_client = || {
        Client::builder()
            .user_agent(concat!("marcopolo/", env!("CARGO_PKG_VERSION"), " (multi-type file downloader + book finder)"))
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(30))
            // Never follow an HTTPS → HTTP redirect: on a plain-HTTP hop any
            // middlebox (ISP captive portal, hostile Wi-Fi) can swap the
            // file. The downloader re-requests such targets over HTTPS.
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let downgrade = attempt.url().scheme() == "http"
                    && attempt.previous().last().is_some_and(|u| u.scheme() == "https");
                if downgrade {
                    attempt.stop()
                } else if attempt.previous().len() >= 10 {
                    attempt.error("too many redirects")
                } else {
                    attempt.follow()
                }
            }))
    };

    // `client` talks to arbitrary hosts; `gh_client` carries the GitHub Accept
    // header and token and is used only for GitHub hosts, so the token is
    // never sent to third-party websites or search engines.
    let client = base_client().build()?;
    let gh_client = {
        let mut gh_headers = header::HeaderMap::new();
        gh_headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/vnd.github+json"),
        );
        if let Some(t) = &token {
            if let Ok(mut val) = header::HeaderValue::from_str(&format!("Bearer {t}")) {
                val.set_sensitive(true);
                gh_headers.insert(header::AUTHORIZATION, val);
            }
        }
        base_client().default_headers(gh_headers).build()?
    };

    match cli.command {
        // ── scrape subcommand ──────────────────────────────────────────────────
        Command::Scrape { url, kinds, out, depth, delay, resume, retries, jobs, list, filter, quiet } => {
            let exts   = all_extensions(&kinds);

            if !quiet {
                let kinds_label = kinds.iter()
                    .map(|k| k.label().cyan().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                println!(
                    "{} {}  [{}]",
                    "🧭 marcopolo scrape →".cyan().bold(),
                    url.yellow().bold(),
                    kinds_label,
                );
            }

            let mut all: Vec<FileSource> = if is_github_url(&url) {
                let (owner, repo, subpath, branch_from_url) = parse_github_url(&url)?;

                if !quiet {
                    if let Some(ref sp) = subpath {
                        println!("{} subdirectory scope: {}", "→".dimmed(), sp.cyan());
                    }
                    println!("{} scanning repo tree, README, and releases …", "→".dimmed());
                }

                let branch = match branch_from_url {
                    Some(b) => b,
                    None    => default_branch(&gh_client, &owner, &repo).await?,
                };
                let sp = subpath.as_deref();

                let (tree_r, readme_r, releases_r) = tokio::join!(
                    repo_files(&gh_client, &owner, &repo, &branch, &exts, sp),
                    readme_files(&gh_client, &owner, &repo, &exts, sp),
                    release_files(&gh_client, &owner, &repo, &exts),
                );

                let mut seen:  HashSet<String> = HashSet::new();
                let mut files: Vec<FileSource> = Vec::new();
                for (label, result) in [("repo tree", tree_r), ("README", readme_r), ("releases", releases_r)] {
                    match result {
                        Ok(found) => {
                            for f in found {
                                if seen.insert(f.url.clone()) { files.push(f); }
                            }
                        }
                        // Surface failures (e.g. 403 rate limit) instead of
                        // silently reporting "No files found."
                        Err(e) => eprintln!("{} {label} scan failed: {e}", "warning:".yellow()),
                    }
                }
                files
            } else {
                if !quiet {
                    println!(
                        "{} scanning web page (depth: {}) …",
                        "→".dimmed(),
                        depth.to_string().cyan(),
                    );
                }
                web_scraper::scrape_files(&client, &url, depth, &exts).await?
            };

            if let Some(ref kw) = filter {
                let kw_lower = kw.to_lowercase();
                all.retain(|f| f.name.to_lowercase().contains(&kw_lower));
                if !quiet {
                    println!(
                        "{} filter \"{}\" → {} match(es)",
                        "→".dimmed(), kw.cyan(), all.len().to_string().green(),
                    );
                }
            }

            if all.is_empty() {
                if !quiet { println!("{}", "No files found.".yellow()); }
                return Ok(());
            }

            // Names come from remote URLs/assets: confine them to `out` and
            // keep same-named files from different folders from overwriting
            // each other.
            let mut used = HashSet::new();
            for f in &mut all {
                f.name = unique_filename(&safe_filename(&f.name), &mut used);
            }

            if !quiet {
                println!(
                    "{} {} file(s) discovered",
                    "✓".green().bold(),
                    all.len().to_string().green().bold(),
                );
            }

            if list {
                println!("\n{}", "Discovered files:".cyan().bold());
                for (i, f) in all.iter().enumerate() {
                    println!(
                        "  {}  {}  {}",
                        format!("[{}]", i + 1).dimmed(),
                        f.name.green(),
                        f.url.dimmed(),
                    );
                }
                return Ok(());
            }

            let opts = DownloadOpts { jobs: jobs.into(), resume, delay_ms: delay, retries, quiet };
            let summary = download_all(&client, &gh_client, &all, &out, &opts).await?;
            if summary.failed > 0 {
                // Non-zero exit so scripts notice partial failures.
                return Err(format!("{} download(s) failed", summary.failed).into());
            }
        }

        // ── find subcommand ────────────────────────────────────────────────────
        Command::Find { query, list, get, source, format, out, jobs, quiet } => {
            if !utils::validation::is_valid_query(&query) {
                eprintln!("{} query must not be empty.", "error:".red().bold());
                std::process::exit(1);
            }

            if !quiet {
                let sources_label = match &source {
                    Some(s) => format!("[{s}]"),
                    None    => format!(
                        "[{}]",
                        utils::validation::Source::ALL
                            .iter()
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>()
                            .join(" · "),
                    ),
                };
                println!(
                    "{} \"{}\"  {}",
                    "🔍 marcopolo find →".cyan().bold(),
                    query.yellow().bold(),
                    sources_label,
                );
            }

            let pb_spin = if quiet {
                ProgressBar::hidden()
            } else {
                let b = ProgressBar::new_spinner();
                b.set_style(
                    ProgressStyle::default_spinner()
                        .template("{spinner:.cyan} {msg}")
                        .unwrap(),
                );
                b.set_message("Searching all sources …");
                b.enable_steady_tick(std::time::Duration::from_millis(80));
                b
            };

            let mut results = find::search_all(&client, &gh_client, &query, source).await;
            pb_spin.finish_and_clear();

            if let Some(fmt) = format {
                let fmt = fmt.to_string();
                results.retain(|r| r.format.as_deref() == Some(fmt.as_str()));
            }

            if results.is_empty() {
                if !quiet { println!("{} No results found for \"{}\".", "✗".red(), query); }
                return Ok(());
            }

            if !quiet {
                println!(
                    "\n{} {} result(s) for \"{}\":\n",
                    "✓".green().bold(),
                    results.len().to_string().green().bold(),
                    query.yellow(),
                );
                for (i, r) in results.iter().enumerate() {
                    let fmt_tag = r.format
                        .as_deref()
                        .map(|f| format!(" [{}]", f.to_uppercase()))
                        .unwrap_or_default();
                    println!(
                        "  {}. ({})  {}{}",
                        format!("{:>2}", i + 1).bold(),
                        r.source.yellow(),
                        r.title.green(),
                        fmt_tag.cyan(),
                    );
                    println!("      {}", r.url.dimmed());
                    println!();
                }
            }

            if list { return Ok(()); }

            let chosen: Vec<usize> = if get {
                // Top result from each source, as documented for --get.
                let mut seen_sources = HashSet::new();
                (0..results.len())
                    .filter(|&i| seen_sources.insert(results[i].source))
                    .collect()
            } else if quiet {
                // In quiet mode without --get there is nothing to select interactively;
                // just download everything.
                (0..results.len()).collect()
            } else {
                println!(
                    "{}",
                    "──────────────────────────────────────────────".dimmed(),
                );
                println!(
                    "  {} enter number(s) to download, {} for all, {} to quit",
                    "select:".cyan().bold(),
                    "a".green().bold(),
                    "q".red().bold(),
                );
                println!(
                    "  {} {}",
                    "examples:".dimmed(),
                    "1   |   1 3   |   2,4   |   1-3   |   a".dimmed(),
                );
                println!(
                    "{}",
                    "──────────────────────────────────────────────".dimmed(),
                );

                let mut attempts = 0usize;
                loop {
                    print!("{} ", "→".cyan().bold());
                    io::stdout().flush().ok();
                    let raw     = read_line_stdin();
                    let trimmed = raw.trim().to_lowercase();
                    if trimmed == "q" || trimmed == "quit" || trimmed.is_empty() {
                        println!("{} nothing downloaded.", "✗".dimmed());
                        return Ok(());
                    }
                    let selection = parse_selection(&trimmed, results.len());
                    if !selection.is_empty() { break selection; }
                    attempts += 1;
                    if attempts >= 3 {
                        println!("{} no valid selection — aborting.", "✗".red());
                        return Ok(());
                    }
                    eprintln!(
                        "{} unrecognised input — enter a number (e.g. {}) or {} for all",
                        "!".yellow().bold(), "1".cyan(), "a".green(),
                    );
                }
            };

            if chosen.is_empty() { return Ok(()); }

            // Archive.org / Open Library hits only carry an item id; look up
            // the item's real public PDF (concurrently) before downloading.
            let urls = futures::future::join_all(
                chosen.iter().map(|&i| find::archive::resolve_download_url(&client, &results[i].url, &query)),
            ).await;

            // Many results share a title (e.g. several "Clean Code" scans), so
            // give each download its own filename instead of overwriting.
            let mut used = HashSet::new();
            let files: Vec<FileSource> = chosen
                .iter()
                .zip(urls)
                .map(|(&i, url)| FileSource {
                    name: unique_filename(&safe_filename(&results[i].filename), &mut used),
                    url,
                })
                .collect();

            if !quiet {
                println!(
                    "\n{} downloading {} file(s) → {}",
                    "↓".cyan().bold(),
                    files.len(),
                    out.display().to_string().cyan(),
                );
            }

            let opts = DownloadOpts { jobs: jobs.into(), resume: false, delay_ms: None, retries: 3, quiet };
            let summary = download_all(&client, &gh_client, &files, &out, &opts).await?;
            if summary.failed > 0 {
                return Err(format!("{} download(s) failed", summary.failed).into());
            }
        }
    }

    Ok(())
}

// =============================================================================
// Tests — selection parser
// =============================================================================

#[cfg(test)]
mod tests {
    use super::{extract_file_links, is_github_url, matches_ext, parse_github_url, parse_selection};

    #[test]
    fn select_single() { assert_eq!(parse_selection("1", 5), vec![0]); }

    #[test]
    fn select_multiple_space() { assert_eq!(parse_selection("1 3 5", 5), vec![0, 2, 4]); }

    #[test]
    fn select_multiple_comma() { assert_eq!(parse_selection("2,4", 5), vec![1, 3]); }

    #[test]
    fn select_range() { assert_eq!(parse_selection("1-3", 5), vec![0, 1, 2]); }

    #[test]
    fn select_all_a() { assert_eq!(parse_selection("a", 3), vec![0, 1, 2]); }

    #[test]
    fn select_all_word() { assert_eq!(parse_selection("all", 3), vec![0, 1, 2]); }

    #[test]
    fn select_out_of_range_ignored() { assert_eq!(parse_selection("1 6", 5), vec![0]); }

    #[test]
    fn select_zero_ignored() { assert_eq!(parse_selection("0 1", 5), vec![0]); }

    #[test]
    fn select_deduplication() { assert_eq!(parse_selection("1 1 2", 5), vec![0, 1]); }

    #[test]
    fn select_invalid_returns_empty() { assert!(parse_selection("foo bar", 5).is_empty()); }

    #[test]
    fn select_q_returns_empty() { assert!(parse_selection("q", 5).is_empty()); }

    #[test]
    fn github_host_detection() {
        assert!(is_github_url("https://github.com/owner/repo"));
        assert!(!is_github_url("https://example.com/?ref=github.com"));
    }

    #[test]
    fn github_url_strips_dot_git() {
        let (_, repo, _, _) = parse_github_url("https://github.com/owner/repo.git").unwrap();
        assert_eq!(repo, "repo");
    }

    #[test]
    fn matches_ext_ignores_fragment() {
        assert!(matches_ext("https://x.org/a.pdf#page=2", &["pdf"]));
    }

    #[test]
    fn readme_links_match_path_extension_only() {
        let text = "see https://developer.mdn.org/guide and \
                    [book](https://x.org/files/book.md). Also https://x.org/a.pdf, done";
        let found = extract_file_links(text, &["md", "pdf"]);
        let urls: Vec<_> = found.iter().map(|f| f.url.as_str()).collect();
        assert_eq!(urls, ["https://x.org/files/book.md", "https://x.org/a.pdf"]);
        assert_eq!(found[0].name, "book.md");
    }
}
