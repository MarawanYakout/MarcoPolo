//! CLI definition — Clap subcommands for marcopolo.
//!
//! Usage:
//! ```text
//! marcopolo scrape <URL> [OPTIONS]
//! marcopolo find <QUERY> [OPTIONS]
//! ```

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// 🧭 Hunt and download files from GitHub repos, websites, or free book sources.
#[derive(Parser, Debug)]
#[command(name = "marcopolo", version = "0.4.0", about = "Multi-source file hunter & downloader")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Scrape a GitHub repo or any website for files to download.
    Scrape {
        /// GitHub repo URL or any website URL to crawl.
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

        /// Milliseconds of delay between each download request.
        #[arg(long)]
        delay: Option<u64>,

        /// Append to partially downloaded files instead of re-downloading.
        #[arg(long = "continue", default_value_t = false)]
        resume: bool,

        /// Retry attempts per file before giving up.
        #[arg(long, default_value_t = 3)]
        retries: u32,

        /// List files without downloading (dry run).
        #[arg(long, default_value_t = false)]
        list: bool,

        /// Only include files whose name contains this string (case-insensitive).
        #[arg(long)]
        filter: Option<String>,

        /// GitHub personal access token (raises rate limit 60 → 5 000 req/hr).
        #[arg(long)]
        token: Option<String>,
    },

    /// Search free book sources and optionally download results.
    ///
    /// Sources queried in parallel: Archive.org, Open Library, Gutendex, Anna's Archive.
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

        /// Restrict search to a single source (archive | openlibrary | gutenberg | annas).
        #[arg(long, value_name = "SOURCE")]
        source: Option<String>,

        /// Output directory for downloaded files.
        #[arg(long, short = 'o', default_value = "downloads")]
        out: PathBuf,

        /// GitHub personal access token (not normally needed for find).
        #[arg(long)]
        token: Option<String>,
    },
}
