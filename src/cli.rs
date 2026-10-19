//! CLI definition — Clap subcommands for marcopolo.
//!
//! Usage:
//! ```text
//! marcopolo scrape <URL> [OPTIONS]
//! marcopolo find <QUERY> [OPTIONS]
//! ```
//!
//! # GitHub Authentication
//! To raise the API rate limit from 60 to 5,000 requests/hr, set one of:
//!   export GITHUB_TOKEN=ghp_...
//!   export MARCOPOLO_GITHUB_TOKEN=ghp_...
//!
//! Do NOT pass tokens via --token flags — they would be visible to all users
//! via `ps aux` on shared systems.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// 🧭 Hunt and download files from GitHub repos, websites, or free book sources.
#[derive(Parser, Debug)]
#[command(name = "marcopolo", version, about = "Multi-source file hunter & downloader")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Scrape a GitHub repo, an archive.org item/search page, a Google Drive file, or any website for files to download.
    ///
    /// archive.org item/search pages and Google Drive files linked from the
    /// repo or website are looked up too, and the public files behind them are
    /// offered for download.
    Scrape {
        /// GitHub repo URL, archive.org item/search URL, Google Drive file URL, or any website URL to crawl.
        url: String,

        /// File types to hunt (repeatable: --type pdf --type img)
        #[arg(
            long = "type",
            short = 't',
            value_enum,
            default_values_t = vec![crate::FileKind::Pdf],
            num_args = 1..
        )]
        kinds: Vec<crate::FileKind>,

        /// Output directory for downloaded files.
        #[arg(long, short = 'o', default_value = "downloads")]
        out: PathBuf,

        /// Link-depth to crawl (web mode only; 0 = landing page only).
        #[arg(long, default_value_t = 1)]
        depth: usize,

        /// Minimum milliseconds between download requests (across all parallel jobs).
        #[arg(long)]
        delay: Option<u64>,

        /// Also skip any same-named file in the output folder and resume any .part file, even ones from before
        /// marcopolo kept progress. (Progress it recorded is always continued: just run the same command again.)
        #[arg(long = "continue", default_value_t = false)]
        resume: bool,

        /// Retry attempts per file before giving up.
        #[arg(long, default_value_t = 3)]
        retries: u32,

        /// Number of files to download in parallel.
        #[arg(long, short = 'j', default_value_t = 4, value_parser = clap::value_parser!(u16).range(1..=64))]
        jobs: u16,

        /// List files without downloading (dry run).
        #[arg(long, default_value_t = false)]
        list: bool,

        /// Only include files whose name contains this string (case-insensitive).
        #[arg(long)]
        filter: Option<String>,

        /// Suppress all output except errors (useful for scripting).
        #[arg(long, default_value_t = false)]
        quiet: bool,

        /// Skip the ClamAV malware scan (files are saved without being isolated and checked).
        #[arg(long = "no-scan", default_value_t = false)]
        no_scan: bool,
    },

    /// Search free book sources and optionally download results.
    ///
    /// Sources queried in parallel: Archive.org, Open Library, Gutendex, Anna's Archive, GitHub, Google Scholar, DuckDuckGo.
    ///
    /// Examples:
    ///   marcopolo find "Clean Code"
    ///   marcopolo find "The Pragmatic Programmer" --list
    ///   marcopolo find "CLRS" --source archive
    ///   marcopolo find "Design Patterns" --get --out ~/books
    Find {
        /// Book or document title to search for.
        query: String,

        /// Preview results without downloading.
        #[arg(long, default_value_t = false)]
        list: bool,

        /// Download the top result from each source.
        #[arg(long, default_value_t = false)]
        get: bool,

        /// Restrict search to a single source (archive | openlibrary | gutenberg | annas | github | googlescholar | duckduckgo).
        #[arg(long, value_enum, value_name = "SOURCE")]
        source: Option<crate::utils::validation::Source>,

        /// Only keep results in this format.
        #[arg(long, value_enum, value_name = "FORMAT")]
        format: Option<crate::utils::validation::SupportedExtension>,

        /// Output directory for downloaded files.
        #[arg(long, short = 'o', default_value = "downloads")]
        out: PathBuf,

        /// Number of files to download in parallel.
        #[arg(long, short = 'j', default_value_t = 4, value_parser = clap::value_parser!(u16).range(1..=64))]
        jobs: u16,

        /// Suppress all output except errors (useful for scripting).
        #[arg(long, default_value_t = false)]
        quiet: bool,

        /// Skip the ClamAV malware scan (files are saved without being isolated and checked).
        #[arg(long = "no-scan", default_value_t = false)]
        no_scan: bool,
    },
}
