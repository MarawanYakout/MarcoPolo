//! Generic deduplication helpers.

use std::collections::HashSet;
use std::hash::Hash;

/// Remove duplicate strings while preserving insertion order.
///
/// # Examples
/// ```
/// let v = dedup_strings(vec!["a".into(), "b".into(), "a".into()]);
/// assert_eq!(v, ["a", "b"]);
/// ```
pub fn dedup_strings(items: Vec<String>) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out:  Vec<String>     = Vec::new();
    for item in items {
        if seen.insert(item.clone()) {
            out.push(item);
        }
    }
    out
}

/// Remove duplicate items by a key function, preserving insertion order.
///
/// # Examples
/// ```
/// let v = dedup_by_key(vec![("a", 1), ("b", 2), ("a", 3)], |(k, _)| k);
/// // → [("a", 1), ("b", 2)]
/// ```
pub fn dedup_by_key<T, K, F>(items: Vec<T>, key_fn: F) -> Vec<T>
where
    K: Eq + Hash,
    F: Fn(&T) -> K,
{
    let mut seen: HashSet<K> = HashSet::new();
    let mut out:  Vec<T>     = Vec::new();
    for item in items {
        let k = key_fn(&item);
        if seen.insert(k) {
            out.push(item);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_removes_duplicates() {
        let input = vec!["a".into(), "b".into(), "a".into(), "c".into()];
        assert_eq!(
            dedup_strings(input),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn dedup_preserves_order() {
        let input = vec!["z".into(), "a".into(), "m".into(), "a".into()];
        assert_eq!(
            dedup_strings(input),
            vec!["z".to_string(), "a".to_string(), "m".to_string()]
        );
    }

    #[test]
    fn dedup_empty_vec() {
        assert!(dedup_strings(vec![]).is_empty());
    }

    #[test]
    fn dedup_all_duplicates() {
        let input = vec!["x".into(), "x".into(), "x".into()];
        assert_eq!(dedup_strings(input), vec!["x".to_string()]);
    }

    #[test]
    fn dedup_by_key_basic() {
        let v = vec![("a", 1usize), ("b", 2), ("a", 3)];
        let got = dedup_by_key(v, |(k, _)| *k);
        assert_eq!(got, vec![("a", 1), ("b", 2)]);
    }

    #[test]
    fn dedup_by_key_empty() {
        let v: Vec<(i32, i32)> = vec![];
        assert!(dedup_by_key(v, |(k, _)| *k).is_empty());
    }
}
