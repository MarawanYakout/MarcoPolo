//! Short, human-readable lists for notes and warnings.

/// Joins up to `max` names with `, ` and adds `, …` when some were left out,
/// so a note about 200 items stays one readable line.
///
/// # Examples
/// ```
/// assert_eq!(short_list(&["a", "b", "c"], 2), "a, b, …");
/// ```
pub fn short_list(names: &[&str], max: usize) -> String {
    let mut out = names.iter().take(max).copied().collect::<Vec<_>>().join(", ");
    if names.len() > max {
        out.push_str(", …");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_list_cuts_long_lists() {
        assert_eq!(short_list(&["a", "b", "c"], 2), "a, b, …");
        assert_eq!(short_list(&["a", "b"], 2), "a, b");
        assert_eq!(short_list(&["a"], 5), "a");
        assert_eq!(short_list(&[], 5), "");
    }
}
