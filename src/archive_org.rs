//! Internet Archive (archive.org) links → downloadable files.
//!
//! An archive.org page link is usually not a file: `/details/<id>` is the web
//! page of an *item* (one book, film, …) that holds several files, and
//! `/search?query=…` is a list of items. Reading lists such as
//! `github.com/valeman/Awesome_Math_Books` link those pages rather than the
//! PDFs inside them, so a plain "does the URL end in .pdf" check finds nothing.
//!
//! This module turns such links into real file URLs with two public JSON APIs
//! (no key needed):
//! - Metadata API — `https://archive.org/metadata/<id>` lists an item's files.
//! - Advanced Search — `https://archive.org/advancedsearch.php` turns a search
//!   query into item identifiers.
//!
//! Lend-only files (marked private, or DRM-encrypted copies) answer 403 to
//! anyone not borrowing the book, so they are skipped and reported.

use std::{collections::HashSet, time::Duration};

use colored::Colorize;
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Client;
use serde::Deserialize;
use url::Url;

use crate::retry::with_backoff;
use crate::utils::dedup::dedup_by_key;
use crate::utils::listing::short_list;
use crate::utils::parsing::encode_path;
use crate::{matches_ext, FileSource, Result};

const API_BASE: &str = "https://archive.org";

/// Lend-only collections: their files are private or DRM-encrypted.
pub const NOT_RESTRICTED: &str = "NOT collection:(inlibrary OR printdisabled)";

/// Items looked up at the same time — quick, yet polite to a free service.
const ITEM_FETCH_JOBS: usize = 8;
/// At most this many items are looked up per run, so a page linking thousands
/// of items can't fire off thousands of requests.
const MAX_ITEMS: usize = 500;
/// Items taken from each search link (Advanced Search `rows`).
const SEARCH_ROWS: &str = "50";
/// Tries per metadata request: archive.org answers the odd 429/503 under load.
const FETCH_ATTEMPTS: u32 = 3;
/// Item identifiers are at most this long.
const MAX_ID_LEN: usize = 100;
/// Item ids named in a note before it is cut short with "…".
const MAX_IDS_SHOWN: usize = 5;

// =============================================================================
// Link parsing (pure)
// =============================================================================

/// An archive.org page that stands for files rather than being one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ArchiveLink {
    /// One item: `/details/<id>`, `/stream/<id>`, `/embed/<id>` or a bare
    /// `/download/<id>` — viewer state such as `/page/n9/mode/2up` included.
    Item(String),
    /// A search page (`/search?query=…`); holds the query.
    Search(String),
}

/// `true` when `url` is archive.org itself — not `web.archive.org` (the
/// Wayback Machine, which stores copies of *other* sites) or a lookalike such
/// as `archive.org.evil.com`.
fn is_archive_host(url: &Url) -> bool {
    matches!(url.host_str(), Some("archive.org" | "www.archive.org"))
}

/// `true` for a valid item identifier: up to 100 letters, digits, `.`, `_`
/// or `-`, starting with a letter or digit. Rejects user pages (`@name`) and
/// anything that could change the path of the API URL it is put into.
fn is_item_id(s: &str) -> bool {
    s.len() <= MAX_ID_LEN
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Recognises an archive.org item or search page.
///
/// Returns `None` for any other URL, including a link to one file inside an
/// item (`/details/<id>/book.pdf`): that is a file already, see [`file_url`].
///
/// ```ignore
/// let u = Url::parse("https://archive.org/details/kolmogorov_202112/page/n9/mode/2up").unwrap();
/// assert_eq!(parse_link(&u), Some(ArchiveLink::Item("kolmogorov_202112".into())));
/// ```
pub fn parse_link(url: &Url) -> Option<ArchiveLink> {
    if !is_archive_host(url) { return None; }
    let segs: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    match segs.as_slice() {
        ["search" | "search.php"] => {
            let (_, query) = url.query_pairs().find(|(k, _)| k == "query")?;
            let query = query.trim();
            (!query.is_empty()).then(|| ArchiveLink::Search(query.to_owned()))
        }
        [kind, id, rest @ ..]
            if matches!(*kind, "details" | "stream" | "embed" | "download") && is_item_id(id) =>
        {
            // Viewer state (`/page/n9/mode/2up`) has no dots; a last segment
            // with one names a single file in the item.
            let names_a_file = rest.last().is_some_and(|s| s.contains('.'));
            (!names_a_file).then(|| ArchiveLink::Item((*id).to_owned()))
        }
        _ => None,
    }
}

/// Direct download URL for a link to one file inside an item.
///
/// `/details/<id>/<path>` and `/stream/<id>/<path>` are viewer pages around
/// the file; the file itself is served at `/download/<id>/<path>`. `None` for
/// any other URL.
pub fn file_url(url: &Url) -> Option<String> {
    if !is_archive_host(url) { return None; }
    let segs: Vec<&str> = url.path_segments()?.collect();
    match segs.as_slice() {
        [kind, id, rest @ ..] if matches!(*kind, "details" | "stream") && is_item_id(id) && !rest.is_empty() => {
            // Segments are still percent-encoded as in the link, so they can
            // be joined back into a URL as they are.
            Some(format!("{API_BASE}/download/{id}/{}", rest.join("/")))
        }
        _ => None,
    }
}

/// Download URL of file `name` (may contain `/` for sub-folders) in item `id`.
pub fn download_url(id: &str, name: &str) -> String {
    format!("{API_BASE}/download/{id}/{}", encode_path(name))
}

// =============================================================================
// Item metadata
// =============================================================================

/// The part of a Metadata API answer we use.
#[derive(Debug, Deserialize)]
pub struct Meta {
    /// Absent when the item does not exist (the API then answers `{}`).
    #[serde(default)]
    pub files: Vec<MetaFile>,
}

/// One file inside an item.
#[derive(Debug, Deserialize)]
pub struct MetaFile {
    /// Path inside the item, e.g. `book.pdf` or `scans/book.pdf`.
    pub name: String,
    /// archive.org's format label, e.g. `Text PDF` or `ACS Encrypted PDF`.
    #[serde(default)]
    pub format: String,
    /// `original` for uploaded files, `derivative` for copies archive.org
    /// made from them (OCR'd `_text.pdf`, EPUB, …).
    #[serde(default)]
    source: String,
    /// For a derivative: the file it was made from. Derivatives sharing it
    /// (`book.pdf` and grayscale `book_bw.pdf`) are versions of one scan.
    #[serde(default)]
    original: String,
    /// SHA-1 of the content; byte-identical copies share it.
    #[serde(default)]
    sha1: String,
    /// Present (as `"true"`) on lend-only files, which answer 403.
    #[serde(default)]
    private: Option<serde_json::Value>,
}

impl MetaFile {
    /// `false` for lend-only files: marked private, or DRM-encrypted copies
    /// (`ACS Encrypted PDF`, `LCP Encrypted EPUB`).
    pub fn is_public(&self) -> bool {
        self.private.is_none() && !self.format.contains("Encrypted")
    }

    fn is_original(&self) -> bool {
        self.source == "original"
    }
}

/// Fetches the file list of item `id` (which must pass [`is_item_id`] or come
/// from archive.org itself).
pub async fn fetch_meta(client: &Client, id: &str) -> Result<Meta> {
    Ok(client
        .get(format!("{API_BASE}/metadata/{id}"))
        .send().await?
        .error_for_status()?
        .json().await?)
}

/// A file picked from an item, ready to download.
#[derive(Debug)]
struct Picked {
    file: FileSource,
    /// SHA-1 archive.org lists for the content; empty when it lists none.
    sha1: String,
}

impl Picked {
    /// What makes two picks the same file: byte-identical copies share a
    /// SHA-1. Without one, only the same URL counts.
    fn identity(&self) -> String {
        if self.sha1.is_empty() { self.file.url.clone() } else { self.sha1.clone() }
    }
}

/// What one item holds of the wanted file types.
#[derive(Debug)]
enum ItemFiles {
    /// Public files, ready to download; `duplicates` older copies were left out.
    Found { files: Vec<Picked>, duplicates: usize },
    /// Wanted files exist, but only for borrowing on archive.org.
    LendOnly,
    /// No file of the wanted types (collections have none either).
    Nothing,
    /// The item does not exist (any more): a dead link, or a removed item.
    Missing,
}

/// Picks the files of item `id` worth downloading.
fn pick_files(id: &str, files: &[MetaFile], exts: &[&str]) -> ItemFiles {
    // A live item always lists at least its own `<id>_meta.xml`; no files at
    // all means the API answered `{}` for an unknown or removed item.
    if files.is_empty() { return ItemFiles::Missing; }

    // `__ia_thumb.jpg` and the like are archive.org's own thumbnails.
    let wanted: Vec<&MetaFile> = files
        .iter()
        .filter(|f| matches_ext(&f.name, exts) && !file_name(&f.name).starts_with("__ia_"))
        .collect();
    if wanted.is_empty() { return ItemFiles::Nothing; }

    let public: Vec<&MetaFile> = wanted.into_iter().filter(|f| f.is_public()).collect();
    if public.is_empty() { return ItemFiles::LendOnly; }

    // An item holds the upload plus copies archive.org derived from it (an
    // OCR'd `_text.pdf`, a grayscale `_bw.pdf`, …). So each book is listed
    // once, not two or three times: where an extension has an original, skip
    // its derivatives; otherwise keep the first derivative per source file
    // (files are listed by name, so `book.pdf` comes before `book_bw.pdf`).
    let original_exts: HashSet<String> = public
        .iter()
        .filter(|f| f.is_original())
        .map(|f| extension(&f.name))
        .collect();
    let mut sources_seen = HashSet::new();
    let mut versions: Vec<&MetaFile> = public
        .into_iter()
        .filter(|f| {
            let ext = extension(&f.name);
            f.is_original() || (!original_exts.contains(&ext) && sources_seen.insert((f.original.as_str(), ext)))
        })
        .collect();

    // Some uploaders leave an older copy in a sub-folder (`del/book.pdf` next
    // to `book.pdf`); the file at the item's root wins. Same-named files that
    // all sit in sub-folders (`vol1/book.pdf`, `vol2/book.pdf`) are different
    // files and are all kept.
    let root_names: HashSet<&str> = versions
        .iter()
        .copied()
        .map(|f| f.name.as_str())
        .filter(|name| !name.contains('/'))
        .collect();
    let before = versions.len();
    versions.retain(|f| !f.name.contains('/') || !root_names.contains(file_name(&f.name)));

    ItemFiles::Found {
        duplicates: before - versions.len(),
        files: versions
            .into_iter()
            .map(|f| Picked {
                file: FileSource { name: file_name(&f.name).to_owned(), url: download_url(id, &f.name) },
                sha1: f.sha1.clone(),
            })
            .collect(),
    }
}

/// Last component of a path inside an item (`scans/book.pdf` → `book.pdf`).
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Lower-case extension of `name`, empty when it has none.
fn extension(name: &str) -> String {
    name.rsplit_once('.').map(|(_, ext)| ext.to_lowercase()).unwrap_or_default()
}

// =============================================================================
// Search
// =============================================================================

#[derive(Debug, Deserialize)]
struct SearchResp {
    response: SearchInner,
}

#[derive(Debug, Deserialize)]
struct SearchInner {
    /// Total matches, of which at most `SEARCH_ROWS` are in `docs`.
    #[serde(rename = "numFound", default)]
    num_found: usize,
    docs: Vec<SearchDoc>,
}

#[derive(Debug, Deserialize)]
struct SearchDoc {
    identifier: String,
}

/// Items one search link turned up.
#[derive(Debug)]
struct SearchHits {
    ids:   Vec<String>,
    /// Matches in total; more than `ids.len()` when results were cut off.
    total: usize,
}

/// Runs `query` (Lucene syntax, as in the `query=` of an archive.org search
/// page) through Advanced Search, with lend-only collections left out.
async fn search_ids(client: &Client, query: &str) -> Result<SearchHits> {
    let q = format!("({query}) AND {NOT_RESTRICTED}");
    let resp: SearchResp = client
        .get(format!("{API_BASE}/advancedsearch.php"))
        .query(&[("q", q.as_str()), ("fl[]", "identifier"), ("rows", SEARCH_ROWS), ("output", "json")])
        .send().await?
        .error_for_status()?
        .json().await?;
    Ok(hits_from(resp))
}

/// Keeps only well-formed identifiers from a search answer.
fn hits_from(resp: SearchResp) -> SearchHits {
    SearchHits {
        total: resp.response.num_found,
        ids:   resp.response.docs.into_iter().map(|d| d.identifier).filter(|id| is_item_id(id)).collect(),
    }
}

// =============================================================================
// Resolving links into files
// =============================================================================

/// Every downloadable file behind `links`, in link order.
///
/// Search links are expanded into their items first, then each item's file
/// list is fetched (`ITEM_FETCH_JOBS` at a time). Lookup failures are
/// warnings, never errors, so one bad link can't stop the rest of a scrape.
pub async fn resolve(client: &Client, links: &[ArchiveLink], exts: &[&str], quiet: bool) -> Vec<FileSource> {
    let mut ids = item_ids(client, links).await;
    if ids.len() > MAX_ITEMS {
        eprintln!(
            "{} {} archive.org items linked; only the first {MAX_ITEMS} are looked up.",
            "warning:".yellow(), ids.len(),
        );
        ids.truncate(MAX_ITEMS);
    }
    if ids.is_empty() { return Vec::new(); }

    let pb = if quiet { ProgressBar::hidden() } else { ProgressBar::new(ids.len() as u64) };
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} archive.org items [{bar:30.cyan/blue}] {pos}/{len}")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("█▓░"),
    );
    pb.enable_steady_tick(Duration::from_millis(120));

    // `buffer_unordered`, not `buffered`: with `buffered` one slow item holds
    // back the answers behind it and no new request starts meanwhile (3x
    // slower on a 200-item list). The index restores link order afterwards.
    let mut outcomes: Vec<(usize, Result<ItemFiles>)> = stream::iter(ids.iter().enumerate())
        .map(|(i, id)| {
            let pb = &pb;
            async move {
                let r = with_backoff(FETCH_ATTEMPTS, || fetch_meta(client, id))
                    .await
                    .map(|m| pick_files(id, &m.files, exts));
                pb.inc(1);
                (i, r)
            }
        })
        .buffer_unordered(ITEM_FETCH_JOBS)
        .collect()
        .await;
    pb.finish_and_clear();
    outcomes.sort_unstable_by_key(|(i, _)| *i);

    let mut picked: Vec<Picked> = Vec::new();
    let (mut lend_only, mut missing): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
    let (mut empty, mut failed, mut duplicates) = (0usize, 0usize, 0usize);
    let mut first_error = None;
    for (i, outcome) in outcomes {
        let id = ids[i].as_str();
        match outcome {
            Ok(ItemFiles::Found { files, duplicates: copies }) => {
                picked.extend(files);
                duplicates += copies;
            }
            Ok(ItemFiles::LendOnly)     => lend_only.push(id),
            Ok(ItemFiles::Missing)      => missing.push(id),
            Ok(ItemFiles::Nothing)      => empty += 1,
            Err(e) => {
                failed += 1;
                first_error.get_or_insert(e);
            }
        }
    }

    // The same upload can sit in several items (mirrors, re-uploads);
    // byte-identical copies share a SHA-1, so only the first one is kept.
    let before = picked.len();
    let picked = dedup_by_key(picked, Picked::identity);
    duplicates += before - picked.len();

    if let Some(e) = first_error {
        eprintln!(
            "{} {failed} archive.org item(s) could not be looked up (first error: {e}).",
            "warning:".yellow(),
        );
    }
    if !quiet {
        note_ids("are lend-only (borrow them on archive.org)", &lend_only);
        note_ids("no longer exist (dead links)", &missing);
        if empty > 0 {
            println!("{} {empty} archive.org item(s) have no {} files.", "note:".cyan(), exts.join("/"));
        }
        if duplicates > 0 {
            println!(
                "{} {duplicates} duplicate file(s) skipped (an older copy in a sub-folder, or the same file in another item).",
                "note:".cyan(),
            );
        }
    }
    picked.into_iter().map(|p| p.file).collect()
}

/// Item ids behind `links`: item links as they are, search links expanded.
/// Duplicates are dropped, first occurrence wins.
async fn item_ids(client: &Client, links: &[ArchiveLink]) -> Vec<String> {
    let mut seen_links = HashSet::new();
    let unique: Vec<&ArchiveLink> = links.iter().filter(|l| seen_links.insert(*l)).collect();

    let per_link: Vec<Vec<String>> = stream::iter(unique)
        .map(|link| async move {
            match link {
                ArchiveLink::Item(id) => vec![id.clone()],
                ArchiveLink::Search(query) => match search_ids(client, query).await {
                    Ok(hits) => {
                        if hits.total > hits.ids.len() {
                            eprintln!(
                                "{} archive.org search \"{query}\" matched {} items; only the first {} are used.",
                                "warning:".yellow(), hits.total, hits.ids.len(),
                            );
                        }
                        hits.ids
                    }
                    Err(e) => {
                        eprintln!("{} archive.org search \"{query}\" failed: {e}", "warning:".yellow());
                        Vec::new()
                    }
                },
            }
        })
        .buffered(ITEM_FETCH_JOBS)
        .collect()
        .await;

    let mut seen_ids = HashSet::new();
    per_link.into_iter().flatten().filter(|id| seen_ids.insert(id.clone())).collect()
}

/// Says which items gave no files and why, so "233 links → 185 files" isn't
/// a mystery. Prints nothing when `ids` is empty.
fn note_ids(why: &str, ids: &[&str]) {
    if ids.is_empty() { return; }
    println!("{} {} archive.org item(s) {why}: {}", "note:".cyan(), ids.len(), short_list(ids, MAX_IDS_SHOWN));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(raw: &str) -> Option<ArchiveLink> {
        parse_link(&Url::parse(raw).unwrap())
    }

    fn item(id: &str) -> Option<ArchiveLink> {
        Some(ArchiveLink::Item(id.to_owned()))
    }

    fn meta_file(name: &str, source: &str, format: &str, private: bool) -> MetaFile {
        MetaFile {
            name:     name.to_owned(),
            format:   format.to_owned(),
            source:   source.to_owned(),
            original: String::new(),
            sha1:     String::new(),
            private:  private.then(|| serde_json::Value::String("true".to_owned())),
        }
    }

    fn derived_from(mut f: MetaFile, original: &str) -> MetaFile {
        f.original = original.to_owned();
        f
    }

    fn found_urls(r: ItemFiles) -> Vec<String> {
        match r {
            ItemFiles::Found { files, .. } => files.into_iter().map(|p| p.file.url).collect(),
            other => panic!("expected Found, got {other:?}"),
        }
    }

    fn pick(url: &str, sha1: &str) -> Picked {
        Picked {
            file: FileSource { name: "x.pdf".to_owned(), url: url.to_owned() },
            sha1: sha1.to_owned(),
        }
    }

    #[test]
    fn item_links_in_all_forms() {
        assert_eq!(link("https://archive.org/details/kolmogorov_202112"), item("kolmogorov_202112"));
        assert_eq!(link("https://archive.org/details/algebra-gelfand-1/"), item("algebra-gelfand-1"));
        assert_eq!(link("https://www.archive.org/stream/foo"), item("foo"));
        assert_eq!(link("https://archive.org/embed/foo"), item("foo"));
        assert_eq!(link("https://archive.org/download/foo"), item("foo"));
        // Plain-HTTP links are recognised; the API calls are made over HTTPS anyway.
        assert_eq!(link("http://archive.org/details/foo"), item("foo"));
    }

    #[test]
    fn viewer_state_and_query_still_mean_the_item() {
        assert_eq!(link("https://archive.org/details/ProbabilityTheoryfirstSteps/page/n9/mode/2up"), item("ProbabilityTheoryfirstSteps"));
        assert_eq!(link("https://archive.org/details/foo/mode/2up"), item("foo"));
        assert_eq!(link("https://archive.org/details/foo?utm_source=chatgpt.com"), item("foo"));
        assert_eq!(link("https://archive.org/details/foo#reviews"), item("foo"));
    }

    #[test]
    fn identifiers_may_contain_dots() {
        assert_eq!(link("https://archive.org/details/mathsuperieureshighermathsmirnovmir.7z"), item("mathsuperieureshighermathsmirnovmir.7z"));
    }

    #[test]
    fn file_links_are_not_items() {
        assert_eq!(link("https://archive.org/details/foo/book.pdf"), None);
        assert_eq!(link("https://archive.org/download/foo/book.pdf"), None);
    }

    #[test]
    fn search_links() {
        assert_eq!(
            link("https://archive.org/search?query=creator%3A%22V.+I.+Smirnov%22"),
            Some(ArchiveLink::Search("creator:\"V. I. Smirnov\"".to_owned())),
        );
        assert_eq!(link("https://archive.org/search.php?query=calculus"), Some(ArchiveLink::Search("calculus".to_owned())));
        assert_eq!(link("https://archive.org/search?query=+"), None);
        assert_eq!(link("https://archive.org/search"), None);
    }

    #[test]
    fn other_urls_are_ignored() {
        assert_eq!(link("https://web.archive.org/web/2020/https://example.com/a.pdf"), None);
        assert_eq!(link("https://archive.org.evil.com/details/foo"), None);
        assert_eq!(link("https://example.com/details/foo"), None);
        assert_eq!(link("https://archive.org/details/@someuser"), None);
        assert_eq!(link("https://archive.org/details/..%2Fadmin"), None);
        assert_eq!(link("https://archive.org/"), None);
        assert_eq!(link("https://archive.org/about"), None);
        assert_eq!(link(&format!("https://archive.org/details/{}", "a".repeat(101))), None);
    }

    #[test]
    fn viewer_file_link_becomes_download_link() {
        let u = Url::parse("https://archive.org/details/foo/My%20Book.pdf?x=1").unwrap();
        assert_eq!(file_url(&u).as_deref(), Some("https://archive.org/download/foo/My%20Book.pdf"));
        let u = Url::parse("https://archive.org/stream/foo/sub/b.pdf").unwrap();
        assert_eq!(file_url(&u).as_deref(), Some("https://archive.org/download/foo/sub/b.pdf"));
        let u = Url::parse("https://archive.org/details/foo").unwrap();
        assert_eq!(file_url(&u), None);
        let u = Url::parse("https://example.com/details/foo/b.pdf").unwrap();
        assert_eq!(file_url(&u), None);
    }

    #[test]
    fn download_url_encodes_names() {
        assert_eq!(
            download_url("algebra-gelfand-1", "algebra_gelfand (1).pdf"),
            "https://archive.org/download/algebra-gelfand-1/algebra_gelfand%20(1).pdf",
        );
        assert_eq!(download_url("x", "scans/C# 1.pdf"), "https://archive.org/download/x/scans/C%23%201.pdf");
    }

    #[test]
    fn originals_win_over_their_derivatives() {
        let files = [
            meta_file("algebra_gelfand (1).epub", "derivative", "EPUB", false),
            meta_file("algebra_gelfand (1).pdf", "original", "Image Container PDF", false),
            meta_file("algebra_gelfand (1)_text.pdf", "derivative", "Additional Text PDF", false),
        ];
        assert_eq!(
            found_urls(pick_files("g", &files, &["pdf"])),
            ["https://archive.org/download/g/algebra_gelfand%20(1).pdf"],
        );
        // The EPUB has no original of its own, so asking for it still gets it.
        assert_eq!(found_urls(pick_files("g", &files, &["pdf", "epub"])).len(), 2);
    }

    #[test]
    fn derivatives_used_when_no_original_of_that_type() {
        let files = [
            meta_file("book.djvu", "original", "DjVu", false),
            meta_file("book.pdf", "derivative", "Text PDF", false),
        ];
        assert_eq!(found_urls(pick_files("b", &files, &["pdf"])), ["https://archive.org/download/b/book.pdf"]);
    }

    #[test]
    fn one_derivative_per_source_file() {
        // Shape of courseofpuremath00hardrich: two PDFs made from one scan.
        let files = [
            derived_from(meta_file("hardy.pdf", "derivative", "Text PDF", false), "hardy_jp2.zip"),
            derived_from(meta_file("hardy_bw.pdf", "derivative", "Grayscale PDF", false), "hardy_jp2.zip"),
            derived_from(meta_file("other.pdf", "derivative", "Text PDF", false), "other_jp2.zip"),
        ];
        assert_eq!(
            found_urls(pick_files("h", &files, &["pdf"])),
            ["https://archive.org/download/h/hardy.pdf", "https://archive.org/download/h/other.pdf"],
        );
    }

    #[test]
    fn every_original_is_kept() {
        // Multi-volume items and several uploaded resolutions are all real files.
        let files = [
            meta_file("Vol 1.pdf", "original", "Image Container PDF", false),
            meta_file("Vol 1_text.pdf", "derivative", "Additional Text PDF", false),
            meta_file("Vol 2.pdf", "original", "Image Container PDF", false),
        ];
        assert_eq!(found_urls(pick_files("m", &files, &["pdf"])).len(), 2);
    }

    #[test]
    fn lend_only_items_are_recognised() {
        let files = [
            meta_file("rudi.pdf", "derivative", "Text PDF", true),
            meta_file("rudi_encrypted.pdf", "derivative", "ACS Encrypted PDF", false),
        ];
        assert!(matches!(pick_files("r", &files, &["pdf"]), ItemFiles::LendOnly));
    }

    #[test]
    fn items_without_wanted_files() {
        let files = [meta_file("c_meta.xml", "original", "Metadata", false)];
        assert!(matches!(pick_files("c", &files, &["pdf"]), ItemFiles::Nothing));
        let files = [
            meta_file("__ia_thumb.jpg", "original", "Item Tile", false),
            meta_file("book.pdf", "original", "Text PDF", false),
        ];
        assert!(matches!(pick_files("t", &files, &["jpg"]), ItemFiles::Nothing));
    }

    #[test]
    fn unknown_items_are_missing() {
        // The API answers `{}` for them: no file list at all.
        assert!(matches!(pick_files("gone", &[], &["pdf"]), ItemFiles::Missing));
    }

    #[test]
    fn older_copy_in_sub_folder_is_a_duplicate() {
        // Shape of the smirnov-a-course-of-higher-mathematics-vol-* items.
        let files = [
            meta_file("Smirnov - Vol 2.pdf", "original", "Text PDF", false),
            meta_file("del/Smirnov - Vol 2.pdf", "original", "Text PDF", false),
        ];
        let r = pick_files("s", &files, &["pdf"]);
        assert!(matches!(r, ItemFiles::Found { duplicates: 1, .. }));
        assert_eq!(found_urls(r), ["https://archive.org/download/s/Smirnov%20-%20Vol%202.pdf"]);
    }

    #[test]
    fn same_names_only_in_sub_folders_are_kept() {
        let files = [
            meta_file("vol1/book.pdf", "original", "Text PDF", false),
            meta_file("vol2/book.pdf", "original", "Text PDF", false),
        ];
        let r = pick_files("v", &files, &["pdf"]);
        assert!(matches!(r, ItemFiles::Found { duplicates: 0, .. }));
        assert_eq!(found_urls(r).len(), 2);
    }

    #[test]
    fn folder_copy_kept_when_root_copy_is_lend_only() {
        // Never drop the only copy that can actually be downloaded.
        let files = [
            meta_file("book.pdf", "original", "Text PDF", true),
            meta_file("old/book.pdf", "original", "Text PDF", false),
        ];
        assert_eq!(found_urls(pick_files("l", &files, &["pdf"])), ["https://archive.org/download/l/old/book.pdf"]);
    }

    #[test]
    fn identical_copies_share_an_identity() {
        let picks = vec![
            pick("https://archive.org/download/a/book.pdf", "abc"),
            pick("https://archive.org/download/b/mirror.pdf", "abc"), // same bytes, other item
            pick("https://archive.org/download/c/one.pdf", ""),       // no SHA-1 listed
            pick("https://archive.org/download/d/two.pdf", ""),       // no SHA-1 listed
        ];
        let kept: Vec<String> = dedup_by_key(picks, Picked::identity).into_iter().map(|p| p.file.url).collect();
        assert_eq!(
            kept,
            [
                "https://archive.org/download/a/book.pdf",
                "https://archive.org/download/c/one.pdf",
                "https://archive.org/download/d/two.pdf",
            ],
        );
    }

    #[test]
    fn nested_files_get_their_own_name() {
        let files = [meta_file("vol 1/book.pdf", "original", "Text PDF", false)];
        match pick_files("n", &files, &["pdf"]) {
            ItemFiles::Found { files, .. } => {
                assert_eq!(files[0].file.name, "book.pdf");
                assert_eq!(files[0].file.url, "https://archive.org/download/n/vol%201/book.pdf");
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn metadata_json_shapes() {
        // Unknown items answer `{}`.
        let meta: Meta = serde_json::from_str("{}").unwrap();
        assert!(meta.files.is_empty());

        let meta: Meta = serde_json::from_str(
            r#"{"files":[{"name":"a.pdf","source":"original","format":"Text PDF","size":"12","sha1":"ab12"},
                        {"name":"b.pdf","private":"true"}]}"#,
        ).unwrap();
        assert!(meta.files[0].is_public() && meta.files[0].is_original());
        assert_eq!(meta.files[0].sha1, "ab12");
        assert!(!meta.files[1].is_public());
    }

    #[test]
    fn search_answer_keeps_valid_ids() {
        let resp: SearchResp = serde_json::from_str(
            r#"{"response":{"numFound":120,"start":0,"docs":[{"identifier":"good-id"},{"identifier":"@bad"}]}}"#,
        ).unwrap();
        let hits = hits_from(resp);
        assert_eq!(hits.ids, ["good-id"]);
        assert_eq!(hits.total, 120);
    }
}
