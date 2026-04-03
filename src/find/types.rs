//! Shared result type for all `find` sources.

/// A single search result returned by any source.
#[derive(Debug, Clone)]
pub struct FindResult {
    /// The source that produced this result (e.g. `"archive.org"`).
    pub source: &'static str,
    /// Human-readable title of the book or document.
    pub title:  String,
    /// Direct download or landing-page URL.
    pub url:    String,
    /// File format hint if known (e.g. `Some("pdf")`).
    pub format: Option<String>,
    /// Relevance score when available (higher = better match).
    pub score:  Option<f32>,
    /// Suggested local filename derived from the title and format.
    pub filename: String,
}
