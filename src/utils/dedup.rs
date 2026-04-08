//! Generic deduplication helpers.

use rustc_hash::FxHashSet;
use std::hash::Hash;

/// Remove duplicate strings while preserving insertion order.
///
/// # Examples
/// ```
/// let v = dedup_strings(vec!["a".into(), "b".into(), "a".into()]);
/// assert_eq!(v, ["a", "b"]);
/// ```
pub fn dedup_strings(mut items: Vec<String>) -> Vec<String> {
    let mut seen: FxHashSet<String> = FxHashSet::with_capacity_and_hasher(items.len(), Default::default());
    items.retain(|item| {
        if seen.contains(item) {
            false
        } else {
            seen.insert(item.clone());
            true
        }
    });
    items
}

/// Remove duplicate items by a key function, preserving insertion order.
///
/// # Examples
/// ```
/// let v = dedup_by_key(vec![("a", 1), ("b", 2), ("a", 3)], |(k, _)| k);
/// // → [("a", 1), ("b", 2)]
/// ```
pub fn dedup_by_key<T, K, F>(mut items: Vec<T>, key_fn: F) -> Vec<T>
where
    K: Eq + Hash,
    F: Fn(&T) -> K,
{
    let mut seen: FxHashSet<K> = FxHashSet::with_capacity_and_hasher(items.len(), Default::default());
    items.retain(|item| seen.insert(key_fn(item)));
    items
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
