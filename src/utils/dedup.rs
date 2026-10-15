//! Generic deduplication helpers.

use rustc_hash::FxHashSet;
use std::hash::Hash;

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
