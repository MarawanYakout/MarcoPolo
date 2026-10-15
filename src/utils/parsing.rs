//! URL / filename parsing helpers.

use std::collections::HashSet;

use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, CONTROLS};
use url::Url;

/// Characters that must be escaped inside a single URL path segment.
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ').add(b'"').add(b'#').add(b'%').add(b'<')
    .add(b'>').add(b'?').add(b'`').add(b'{').add(b'}');

/// Longest filename (in bytes) we hand to the filesystem; most cap at 255.
const MAX_NAME_BYTES: usize = 200;

/// Extract the last path segment of a URL as a filename, percent-decoded
/// (`My%20Book.pdf` → `My Book.pdf`).
///
/// Query strings and fragments are stripped.  Returns `None` when the URL has
/// no meaningful last segment (e.g. bare domain).
///
/// # Examples
/// ```
/// assert_eq!(
///     filename_from_url("https://archive.org/download/book/book.pdf"),
///     Some("book.pdf".to_owned())
/// );
/// assert_eq!(
///     filename_from_url("https://archive.org/download/book/book.pdf?foo=bar"),
///     Some("book.pdf".to_owned())
/// );
/// ```
pub fn filename_from_url(raw: &str) -> Option<String> {
    let parsed = Url::parse(raw).ok()?;
    let segment = parsed.path_segments()?.rev().find(|s| !s.is_empty())?;
    Some(percent_decode_str(segment).decode_utf8_lossy().into_owned())
}

/// Percent-encode each `/`-separated segment of a repo path or branch name,
/// so names like `C# Notes.pdf` survive being put into a URL.
pub fn encode_path(path: &str) -> String {
    path.split('/')
        .map(|seg| utf8_percent_encode(seg, PATH_SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// Turn an untrusted name (URL segment, release asset, search title) into a
/// filename that always stays inside the output directory.
///
/// Only the last path component is kept, so `../x`, `/etc/x` and `a\b`
/// cannot escape; control characters are dropped and overly long names are
/// shortened while keeping the extension.
pub fn safe_filename(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() || trimmed.chars().all(|c| c == '.') {
        return "unknown".to_owned();
    }
    if trimmed.len() <= MAX_NAME_BYTES {
        return trimmed.to_owned();
    }
    let ext = match trimmed.rfind('.') {
        Some(i) if trimmed.len() - i <= 10 => &trimmed[i..],
        _ => "",
    };
    let mut end = MAX_NAME_BYTES - ext.len();
    while !trimmed.is_char_boundary(end) { end -= 1; }
    format!("{}{ext}", &trimmed[..end])
}

/// Return `name`, or `stem_1.ext`, `stem_2.ext`, … when it was already used,
/// so two different files never get written to the same path.
pub fn unique_filename(name: &str, used: &mut HashSet<String>) -> String {
    if used.insert(name.to_owned()) {
        return name.to_owned();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    (1..)
        .map(|n| format!("{stem}_{n}{ext}"))
        .find(|candidate| used.insert(candidate.clone()))
        .expect("unbounded counter always yields a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_basic() {
        assert_eq!(
            filename_from_url("https://archive.org/download/foo/bar.pdf"),
            Some("bar.pdf".to_owned())
        );
    }

    #[test]
    fn filename_strips_query() {
        assert_eq!(
            filename_from_url("https://example.com/file.pdf?token=abc"),
            Some("file.pdf".to_owned())
        );
    }

    #[test]
    fn filename_no_path() {
        // Bare domain — no filename to extract.
        assert_eq!(filename_from_url("https://example.com/"), None);
    }

    #[test]
    fn filename_deep_path() {
        assert_eq!(
            filename_from_url("https://example.com/a/b/c/document.epub"),
            Some("document.epub".to_owned())
        );
    }

    #[test]
    fn filename_is_percent_decoded() {
        assert_eq!(
            filename_from_url("https://example.com/My%20Book.pdf"),
            Some("My Book.pdf".to_owned())
        );
    }

    #[test]
    fn encode_path_escapes_specials() {
        assert_eq!(encode_path("books/C# Notes?.pdf"), "books/C%23%20Notes%3F.pdf");
    }

    #[test]
    fn safe_filename_blocks_traversal() {
        assert_eq!(safe_filename("../../.bashrc"), ".bashrc");
        assert_eq!(safe_filename("/etc/passwd"), "passwd");
        assert_eq!(safe_filename("..\\..\\evil.pdf"), "evil.pdf");
        assert_eq!(safe_filename(".."), "unknown");
        assert_eq!(safe_filename(""), "unknown");
    }

    #[test]
    fn safe_filename_keeps_normal_names() {
        assert_eq!(safe_filename("Clean Code.pdf"), "Clean Code.pdf");
    }

    #[test]
    fn safe_filename_truncates_keeping_extension() {
        let long = format!("{}.pdf", "a".repeat(300));
        let got = safe_filename(&long);
        assert_eq!(got.len(), MAX_NAME_BYTES);
        assert!(got.ends_with(".pdf"));
    }

    #[test]
    fn unique_filename_appends_counter() {
        let mut used = HashSet::new();
        assert_eq!(unique_filename("a.pdf", &mut used), "a.pdf");
        assert_eq!(unique_filename("a.pdf", &mut used), "a_1.pdf");
        assert_eq!(unique_filename("a.pdf", &mut used), "a_2.pdf");
        assert_eq!(unique_filename("README", &mut used), "README");
        assert_eq!(unique_filename("README", &mut used), "README_1");
    }
}
