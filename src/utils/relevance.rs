//! Scoring how well a result title matches the search query.

/// Lowercased alphanumeric words of `s` (`"Clean_Code.pdf"` → clean, code, pdf).
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Fraction of query words found in `title` (0.0–1.0), plus a 0.5 bonus when
/// the whole query appears in it as a contiguous phrase. Higher is better;
/// 0.0 means the title shares no word with the query.
pub fn relevance(query: &str, title: &str) -> f32 {
    let q = words(query);
    if q.is_empty() { return 0.0; }
    let t = words(title);

    let hits = q.iter().filter(|w| t.contains(w)).count();
    let mut score = hits as f32 / q.len() as f32;
    if t.windows(q.len()).any(|win| win == q.as_slice()) {
        score += 0.5;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::relevance;

    #[test]
    fn exact_phrase_scores_highest() {
        let exact   = relevance("clean code", "Clean Code: A Handbook");
        let partial = relevance("clean code", "Code Complete");
        let none    = relevance("clean code", "Moby Dick");
        assert!(exact > partial && partial > none);
        assert_eq!(none, 0.0);
    }

    #[test]
    fn scattered_words_beat_one_word() {
        assert!(relevance("rust book", "The Book of Rust") > relevance("rust book", "Rust"));
    }

    #[test]
    fn filenames_are_tokenised() {
        assert_eq!(relevance("clean code", "Clean_Code-2008.pdf"), 1.5);
    }

    #[test]
    fn empty_query_scores_zero() {
        assert_eq!(relevance("  ", "anything"), 0.0);
    }
}
