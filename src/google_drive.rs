//! Google Drive file and folder links → downloadable files.
//!
//! A Drive share link (`drive.google.com/file/d/<id>/view`) opens a web
//! viewer, not the file, and says nothing about the file's name or type. This
//! module recognises such links and asks Drive's download endpoint
//! (`drive.usercontent.google.com/download`, no API key needed) for the first
//! byte of each file. The answer names the file (`Content-Disposition`) and
//! proves it can be downloaded, so private or deleted files are reported up
//! front instead of failing later — and nothing big is fetched before the
//! user has picked what they want.
//!
//! Folder links (`drive.google.com/drive/folders/<id>`) are listed through
//! Drive's embeddable folder view (`drive.google.com/embeddedfolderview`, also
//! keyless), sub-folders included. The listing names every entry, so files
//! inside folders need no lookup of their own: wanted ones go straight to
//! the download list, others are skipped.
//!
//! `confirm=t` skips the "can't scan this file for viruses" page Drive shows
//! instead of files over ~100 MB; marcopolo scans downloads with ClamAV itself.
//!
//! Google Docs/Sheets/Slides are not files and are left alone.

use std::{collections::HashSet, time::Duration};

use colored::Colorize;
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use percent_encoding::percent_decode_str;
use reqwest::{header, Client, StatusCode};
use scraper::{Html, Selector};
use url::Url;

use crate::retry::with_backoff;
use crate::utils::listing::short_list;
use crate::{matches_ext, FileSource, Result};

const DOWNLOAD_ENDPOINT: &str = "https://drive.usercontent.google.com/download";
const FOLDER_ENDPOINT: &str = "https://drive.google.com/embeddedfolderview";

/// Requests to Drive at the same time — few, so Google doesn't start
/// answering 429 Too Many Requests.
const LOOKUP_JOBS: usize = 4;
/// At most this many files are looked up per run.
const MAX_FILES: usize = 500;
/// At most this many folders (sub-folders included) are listed per run.
const MAX_FOLDERS: usize = 100;
/// Sub-folders are followed this many levels down.
const MAX_FOLDER_DEPTH: usize = 10;
/// Tries per request when Google answers 429/5xx or the network fails.
const ATTEMPTS: u32 = 3;
/// Drive ids are 25–44 characters today; leave room either way.
const ID_LEN: std::ops::RangeInclusive<usize> = 10..=100;
/// Names shown in a note before it is cut short with "…".
const MAX_NAMES_SHOWN: usize = 5;

// =============================================================================
// Link parsing (pure)
// =============================================================================

/// A Drive file or folder id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DriveRef {
    pub id: String,
    /// `resourcekey` that some older share links need for access; passed on
    /// as is.
    pub resource_key: Option<String>,
}

/// What a Drive link points at.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DriveLink {
    File(DriveRef),
    Folder(DriveRef),
}

/// `true` for Drive ids and resource keys: letters, digits, `-` and `_` only,
/// so nothing in them can change the URLs they are put into.
fn is_token(s: &str) -> bool {
    ID_LEN.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Recognises a link to a Drive file or folder, in any of the forms Drive
/// hands out:
///
/// - files: `drive.google.com/file/d/<id>/view` (also `/edit`, `/preview`,
///   `/u/0/d/…`), `open?id=<id>`, `uc?id=<id>` (also on `docs.google.com`),
///   `drive.usercontent.google.com/download?id=<id>`
/// - folders: `drive.google.com/drive/folders/<id>` (also `/drive/u/0/…`,
///   `/drive/mobile/…`), `folderview?id=<id>`, `embeddedfolderview?id=<id>`
///
/// Returns `None` for any other URL, including Google Docs.
pub fn parse_link(url: &Url) -> Option<DriveLink> {
    let host = url.host_str()?;
    if !matches!(host, "drive.google.com" | "docs.google.com" | "drive.usercontent.google.com") {
        return None;
    }
    let query = |key: &str| url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned());

    let segs: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let (id, is_folder) = match segs.as_slice() {
        ["file", "d", id, ..] | ["file", "u", _, "d", id, ..] => ((*id).to_owned(), false),
        ["open" | "uc" | "download"] => (query("id")?, false),
        ["drive", "folders", id, ..]
        | ["drive", "u", _, "folders", id, ..]
        | ["drive", "mobile", "folders", id, ..] => ((*id).to_owned(), true),
        ["folderview" | "embeddedfolderview"] => (query("id")?, true),
        _ => return None,
    };
    if !is_token(&id) { return None; }

    let drive_ref = DriveRef { id, resource_key: query("resourcekey").filter(|k| is_token(k)) };
    Some(if is_folder { DriveLink::Folder(drive_ref) } else { DriveLink::File(drive_ref) })
}

/// `base?id=<id>` plus the resource key, if any. Id and key passed
/// [`is_token`], so they need no escaping.
fn with_id(base: &str, item: &DriveRef) -> String {
    match &item.resource_key {
        Some(key) => format!("{base}id={}&resourcekey={key}", item.id),
        None => format!("{base}id={}", item.id),
    }
}

/// Direct download URL of a file.
pub fn download_url(file: &DriveRef) -> String {
    with_id(&format!("{DOWNLOAD_ENDPOINT}?export=download&confirm=t&"), file)
}

/// Listing page of a folder.
fn folder_url(folder: &DriveRef) -> String {
    with_id(&format!("{FOLDER_ENDPOINT}?"), folder)
}

/// The file name in a `Content-Disposition` header, e.g.
/// `attachment; filename="book.pdf"; filename*=UTF-8''b%C3%BCch.pdf`.
///
/// Prefers `filename*` (RFC 6266/5987: UTF-8, percent-encoded — how Drive
/// sends non-ASCII names) over the plain `filename`.
pub fn disposition_filename(value: &str) -> Option<String> {
    let mut plain = None;
    for param in split_params(value) {
        let Some((key, val)) = param.split_once('=') else { continue };
        match key.trim().to_ascii_lowercase().as_str() {
            "filename*" => {
                // charset'language'percent-encoded-name
                let encoded = val.trim().splitn(3, '\'').nth(2)?;
                let name = percent_decode_str(encoded).decode_utf8_lossy().into_owned();
                if !name.is_empty() { return Some(name); }
            }
            "filename" => plain = Some(unquote(val.trim())),
            _ => {}
        }
    }
    plain.filter(|n| !n.is_empty())
}

/// Splits a header value at `;`, except inside a quoted string.
fn split_params(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    for (i, c) in value.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                parts.push(value[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(value[start..].trim());
    parts
}

/// `"a \"b\".pdf"` → `a "b".pdf`; an unquoted value is returned as is.
fn unquote(value: &str) -> String {
    let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        return value.to_owned();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        out.push(if c == '\\' { chars.next().unwrap_or('\\') } else { c });
    }
    out
}

/// One entry of a folder listing.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    link:  DriveLink,
    /// The name shown in the listing, e.g. `Book.pdf` or a sub-folder's name.
    title: String,
}

/// Entries of a folder listing page (`embeddedfolderview`): one
/// `div.flip-entry` per item, holding its link and a `.flip-entry-title`.
/// Google Docs and anything else that is not a Drive file or folder is left
/// out.
fn parse_listing(html: &str) -> Vec<Entry> {
    let (Ok(entry_sel), Ok(link_sel), Ok(title_sel)) = (
        Selector::parse("div.flip-entry"),
        Selector::parse("a[href]"),
        Selector::parse(".flip-entry-title"),
    ) else {
        return Vec::new();
    };
    let doc = Html::parse_document(html);
    doc.select(&entry_sel)
        .filter_map(|entry| {
            let href = entry.select(&link_sel).next()?.value().attr("href")?;
            let link = parse_link(&Url::parse(href).ok()?)?;
            let title = entry.select(&title_sel).next().map(|t| t.text().collect::<String>()).unwrap_or_default();
            Some(Entry { link, title: title.trim().to_owned() })
        })
        .collect()
}

// =============================================================================
// Talking to Drive
// =============================================================================

/// What Drive said about one file.
#[derive(Debug)]
enum Lookup {
    /// Downloadable; holds the file's name.
    Named(String),
    /// Private, deleted or over its download limit: Drive answered 4xx or an
    /// HTML page (sign-in, "quota exceeded") instead of a file.
    Unavailable,
}

/// `Err` for answers worth another try (429 Too Many Requests, 5xx).
fn retryable(status: StatusCode) -> Result<()> {
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        return Err(format!("HTTP {status}").into());
    }
    Ok(())
}

/// Asks for the first byte of `file` — enough for Drive to name the file.
async fn look_up(client: &Client, file: &DriveRef) -> Result<Lookup> {
    let resp = client
        .get(download_url(file))
        .header(header::RANGE, "bytes=0-0")
        .send()
        .await?;
    let status = resp.status();
    retryable(status)?;
    let name = resp
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .and_then(|v| disposition_filename(&v));
    // The body is never read, so a server ignoring `Range` costs nothing.
    Ok(match name {
        Some(name) if status.is_success() => Lookup::Named(name),
        _ => Lookup::Unavailable,
    })
}

/// What listing one folder gave.
#[derive(Debug)]
enum Listing {
    Entries(Vec<Entry>),
    /// Private (401/403, or a sign-in page) or deleted (404).
    Unavailable,
}

async fn list_folder(client: &Client, folder: &DriveRef) -> Result<Listing> {
    let resp = client.get(folder_url(folder)).send().await?;
    let status = resp.status();
    retryable(status)?;
    if !status.is_success() {
        return Ok(Listing::Unavailable);
    }
    let html = resp.text().await?;
    // Every real listing has this container, even an empty one.
    if !html.contains("flip-entries") {
        return Ok(Listing::Unavailable);
    }
    Ok(Listing::Entries(parse_listing(&html)))
}

/// A file found in a folder listing, with the name the listing gives it.
#[derive(Debug)]
struct Listed {
    file: DriveRef,
    name: String,
}

/// What walking folders turned up.
#[derive(Debug, Default)]
struct FolderWalk {
    /// Files whose listed name has a wanted extension, in listing order.
    files:       Vec<Listed>,
    /// Listed names of files of other types.
    other_types: Vec<String>,
    /// Ids of folders that are private or deleted.
    unavailable: Vec<String>,
    listed:      usize,
    /// Folders not listed because of `MAX_FOLDERS` / `MAX_FOLDER_DEPTH`.
    not_listed:  usize,
    failed:      usize,
    first_error: Option<String>,
}

/// Lists `roots` and their sub-folders, level by level, and collects the
/// files of the wanted types. Each folder is listed once, however often it
/// is linked.
async fn walk_folders(client: &Client, roots: &[&DriveRef], exts: &[&str], quiet: bool) -> FolderWalk {
    let spinner = if quiet { ProgressBar::hidden() } else { ProgressBar::new_spinner() };
    spinner.enable_steady_tick(Duration::from_millis(120));

    let mut walk = FolderWalk::default();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut level: Vec<DriveRef> =
        roots.iter().filter(|f| seen_ids.insert(f.id.clone())).map(|f| (*f).clone()).collect();

    for depth in 0..=MAX_FOLDER_DEPTH {
        if level.is_empty() { break; }
        let room = MAX_FOLDERS.saturating_sub(walk.listed);
        if level.len() > room {
            walk.not_listed += level.len() - room;
            level.truncate(room);
        }
        walk.listed += level.len();
        spinner.set_message(format!("listing Google Drive folders… {} so far", walk.listed));

        // `buffered` keeps listings in order, so files come out in folder order.
        let listings: Vec<Result<Listing>> = stream::iter(&level)
            .map(|folder| with_backoff(ATTEMPTS, move || list_folder(client, folder)))
            .buffered(LOOKUP_JOBS)
            .collect()
            .await;

        let mut next = Vec::new();
        for (folder, listing) in level.iter().zip(listings) {
            match listing {
                Ok(Listing::Entries(entries)) => {
                    for entry in entries {
                        match entry.link {
                            DriveLink::File(file) if matches_ext(&entry.title, exts) => {
                                walk.files.push(Listed { file, name: entry.title });
                            }
                            DriveLink::File(_) => walk.other_types.push(entry.title),
                            DriveLink::Folder(sub) => {
                                if !seen_ids.insert(sub.id.clone()) { continue; }
                                if depth < MAX_FOLDER_DEPTH { next.push(sub) } else { walk.not_listed += 1 }
                            }
                        }
                    }
                }
                Ok(Listing::Unavailable) => walk.unavailable.push(folder.id.clone()),
                Err(e) => {
                    walk.failed += 1;
                    walk.first_error.get_or_insert(e.to_string());
                }
            }
        }
        level = next;
    }
    spinner.finish_and_clear();
    walk
}

// =============================================================================
// Resolving links into files
// =============================================================================

/// The downloadable files behind `links` whose names match `exts`: folders
/// are walked, and linked files (whose names are unknown) are looked up.
/// Unavailable items, files of other types and failures are reported as
/// notes and warnings, never errors.
pub async fn resolve(client: &Client, links: &[DriveLink], exts: &[&str], quiet: bool) -> Vec<FileSource> {
    let folders: Vec<&DriveRef> = links
        .iter()
        .filter_map(|l| match l { DriveLink::Folder(f) => Some(f), DriveLink::File(_) => None })
        .collect();
    let walk = if folders.is_empty() {
        FolderWalk::default()
    } else {
        walk_folders(client, &folders, exts, quiet).await
    };
    if !quiet && walk.listed > 0 {
        println!(
            "{} {} Google Drive folder(s) listed: {} {} file(s) inside",
            "→".dimmed(), walk.listed, walk.files.len(), exts.join("/"),
        );
    }
    if walk.not_listed > 0 {
        eprintln!(
            "{} {} more Google Drive folder(s) not listed (limit: {MAX_FOLDERS} folders, {MAX_FOLDER_DEPTH} levels deep).",
            "warning:".yellow(), walk.not_listed,
        );
    }
    if let Some(e) = &walk.first_error {
        eprintln!(
            "{} {} Google Drive folder(s) could not be listed (first error: {e}).",
            "warning:".yellow(), walk.failed,
        );
    }

    // Linked files are looked up, each once; one that is also in a listed
    // folder needs no lookup, the listing already named it.
    let mut seen: HashSet<&str> = walk.files.iter().map(|l| l.file.id.as_str()).collect();
    let mut files: Vec<&DriveRef> = links
        .iter()
        .filter_map(|l| match l { DriveLink::File(f) => Some(f), DriveLink::Folder(_) => None })
        .filter(|f| seen.insert(&f.id))
        .collect();
    if files.len() > MAX_FILES {
        eprintln!(
            "{} {} Google Drive files linked; only the first {MAX_FILES} are looked up.",
            "warning:".yellow(), files.len(),
        );
        files.truncate(MAX_FILES);
    }

    let mut looked_up = look_up_all(client, &files, exts, quiet).await;
    // Linked files first, then the files found in folders.
    looked_up.found.extend(
        walk.files.iter().map(|l| FileSource { name: l.name.clone(), url: download_url(&l.file) }),
    );
    if !quiet {
        let mut unavailable: Vec<&str> = looked_up.unavailable.iter().map(String::as_str).collect();
        unavailable.extend(walk.unavailable.iter().map(String::as_str));
        if !unavailable.is_empty() {
            println!(
                "{} {} Google Drive item(s) are private, deleted or over their download limit: {}",
                "note:".cyan(), unavailable.len(), short_list(&unavailable, MAX_NAMES_SHOWN),
            );
        }
        let other: Vec<&str> =
            looked_up.other_types.iter().chain(&walk.other_types).map(String::as_str).collect();
        if !other.is_empty() {
            println!(
                "{} {} Google Drive file(s) are not {}: {}",
                "note:".cyan(), other.len(), exts.join("/"), short_list(&other, MAX_NAMES_SHOWN),
            );
        }
    }
    looked_up.found
}

/// What looking up a batch of files gave.
#[derive(Debug, Default)]
struct LookedUp {
    /// Downloadable files of the wanted types, in input order.
    found:       Vec<FileSource>,
    /// Ids of files that are private, deleted or over their limit.
    unavailable: Vec<String>,
    /// Names of files of other types.
    other_types: Vec<String>,
}

/// Looks up every file, `LOOKUP_JOBS` at a time.
async fn look_up_all(client: &Client, files: &[&DriveRef], exts: &[&str], quiet: bool) -> LookedUp {
    let mut result = LookedUp::default();
    if files.is_empty() { return result; }

    let pb = if quiet { ProgressBar::hidden() } else { ProgressBar::new(files.len() as u64) };
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} Google Drive files [{bar:30.cyan/blue}] {pos}/{len}")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("█▓░"),
    );
    pb.enable_steady_tick(Duration::from_millis(120));

    // Unordered so one slow answer doesn't hold up the rest; the index
    // restores input order afterwards.
    let mut outcomes: Vec<(usize, Result<Lookup>)> = stream::iter(files.iter().enumerate())
        .map(|(i, file)| {
            let pb = &pb;
            async move {
                let r = with_backoff(ATTEMPTS, || look_up(client, file)).await;
                pb.inc(1);
                (i, r)
            }
        })
        .buffer_unordered(LOOKUP_JOBS)
        .collect()
        .await;
    pb.finish_and_clear();
    outcomes.sort_unstable_by_key(|(i, _)| *i);

    let mut failed = 0usize;
    let mut first_error = None;
    for (i, outcome) in outcomes {
        let file = files[i];
        match outcome {
            Ok(Lookup::Named(name)) if matches_ext(&name, exts) => {
                result.found.push(FileSource { name, url: download_url(file) });
            }
            Ok(Lookup::Named(name)) => result.other_types.push(name),
            Ok(Lookup::Unavailable) => result.unavailable.push(file.id.clone()),
            Err(e) => {
                failed += 1;
                first_error.get_or_insert(e);
            }
        }
    }
    if let Some(e) = first_error {
        eprintln!(
            "{} {failed} Google Drive file(s) could not be looked up (first error: {e}).",
            "warning:".yellow(),
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "1Rx7MAekKZbyNVc5B2MNfR0HXXIEJCksp";
    const FOLDER: &str = "17-9yPFfkiO6sqEDcYMIKLKGCKT_vaSsp";

    fn link(raw: &str) -> Option<DriveLink> {
        parse_link(&Url::parse(raw).unwrap())
    }

    fn drive_ref(id: &str) -> DriveRef {
        DriveRef { id: id.to_owned(), resource_key: None }
    }

    fn file(id: &str) -> Option<DriveLink> {
        Some(DriveLink::File(drive_ref(id)))
    }

    fn folder(id: &str) -> Option<DriveLink> {
        Some(DriveLink::Folder(drive_ref(id)))
    }

    #[test]
    fn file_link_forms() {
        for raw in [
            format!("https://drive.google.com/file/d/{ID}/view"),
            format!("https://drive.google.com/file/d/{ID}/view?usp=sharing"),
            format!("https://drive.google.com/file/d/{ID}/edit"),
            format!("https://drive.google.com/file/d/{ID}/preview"),
            format!("https://drive.google.com/file/d/{ID}"),
            format!("https://drive.google.com/file/u/1/d/{ID}/view"),
            format!("https://drive.google.com/open?id={ID}&usp=drive_fs"),
            format!("https://drive.google.com/uc?id={ID}&export=download"),
            format!("https://docs.google.com/uc?export=download&id={ID}"),
            format!("https://drive.usercontent.google.com/download?id={ID}&export=download"),
            format!("http://drive.google.com/file/d/{ID}/view"),
        ] {
            assert_eq!(link(&raw), file(ID), "{raw}");
        }
    }

    #[test]
    fn folder_link_forms() {
        for raw in [
            format!("https://drive.google.com/drive/folders/{FOLDER}"),
            format!("https://drive.google.com/drive/folders/{FOLDER}?usp=sharing"),
            format!("https://drive.google.com/drive/u/0/folders/{FOLDER}"),
            format!("https://drive.google.com/drive/mobile/folders/{FOLDER}"),
            format!("https://drive.google.com/folderview?id={FOLDER}"),
            format!("https://drive.google.com/embeddedfolderview?id={FOLDER}#list"),
        ] {
            assert_eq!(link(&raw), folder(FOLDER), "{raw}");
        }
    }

    #[test]
    fn resource_key_is_kept() {
        let key = Some("0-AbC_d-12345".to_owned());
        assert_eq!(
            link(&format!("https://drive.google.com/file/d/{ID}/view?resourcekey=0-AbC_d-12345")),
            Some(DriveLink::File(DriveRef { id: ID.to_owned(), resource_key: key.clone() })),
        );
        assert_eq!(
            link(&format!("https://drive.google.com/drive/folders/{FOLDER}?resourcekey=0-AbC_d-12345&usp=share_link")),
            Some(DriveLink::Folder(DriveRef { id: FOLDER.to_owned(), resource_key: key })),
        );
    }

    #[test]
    fn not_drive_files_or_folders() {
        assert_eq!(link("https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrStUvWxYz/edit"), None);
        assert_eq!(link("https://drive.google.com/"), None);
        assert_eq!(link("https://drive.google.com/open"), None);
        assert_eq!(link("https://drive.google.com/drive/my-drive"), None);
        assert_eq!(link(&format!("https://drive.google.com.evil.com/file/d/{ID}/view")), None);
        assert_eq!(link(&format!("https://example.com/drive/folders/{FOLDER}")), None);
        assert_eq!(link("https://drive.google.com/file/d/short/view"), None);
        assert_eq!(link("https://drive.google.com/open?id=abc%26export%3Dx%2Fyz123"), None);
    }

    #[test]
    fn urls_built_from_ids() {
        let plain = drive_ref(ID);
        assert_eq!(
            download_url(&plain),
            format!("https://drive.usercontent.google.com/download?export=download&confirm=t&id={ID}"),
        );
        let keyed = DriveRef { id: FOLDER.to_owned(), resource_key: Some("0-AbC_d-12345".to_owned()) };
        assert_eq!(
            folder_url(&keyed),
            format!("https://drive.google.com/embeddedfolderview?id={FOLDER}&resourcekey=0-AbC_d-12345"),
        );
        // Both are themselves Drive links to the same item.
        assert_eq!(link(&download_url(&plain)), Some(DriveLink::File(plain)));
        assert_eq!(link(&folder_url(&keyed)), Some(DriveLink::Folder(keyed)));
    }

    #[test]
    fn listing_entries() {
        // Trimmed from a real `embeddedfolderview` page.
        let html = r#"<div class="flip-entries">
            <div class="flip-entry" id="entry-1hfYHuDicBl5rck3pJM3O-rwa4s0m2BQM"><div class="flip-entry-info">
              <a href="https://drive.google.com/file/d/1hfYHuDicBl5rck3pJM3O-rwa4s0m2BQM/view?usp=drive_web" target="_blank">
              <div class="flip-entry-title">[CERN] Magnetic Components &amp; Converters.pdf</div></a></div></div>
            <div class="flip-entry" id="entry-0Bwxfq4Y7f7vkeXRQWk91YVF3MkE"><div class="flip-entry-info">
              <a href="https://drive.google.com/drive/folders/0Bwxfq4Y7f7vkeXRQWk91YVF3MkE?resourcekey=0-PyWvn62RTq4drIitu7KrEg">
              <div class="flip-entry-title">Lectures</div></a></div></div>
            <div class="flip-entry" id="entry-1AbCdEfGhIjKlMnOpQrStUvWxYz"><div class="flip-entry-info">
              <a href="https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrStUvWxYz/edit?usp=drive_web">
              <div class="flip-entry-title">courseinfo</div></a></div></div>
        </div>"#;
        assert_eq!(
            parse_listing(html),
            [
                Entry {
                    link:  DriveLink::File(drive_ref("1hfYHuDicBl5rck3pJM3O-rwa4s0m2BQM")),
                    title: "[CERN] Magnetic Components & Converters.pdf".to_owned(),
                },
                Entry {
                    link: DriveLink::Folder(DriveRef {
                        id:           "0Bwxfq4Y7f7vkeXRQWk91YVF3MkE".to_owned(),
                        resource_key: Some("0-PyWvn62RTq4drIitu7KrEg".to_owned()),
                    }),
                    title: "Lectures".to_owned(),
                },
            ],
        );
        assert!(parse_listing("<html><title>Sign in</title></html>").is_empty());
    }

    #[test]
    fn retry_only_when_busy() {
        assert!(retryable(StatusCode::TOO_MANY_REQUESTS).is_err());
        assert!(retryable(StatusCode::SERVICE_UNAVAILABLE).is_err());
        assert!(retryable(StatusCode::NOT_FOUND).is_ok());
        assert!(retryable(StatusCode::UNAUTHORIZED).is_ok());
        assert!(retryable(StatusCode::OK).is_ok());
    }

    #[test]
    fn disposition_names() {
        // What Drive sends.
        assert_eq!(disposition_filename(r#"attachment; filename="optimization.pdf""#).as_deref(), Some("optimization.pdf"));
        // `filename*` wins and is UTF-8 decoded.
        assert_eq!(
            disposition_filename(r#"attachment; filename="B?ch.pdf"; filename*=UTF-8''B%C3%BCch%20eins.pdf"#).as_deref(),
            Some("Büch eins.pdf"),
        );
        // `;` and escaped quotes inside a quoted name.
        assert_eq!(
            disposition_filename(r#"attachment; filename="a; \"b\".pdf""#).as_deref(),
            Some(r#"a; "b".pdf"#),
        );
        assert_eq!(disposition_filename("attachment; filename=plain.pdf").as_deref(), Some("plain.pdf"));
        assert_eq!(disposition_filename("attachment").as_deref(), None);
        assert_eq!(disposition_filename(r#"attachment; filename="""#).as_deref(), None);
        assert_eq!(disposition_filename("").as_deref(), None);
    }
}
