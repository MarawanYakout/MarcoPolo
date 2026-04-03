//! URL / filename parsing helpers.

use url::Url;

/// Extract the last path segment of a URL as a filename.
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
    let segments: Vec<&str> = parsed
        .path_segments()?
        .filter(|s| !s.is_empty())
        .collect();
    let last = *segments.last()?;
    Some(last.to_owned())
}

/// Guess the file extension from a URL path (lowercase, without the dot).
///
/// Returns `None` when no extension can be determined.
pub fn extension_from_url(raw: &str) -> Option<String> {
    let name = filename_from_url(raw)?;
    let ext  = std::path::Path::new(&name)
        .extension()?
        .to_str()?
        .to_lowercase();
    Some(ext)
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
    fn extension_pdf() {
        assert_eq!(
            extension_from_url("https://archive.org/download/x/x.PDF"),
            Some("pdf".to_owned())
        );
    }

    #[test]
    fn extension_none_when_no_ext() {
        assert_eq!(extension_from_url("https://example.com/readme"), None);
    }
}
