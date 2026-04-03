//! Input validation helpers for CLI arguments and search queries.

/// Supported file extensions for the `find` subcommand.
const SUPPORTED_EXTS: &[&str] = &["pdf", "epub", "txt", "mobi", "djvu"];

/// Supported source identifiers for `--source`.
const SUPPORTED_SOURCES: &[&str] = &["archive", "openlibrary", "gutenberg", "annas"];

/// Returns `true` if the extension is in the supported list (case-insensitive).
pub fn is_supported_extension(ext: &str) -> bool {
    let lower = ext.to_lowercase();
    SUPPORTED_EXTS.contains(&lower.as_str())
}

/// Returns `Ok(())` when `source` is a known source identifier,
/// or `Err` with a message listing valid choices.
pub fn validate_source(source: &str) -> Result<(), String> {
    let lower = source.to_lowercase();
    if SUPPORTED_SOURCES.contains(&lower.as_str()) {
        Ok(())
    } else {
        Err(format!(
            "Unknown source \"{}\" — valid choices: {}",
            source,
            SUPPORTED_SOURCES.join(", ")
        ))
    }
}

/// Returns `true` when a search query is non-empty and not all whitespace.
pub fn is_valid_query(query: &str) -> bool {
    !query.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_ext_pdf() {
        assert!(is_supported_extension("pdf"));
        assert!(is_supported_extension("PDF"));
    }

    #[test]
    fn unsupported_ext_mp4() {
        assert!(!is_supported_extension("mp4"));
    }

    #[test]
    fn validate_known_source() {
        assert!(validate_source("archive").is_ok());
        assert!(validate_source("OPENLIBRARY").is_ok());
        assert!(validate_source("gutenberg").is_ok());
        assert!(validate_source("annas").is_ok());
    }

    #[test]
    fn validate_unknown_source_errors() {
        let err = validate_source("libgen").unwrap_err();
        assert!(err.contains("archive"));
    }

    #[test]
    fn valid_query_nonempty() {
        assert!(is_valid_query("Clean Code"));
    }

    #[test]
    fn invalid_query_empty() {
        assert!(!is_valid_query(""));
        assert!(!is_valid_query("   "));
    }
}
