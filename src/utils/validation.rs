//! Input validation helpers for CLI arguments and search queries.

use clap::ValueEnum;
use std::fmt;

/// Supported file extensions for the `find` subcommand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SupportedExtension {
    Pdf,
    Epub,
    Txt,
    Mobi,
    Djvu,
}

impl fmt::Display for SupportedExtension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pdf => write!(f, "pdf"),
            Self::Epub => write!(f, "epub"),
            Self::Txt => write!(f, "txt"),
            Self::Mobi => write!(f, "mobi"),
            Self::Djvu => write!(f, "djvu"),
        }
    }
}

/// Supported source identifiers for `--source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Source {
    Archive,
    Openlibrary,
    Gutenberg,
    Annas,
    Github,
    Googlescholar,
    Duckduckgo,
}

impl Source {
    pub const ALL: &'static [Source] = &[
        Source::Archive,
        Source::Openlibrary,
        Source::Gutenberg,
        Source::Annas,
        Source::Github,
        Source::Googlescholar,
        Source::Duckduckgo,
    ];
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Archive => write!(f, "archive"),
            Self::Openlibrary => write!(f, "openlibrary"),
            Self::Gutenberg => write!(f, "gutenberg"),
            Self::Annas => write!(f, "annas"),
            Self::Github => write!(f, "github"),
            Self::Googlescholar => write!(f, "googlescholar"),
            Self::Duckduckgo => write!(f, "duckduckgo"),
        }
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
    fn valid_query_nonempty() {
        assert!(is_valid_query("Clean Code"));
    }

    #[test]
    fn invalid_query_empty() {
        assert!(!is_valid_query(""));
        assert!(!is_valid_query("   "));
    }
}