//! marcopolo v0.4 — multi-type file hunter & downloader + free-book search engine
//!
//! # Subcommands
//! ```text
//! marcopolo scrape <URL> [OPTIONS]   — crawl a GitHub repo or any website
//! marcopolo find   <QUERY> [OPTIONS] — search Archive.org, Open Library,
//!                                      Gutenberg, and Anna's Archive
//! ```

mod cli;
mod find;
mod utils;
mod web_scraper;

// ── Standard library ──────────────────────────────────────────────────────────
use std::{
    collections::HashSet,
    fmt::Write as FmtWrite,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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
    let clean = lower.split('?').next().unwrap_or(&lower);
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
// Log entry
// =============================================================================

/// A single record written to marcopolo.log after each download attempt.
#[derive(Debug)]
struct LogEntry {
    timestamp: String,
    filename:  String,
    url:       String,
    status:    String, // "ok" | "error: <msg>"
}

impl LogEntry {
    fn format(&self) -> String {
        format!(
            "[{}] {}  {}  {}\n",
            self.timestamp, self.status, self.filename, self.url
        )
    }
}

/// Returns the current UTC time formatted as `YYYY-MM-DD HH:MM:SS`.
fn utc_now() -> String {
    // std::time gives us seconds since epoch; format manually to avoid
    // pulling in the `chrono` crate.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (s, m, h) = (secs % 60, (secs / 60) % 60, (secs / 3600) % 24);
    let days      = secs / 86400; // days since 1970-01-01
    // Quick Gregorian approximation (good enough for log timestamps)
    let (mut y, mut rem) = (1970u32, days);
    loop {
        let dy = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
        if rem < dy { break; }
        rem -= dy;
        y   += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days_in = [31u32,if leap{29}else{28},31,30,31,30,31,31,30,31,30,31];
    let (mut mo, mut d) = (1u32, rem + 1);
    for dim in &days_in {
        if d <= *dim { break; }
        d  -= dim;
        mo += 1;
    }
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}")
}

// =============================================================================
// URL parsing
// =============================================================================

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
    let repo  = segs[1].to_owned();

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

async fn default_branch(client: &Client, owner: &str, repo: &str) -> Result<String> {
    let url  = format!("https://api.github.com/repos/{owner}/{repo}");
    let info: RepoInfo = client
        .get(&url)
        .send().await?
        .error_for_status()?
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

        let readme: ReadmeResponse = resp.error_for_status()?.json().await?;
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

pub fn extract_file_links(text: &str, exts: &[&str]) -> Vec<FileSource> {
    let ext_alt = exts.join("|");
    let pattern = format!(r#"https?://[^\s\)\]"'>]+\.(?i:{ext_alt})(?:[^\s\)\]"'>]*)"#);
    let re      = Regex::new(&pattern).unwrap();
    let mut seen = HashSet::new();

    re.find_iter(text)
        .filter_map(|m| {
            let url = m.as_str().to_owned();
            if !seen.insert(url.clone()) { return None; }
            let name = url.split('/')
                .last()
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
    client: &Client,
    url:    &str,
    dest:   &PathBuf,
) -> Result<()> {
    let resp  = client.get(url).send().await?.error_for_status()?;
    let bytes = resp.bytes().await?;
    tokio::fs::write(dest, &bytes).await?;
    Ok(())
}

async fn download_file_full(
    client:   &Client,
    src:      &FileSource,
    dir:      &Path,
    pb:       &ProgressBar,
    resume:   bool,
    delay_ms: Option<u64>,
    retries:  u32,
    quiet:    bool,
    log:      &Arc<Mutex<Vec<LogEntry>>>,
) -> Result<()> {
    let dest = dir.join(&src.name);

    // resume=true  → send Range header to continue a partial file
    // resume=false → always re-download from scratch (overwrite)
    let existing_size: u64 = if dest.exists() && resume {
        tokio::fs::metadata(&dest).await.map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    if let Some(ms) = delay_ms {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    if !quiet {
        pb.set_message(format!("{} {}", "↓".cyan(), src.name));
    }

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
            if !quiet {
                pb.set_message(format!("{} {}", "complete".dimmed(), src.name.dimmed()));
            }
            pb.inc(1);
            log.lock().unwrap().push(LogEntry {
                timestamp: utc_now(),
                filename:  src.name.clone(),
                url:       src.url.clone(),
                status:    "ok (already complete)".into(),
            });
            return Ok(());
        }

        let is_partial = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;

        let resp = match resp.error_for_status() {
            Ok(r)  => r,
            Err(e) => {
                if e.status().map(|s| s.is_client_error()).unwrap_or(false) {
                    let msg = format!("{e}");
                    log.lock().unwrap().push(LogEntry {
                        timestamp: utc_now(),
                        filename:  src.name.clone(),
                        url:       src.url.clone(),
                        status:    format!("error: {msg}"),
                    });
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
        log.lock().unwrap().push(LogEntry {
            timestamp: utc_now(),
            filename:  src.name.clone(),
            url:       src.url.clone(),
            status:    "ok".into(),
        });
        return Ok(());
    }

    let err_msg = last_err
        .as_ref()
        .map(|e| e.to_string())
        .unwrap_or_else(|| "download failed after all retries".into());

    log.lock().unwrap().push(LogEntry {
        timestamp: utc_now(),
        filename:  src.name.clone(),
        url:       src.url.clone(),
        status:    format!("error: {err_msg}"),
    });

    Err(last_err.unwrap_or_else(|| "download failed after all retries".into()))
}

/// Flush all log entries to `<out>/marcopolo.log`, appending to any existing file.
async fn write_log(out: &Path, entries: &[LogEntry]) -> Result<()> {
    if entries.is_empty() { return Ok(()); }
    let path = out.join("marcopolo.log");
    let mut content = String::new();
    for e in entries {
        let _ = FmtWrite::write_str(&mut content, &e.format());
    }
    // Append so multiple runs accumulate history
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;
    file.write_all(content.as_bytes()).await?;
    Ok(())
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

    let build_client = |token: Option<&str>| -> Result<Client> {
        let mut default_headers = header::HeaderMap::new();
        default_headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/vnd.github+json"),
        );
        if let Some(t) = token {
            if let Ok(val) = header::HeaderValue::from_str(&format!("Bearer {t}")) {
                default_headers.insert(header::AUTHORIZATION, val);
            }
        }
        Ok(Client::builder()
            .user_agent("marcopolo/0.4 (multi-type file downloader + book finder)")
            .default_headers(default_headers)
            .timeout(Duration::from_secs(30))
            .build()?)
    };

    match cli.command {
        // ── scrape subcommand ──────────────────────────────────────────────────
        Command::Scrape { url, kinds, out, depth, delay, resume, retries, list, filter, token, quiet } => {
            let client = build_client(token.as_deref())?;
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

            let mut all: Vec<FileSource> = if url.contains("github.com") {
                let (owner, repo, subpath, branch_from_url) = parse_github_url(&url)?;

                if !quiet {
                    if let Some(ref sp) = subpath {
                        println!("{} subdirectory scope: {}", "→".dimmed(), sp.cyan());
                    }
                    println!("{} scanning repo tree, README, and releases …", "→".dimmed());
                }

                let branch = match branch_from_url {
                    Some(b) => b,
                    None    => default_branch(&client, &owner, &repo).await?,
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

            tokio::fs::create_dir_all(&out).await?;

            // ── Progress bar: bytes/sec + ETA + file count ────────────────────
            // indicatif tokens used:
            //   {bytes_per_sec} — rolling download throughput
            //   {eta}           — estimated time to completion
            //   {pos}/{len}     — files done / total
            //   {msg}           — current filename
            let pb = if quiet {
                ProgressBar::hidden()
            } else {
                let b = ProgressBar::new(all.len() as u64);
                b.set_style(
                    ProgressStyle::with_template(
                        "{spinner:.cyan} [{bar:40.cyan/blue}] {pos}/{len}  {bytes_per_sec}  ETA {eta}  {msg}",
                    )
                    .unwrap()
                    .progress_chars("█▓░"),
                );
                b
            };

            let log: Arc<Mutex<Vec<LogEntry>>> = Arc::new(Mutex::new(Vec::new()));

            stream::iter(all.iter())
                .map(|src| download_file_full(
                    &client, src, &out, &pb, resume, delay, retries, quiet, &log,
                ))
                .buffer_unordered(4)
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .for_each(|r| {
                    if let Err(e) = r { eprintln!("{} {e}", "download error:".red()); }
                });

            if !quiet { pb.finish_with_message("done ✓"); }

            // ── Write download log ────────────────────────────────────────────
            let entries = log.lock().unwrap();
            if let Err(e) = write_log(&out, &entries).await {
                eprintln!("{} could not write log: {e}", "warning:".yellow());
            } else if !quiet && !entries.is_empty() {
                println!(
                    "{} log written → {}",
                    "✓".green(),
                    out.join("marcopolo.log").display().to_string().cyan(),
                );
            }
            drop(entries);

            if !quiet {
                println!(
                    "{} all files saved to {}",
                    "✓".green().bold(),
                    out.display().to_string().cyan().bold(),
                );
            }
        }

        // ── find subcommand ────────────────────────────────────────────────────
        Command::Find { query, list, get, source, out, token, quiet } => {
            let client = build_client(token.as_deref())?;

            if !utils::validation::is_valid_query(&query) {
                eprintln!("{} query must not be empty.", "error:".red().bold());
                std::process::exit(1);
            }

            if !quiet {
                let sources_label = if let Some(s) = &source {
                    format!("[{}]", s)
                } else {
                    "[archive.org · openlibrary · gutenberg · anna's archive · github · googlescholar · duckduckgo]".to_string()
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

            let results = find::search_all(&client, &query, source).await;
            pb_spin.finish_and_clear();

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
                (0..results.len()).collect()
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

                let mut selection = Vec::new();
                let mut attempts  = 0usize;
                loop {
                    print!("{} ", "→".cyan().bold());
                    io::stdout().flush().ok();
                    let raw     = read_line_stdin();
                    let trimmed = raw.trim().to_lowercase();
                    if trimmed == "q" || trimmed == "quit" || trimmed.is_empty() {
                        println!("{} nothing downloaded.", "✗".dimmed());
                        return Ok(());
                    }
                    selection = parse_selection(&trimmed, results.len());
                    if !selection.is_empty() { break; }
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
                selection
            };

            if chosen.is_empty() { return Ok(()); }

            tokio::fs::create_dir_all(&out).await?;

            if !quiet {
                println!(
                    "\n{} downloading {} file(s) → {}",
                    "↓".cyan().bold(),
                    chosen.len(),
                    out.display().to_string().cyan(),
                );
            }

            // ── Progress bar with throughput + ETA ────────────────────────────
            let pb_dl = if quiet {
                ProgressBar::hidden()
            } else {
                let b = ProgressBar::new(chosen.len() as u64);
                b.set_style(
                    ProgressStyle::with_template(
                        "  {spinner:.cyan} [{bar:40.cyan/blue}] {pos}/{len}  {bytes_per_sec}  ETA {eta}  {msg}"
                    )
                    .unwrap()
                    .progress_chars("█▓░"),
                );
                b
            };

            let log: Arc<Mutex<Vec<LogEntry>>> = Arc::new(Mutex::new(Vec::new()));
            let mut errors: Vec<String> = Vec::new();

            for idx in &chosen {
                let r    = &results[*idx];
                let dest = out.join(&r.filename);
                if !quiet { pb_dl.set_message(format!("{}", r.filename.dimmed())); }

                match download_file(&client, &r.url, &dest).await {
                    Ok(()) => {
                        if !quiet {
                            pb_dl.println(format!(
                                "  {} ({})  {}",
                                "✓".green().bold(), r.source.yellow(), r.filename,
                            ));
                        }
                        log.lock().unwrap().push(LogEntry {
                            timestamp: utc_now(),
                            filename:  r.filename.clone(),
                            url:       r.url.clone(),
                            status:    "ok".into(),
                        });
                    }
                    Err(e) => {
                        let msg = format!("  {} ({})  {}:  {e}",
                            "✗".red().bold(), r.source.yellow(), r.filename);
                        if !quiet { pb_dl.println(msg.clone()); }
                        log.lock().unwrap().push(LogEntry {
                            timestamp: utc_now(),
                            filename:  r.filename.clone(),
                            url:       r.url.clone(),
                            status:    format!("error: {e}"),
                        });
                        errors.push(msg);
                    }
                }
                pb_dl.inc(1);
            }

            pb_dl.finish_and_clear();

            // ── Write log ─────────────────────────────────────────────────────
            let entries = log.lock().unwrap();
            if let Err(e) = write_log(&out, &entries).await {
                eprintln!("{} could not write log: {e}", "warning:".yellow());
            } else if !quiet && !entries.is_empty() {
                println!(
                    "{} log written → {}",
                    "✓".green(),
                    out.join("marcopolo.log").display().to_string().cyan(),
                );
            }
            drop(entries);

            let ok_count = chosen.len() - errors.len();
            if !quiet {
                if ok_count > 0 {
                    println!(
                        "\n{} {} file(s) saved to {}",
                        "✓".green().bold(),
                        ok_count.to_string().green().bold(),
                        out.display().to_string().cyan().bold(),
                    );
                }
                if !errors.is_empty() {
                    println!(
                        "{} {} download(s) failed.",
                        "✗".red().bold(),
                        errors.len().to_string().red(),
                    );
                }
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
    use super::parse_selection;

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
}
