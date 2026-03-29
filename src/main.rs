//! marcopolo v0.3 — multi-type file hunter & downloader
//!
//! Supports downloading PDFs, text documents, images, and videos
//! from GitHub repositories or any website.
//!
//! # Usage
//! ```text
//! marcopolo <url> [OPTIONS]
//!
//! marcopolo https://github.com/owner/repo --type pdf --type img
//! marcopolo https://somesite.com --type video --depth 2
//! marcopolo https://github.com/owner/repo/tree/master/books --list
//! marcopolo https://somesite.com --type pdf --type text --out ~/downloads
//! ```

mod web_scraper;

// ── Standard library ──────────────────────────────────────────────────────────
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

// ── Third-party ───────────────────────────────────────────────────────────────
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use clap::{Parser, ValueEnum};
use colored::Colorize;
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use reqwest::{header, Client};
use serde::Deserialize;
use url::Url;

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
pub fn all_extensions(kinds: &[FileKind]) -> Vec<&'static str> {
    let mut exts: Vec<&'static str> = kinds
        .iter()
        .flat_map(|k| k.extensions().iter().copied())
        .collect();
    exts.dedup();
    exts
}

/// Returns `true` if `path` (or a URL) ends with one of the supplied extensions.
pub fn matches_ext(path: &str, exts: &[&str]) -> bool {
    let lower = path.to_lowercase();
    let clean = lower.split('?').next().unwrap_or(&lower);
    exts.iter().any(|e| clean.ends_with(&format!(".{e}")))
}

// =============================================================================
// CLI
// =============================================================================

/// 🧭 Hunt and download files from GitHub repos or any website.
#[derive(Parser, Debug)]
#[command(name = "marcopolo", version = "0.3.0")]
struct Args {
    /// GitHub repo URL (optionally with /tree/<branch>/<subpath>) or any website
    url: String,

    /// File types to hunt (repeatable: --type pdf --type img)
    #[arg(
        long = "type",
        short = 't',
        value_enum,
        default_values_t = vec![FileKind::Pdf],
        num_args = 1..,
    )]
    kinds: Vec<FileKind>,

    /// Output directory for downloaded files
    #[arg(long, short = 'o', default_value = "downloads")]
    out: PathBuf,

    /// Link-depth to crawl (web mode only; 0 = landing page only)
    #[arg(long, default_value_t = 1)]
    depth: usize,

    /// Milliseconds of delay between each download request
    #[arg(long)]
    delay: Option<u64>,

    /// Append to partially downloaded files instead of re-downloading
    #[arg(long = "continue", default_value_t = false)]
    resume: bool,

    /// Retry attempts per file before giving up
    #[arg(long, default_value_t = 3)]
    retries: u32,

    /// List files without downloading (dry run)
    #[arg(long, default_value_t = false)]
    list: bool,

    /// Only include files whose name contains this string (case-insensitive)
    #[arg(long)]
    filter: Option<String>,

    /// GitHub personal access token (raises rate limit 60 → 5 000 req/hr)
    #[arg(long)]
    token: Option<String>,
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

/// Extracts `(owner, repo, subpath)` from a GitHub URL.
///
/// Handles subdirectory URLs like:
///   `github.com/owner/repo/tree/branch/some/sub/path`
/// returning `subpath = Some("some/sub/path")`.
fn parse_github_url(raw: &str) -> Result<(String, String, Option<String>)> {
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
    let repo  = segs[1].to_owned();

    // /owner/repo/tree/<branch>/<subpath...>
    let subpath = if segs.len() > 4 && segs[2] == "tree" {
        Some(segs[4..].join("/"))
    } else {
        None
    };

    Ok((owner, repo, subpath))
}

// =============================================================================
// GitHub API helpers
// =============================================================================

async fn default_branch(client: &Client, owner: &str, repo: &str) -> Result<String> {
    let url  = format!("https://api.github.com/repos/{owner}/{repo}");
    let info: RepoInfo = client
        .get(&url)
        .send().await?
        .error_for_status()?
        .json().await?;
    Ok(info.default_branch)
}

/// Fetches and base64-decodes the content of a single file in the repo.
/// Returns an empty string (not an error) when the file does not exist.
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

/// Returns every blob in the repo tree whose extension matches `exts`.
/// When `subpath` is given, only blobs under that directory are returned.
async fn repo_files(
    client:  &Client,
    owner:   &str,
    repo:    &str,
    branch:  &str,
    exts:    &[&str],
    subpath: Option<&str>,
) -> Result<Vec<FileSource>> {
    let url  = format!(
        "https://api.github.com/repos/{owner}/{repo}/git/trees/{branch}?recursive=1"
    );
    let tree: GitTree = client
        .get(&url)
        .send().await?
        .error_for_status()?
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
                e.path
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

/// Decodes the root README *and* the subpath README (if given), extracts
/// every link whose extension matches `exts`. Results are deduplicated.
async fn readme_files(
    client:  &Client,
    owner:   &str,
    repo:    &str,
    exts:    &[&str],
    subpath: Option<&str>,
) -> Result<Vec<FileSource>> {
    // ── Root README via the dedicated GitHub endpoint ─────────────────────────
    let root_text = async {
        let url  = format!("https://api.github.com/repos/{owner}/{repo}/readme");
        let resp = client.get(&url).send().await?;
        if resp.status() == 404 { return Ok::<String, Box<dyn std::error::Error>>(String::new()); }

        let readme: ReadmeResponse = resp.error_for_status()?.json().await?;
        if readme.encoding != "base64" { return Ok(String::new()); }

        let bytes = B64.decode(readme.content.replace('\n', ""))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };

    // ── Subpath README (e.g. books/README.md) ────────────────────────────────
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

/// Scans all GitHub Releases for attached assets matching `exts`.
async fn release_files(
    client: &Client,
    owner:  &str,
    repo:   &str,
    exts:   &[&str],
) -> Result<Vec<FileSource>> {
    let url      = format!("https://api.github.com/repos/{owner}/{repo}/releases");
    let releases: Vec<Release> = client
        .get(&url)
        .send().await?
        .error_for_status()?
        .json().await?;

    let files = releases
        .into_iter()
        .flat_map(|r| r.assets)
        .filter(|a| matches_ext(&a.name, exts))
        .map(|a| FileSource { name: a.name, url: a.browser_download_url })
        .collect();

    Ok(files)
}

/// Extracts all matching URLs from plain text or Markdown.
///
/// BUG FIX: raw string uses single backslashes so the regex engine receives
/// `\s`, `\)`, `\.` — not `\\s`, `\\)`, `\\.` which would match literal
/// backslash characters instead of whitespace / punctuation / literal dot.
pub fn extract_file_links(text: &str, exts: &[&str]) -> Vec<FileSource> {
    let ext_alt = exts.join("|");
    // Single `\` in raw string r#"..."# → correct regex metacharacters
    let pattern = format!(r#"https?://[^\s\)\]"'>]+\.(?i:{ext_alt})(?:[^\s\)\]"'>]*)"#);
    let re      = Regex::new(&pattern).unwrap();
    let mut seen = HashSet::new();

    re.find_iter(text)
        .filter_map(|m| {
            let url = m.as_str().to_owned();
            if !seen.insert(url.clone()) { return None; }
            let name = url.split('/').last()
                .and_then(|s| s.split('?').next())
                .unwrap_or("unknown")
                .to_owned();
            Some(FileSource { name, url })
        })
        .collect()
}

// =============================================================================
// Downloader  (resume + retry + exponential backoff + delay)
// =============================================================================

async fn download_file(
    client:   &Client,
    src:      &FileSource,
    dir:      &Path,
    pb:       &ProgressBar,
    resume:   bool,
    delay_ms: Option<u64>,
    retries:  u32,
) -> Result<()> {
    let dest = dir.join(&src.name);

    let existing_size: u64 = if dest.exists() {
        let size = tokio::fs::metadata(&dest).await.map(|m| m.len()).unwrap_or(0);
        if resume && size > 0 {
            size
        } else if !resume {
            pb.set_message(format!("{} {}", "skip".dimmed(), src.name.dimmed()));
            pb.inc(1);
            return Ok(());
        } else {
            0
        }
    } else {
        0
    };

    if let Some(ms) = delay_ms {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    pb.set_message(format!("{} {}", "↓".cyan(), src.name));

    let mut last_err: Option<Box<dyn std::error::Error>> = None;

    for attempt in 0..=retries {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(500 * 2u64.pow(attempt - 1))).await;
        }

        let mut req = client.get(&src.url);
        if existing_size > 0 {
            req = req.header("Range", format!("bytes={existing_size}-"));
        }

        let resp = match req.send().await {
            Ok(r)  => r,
            Err(e) => { last_err = Some(e.into()); continue; }
        };

        if resp.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            pb.set_message(format!("{} {}", "complete".dimmed(), src.name.dimmed()));
            pb.inc(1);
            return Ok(());
        }

        let is_partial = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;

        let resp = match resp.error_for_status() {
            Ok(r)  => r,
            Err(e) => {
                // 4xx errors are not retryable
                if e.status().map(|s| s.is_client_error()).unwrap_or(false) {
                    return Err(e.into());
                }
                last_err = Some(e.into());
                continue;
            }
        };

        let bytes = match resp.bytes().await {
            Ok(b)  => b,
            Err(e) => { last_err = Some(e.into()); continue; }
        };

        if is_partial && existing_size > 0 {
            use tokio::io::AsyncWriteExt;
            let mut file = tokio::fs::OpenOptions::new().append(true).open(&dest).await?;
            file.write_all(&bytes).await?;
        } else {
            tokio::fs::write(&dest, &bytes).await?;
        }

        pb.inc(1);
        return Ok(());
    }

    Err(last_err.unwrap_or_else(|| "download failed after all retries".into()))
}

// =============================================================================
// Entry point
// =============================================================================

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let exts = all_extensions(&args.kinds);

    let kinds_label = args.kinds.iter()
        .map(|k| k.label().cyan().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    println!(
        "{} {}  [{}]",
        "🧭 marcopolo →".cyan().bold(),
        args.url.yellow().bold(),
        kinds_label,
    );

    // ── Build HTTP client ──────────────────────────────────────────────────────
    let mut default_headers = header::HeaderMap::new();
    default_headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("application/vnd.github+json"),
    );
    if let Some(ref token) = args.token {
        if let Ok(val) = header::HeaderValue::from_str(&format!("Bearer {token}")) {
            default_headers.insert(header::AUTHORIZATION, val);
            println!("{} using GitHub token", "→".dimmed());
        }
    }

    let client = Client::builder()
        .user_agent("marcopolo/0.3 (multi-type file downloader)")
        .default_headers(default_headers)
        .timeout(Duration::from_secs(30))
        .build()
        .expect("failed to build HTTP client");

    // ── Discover files ─────────────────────────────────────────────────────────
    let mut all: Vec<FileSource> = if args.url.contains("github.com") {

        let (owner, repo, subpath) = match parse_github_url(&args.url) {
            Ok(t)  => t,
            Err(e) => { eprintln!("{} {e}", "error:".red().bold()); std::process::exit(1); }
        };

        if let Some(ref sp) = subpath {
            println!("{} subdirectory scope: {}", "→".dimmed(), sp.cyan());
        }

        println!("{} scanning repo tree, README, and releases …", "→".dimmed());

        let branch = match default_branch(&client, &owner, &repo).await {
            Ok(b)  => b,
            Err(e) => { eprintln!("{} {e}", "error:".red().bold()); std::process::exit(1); }
        };

        let sp = subpath.as_deref();

        let (tree_r, readme_r, releases_r) = tokio::join!(
            repo_files(&client, &owner, &repo, &branch, &exts, sp),
            readme_files(&client, &owner, &repo, &exts, sp),
            release_files(&client, &owner, &repo, &exts),
        );

        let mut seen:  HashSet<String> = HashSet::new();
        let mut files: Vec<FileSource> = Vec::new();
        for f in tree_r.unwrap_or_default()
            .into_iter()
            .chain(readme_r.unwrap_or_default())
            .chain(releases_r.unwrap_or_default())
        {
            if seen.insert(f.url.clone()) { files.push(f); }
        }
        files

    } else {

        println!(
            "{} scanning web page (depth: {}) …",
            "→".dimmed(),
            args.depth.to_string().cyan(),
        );
        match web_scraper::scrape_files(&client, &args.url, args.depth, &exts).await {
            Ok(files) => files,
            Err(e)    => { eprintln!("{} {e}", "error:".red().bold()); std::process::exit(1); }
        }

    };

    // ── Apply --filter ─────────────────────────────────────────────────────────
    if let Some(ref kw) = args.filter {
        let kw_lower = kw.to_lowercase();
        all.retain(|f| f.name.to_lowercase().contains(&kw_lower));
        println!(
            "{} filter \"{}\" → {} match(es)",
            "→".dimmed(), kw.cyan(), all.len().to_string().green(),
        );
    }

    if all.is_empty() {
        println!("{}", "No files found.".yellow());
        return;
    }

    println!(
        "{} {} file(s) discovered",
        "✓".green().bold(),
        all.len().to_string().green().bold(),
    );

    // ── --list dry-run ─────────────────────────────────────────────────────────
    if args.list {
        println!("\n{}", "Discovered files:".cyan().bold());
        for (i, f) in all.iter().enumerate() {
            println!(
                "  {}  {}  {}",
                format!("[{}]", i + 1).dimmed(),
                f.name.green(),
                f.url.dimmed(),
            );
        }
        return;
    }

    // ── Download ───────────────────────────────────────────────────────────────
    tokio::fs::create_dir_all(&args.out).await.expect("cannot create output directory");

    let pb = ProgressBar::new(all.len() as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.cyan} [{bar:45.cyan/blue}] {pos}/{len}  {msg}",
        )
        .unwrap()
        .progress_chars("█▓░"),
    );

    let out   = args.out.clone();
    let res   = args.resume;
    let delay = args.delay;
    let retry = args.retries;

    stream::iter(all.iter())
        .map(|src| download_file(&client, src, &out, &pb, res, delay, retry))
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .for_each(|r| {
            if let Err(e) = r { eprintln!("{} {e}", "download error:".red()); }
        });

    pb.finish_with_message("done ✓");
    println!(
        "\n{} all files saved to {}",
        "✓".green().bold(),
        args.out.display().to_string().cyan().bold(),
    );
}
