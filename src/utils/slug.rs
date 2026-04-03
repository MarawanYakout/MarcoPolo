//! URL/filename slug generation.

/// Convert an arbitrary string into a safe filename component.
///
/// - Alphanumeric characters and hyphens are kept as-is.
/// - Everything else is replaced with `_`.
/// - Leading and trailing underscores are trimmed.
///
/// # Examples
/// ```
/// assert_eq!(slug("Clean Code"), "Clean_Code");
/// assert_eq!(slug("C++ Primer, 5th Edition"), "C___Primer__5th_Edition");
/// assert_eq!(slug("Design-Patterns"), "Design-Patterns");
/// ```
pub fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::slug;

    #[test]
    fn slug_basic_title() {
        assert_eq!(slug("Clean Code"), "Clean_Code");
    }

    #[test]
    fn slug_removes_outer_underscores() {
        // Leading/trailing non-alphanum chars should vanish after trim.
        assert_eq!(slug("!!Refactoring!!"), "Refactoring");
    }

    #[test]
    fn slug_handles_symbols() {
        assert_eq!(slug("C++ Primer, 5th Edition"), "C___Primer__5th_Edition");
    }

    #[test]
    fn slug_keeps_hyphens() {
        assert_eq!(slug("Design-Patterns"), "Design-Patterns");
    }

    #[test]
    fn slug_empty_input() {
        assert_eq!(slug(""), "");
    }

    #[test]
    fn slug_unicode_alphanum_kept() {
        // Non-ASCII alphanumerics pass `is_alphanumeric()`, so they are kept.
        assert_eq!(slug("café"), "café");
    }

    #[test]
    fn slug_all_separators() {
        assert_eq!(slug("a/b\\c:d"), "a_b_c_d");
    }

    #[test]
    fn slug_single_char() {
        assert_eq!(slug("A"), "A");
        assert_eq!(slug("."), "");
    }
}
