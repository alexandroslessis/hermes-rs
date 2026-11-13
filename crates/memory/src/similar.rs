//! Near-duplicate detection for `add` (Backend v2 Phase 9, CP9.3). Not from Hermes (it has no
//! dedupe): see `PORTING.md`, `diverged`.
//!
//! The plan says "BM25 top-1 above a tuned threshold". This is a deterministic token-set
//! comparison instead, for three reasons: it works for the `core` tier too (whose entries the
//! batch planner sees as plain texts, and which must still work with `memory.db` off), it needs no
//! database round trip inside the write's critical section, and BM25 over a handful of entries
//! says little (the score depends on how many other entries there are). The cost of one `add` is
//! one pass over the tier's entries (at most 5000 short ones).
//!
//! Two entries are "similar" when, after lowercasing, splitting into words, dropping stopwords
//! and trimming plural/verb endings (`stem`), either
//! - their word sets overlap by at least [`JACCARD_MIN`] (Jaccard, sets of at least
//!   [`MIN_WORDS`] words), or
//! - one set is (almost) contained in the other ([`CONTAINMENT_MIN`] of the smaller one), the
//!   smaller having at least [`CONTAINED_MIN_WORDS`] words: the new entry restates, or extends, an
//!   old one.
//!
//! "User prefers tabs" and "User prefers dark mode" share two words of five (0.4): distinct.
//! "The user prefers tabs over spaces" and "User prefers tabs, not spaces" are similar.

use std::collections::HashSet;

/// Word-set overlap (intersection over union) from which two entries are called similar.
pub const JACCARD_MIN: f64 = 0.6;
/// Share of the smaller word set found in the larger one from which they are called similar.
pub const CONTAINMENT_MIN: f64 = 0.9;
/// Fewest words an entry needs for the Jaccard rule (a two-word entry is too easily "similar").
pub const MIN_WORDS: usize = 3;
/// Fewest words the smaller entry needs for the containment rule.
pub const CONTAINED_MIN_WORDS: usize = 4;

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "has", "have", "in",
    "is", "it", "its", "of", "on", "or", "that", "the", "their", "this", "to", "was", "were",
    "will", "with",
];

/// A word with common endings trimmed, so "prefers"/"preferred"/"preferring" meet.
fn stem(word: &str) -> String {
    let mut w = word.to_string();
    for suffix in ["ing", "ed", "es", "s", "ly"] {
        if w.chars().count() > suffix.len() + 2 && w.ends_with(suffix) {
            w.truncate(w.len() - suffix.len());
            // "preferred" -> "preferr" -> "prefer"; "running" -> "runn" -> "run".
            if matches!(suffix, "ing" | "ed") {
                let mut tail = w.chars().rev();
                if let (Some(a), Some(b)) = (tail.next(), tail.next())
                    && a == b
                    && !"aeiou".contains(a)
                {
                    w.pop();
                }
            }
            break;
        }
    }
    // "space"/"spaces", "use"/"used" meet.
    if w.chars().count() > 3 && w.ends_with('e') {
        w.pop();
    }
    w
}

/// The distinct stemmed words of `text`.
pub fn word_set(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && (w.chars().count() > 1 || !w.is_ascii()))
        .filter(|w| !STOPWORDS.contains(w))
        .map(stem)
        .collect()
}

/// Lowercase, whitespace-collapsed text without trailing punctuation: two entries with the same
/// value say the same thing letter for letter.
pub fn normalized(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', '!', ';', ',', ' '])
        .to_string()
}

/// How alike two word sets are (0.0 to 1.0), or `None` when they are not similar by the rules in
/// the module doc.
pub fn similarity(a: &HashSet<String>, b: &HashSet<String>) -> Option<f64> {
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let shared = a.intersection(b).count() as f64;
    let union = (a.len() + b.len()) as f64 - shared;
    let jaccard = shared / union;
    if a.len() >= MIN_WORDS && b.len() >= MIN_WORDS && jaccard >= JACCARD_MIN {
        return Some(jaccard);
    }
    let smaller = a.len().min(b.len());
    if smaller >= CONTAINED_MIN_WORDS && shared / smaller as f64 >= CONTAINMENT_MIN {
        return Some(shared / smaller as f64);
    }
    None
}

/// The entry of `existing` most like `new` (first on ties), with its index and similarity;
/// `None` when none is similar. An entry equal to `new` byte for byte is skipped (that is an
/// exact duplicate, handled by the caller).
pub fn most_similar(new: &str, existing: &[String]) -> Option<(usize, f64)> {
    let words = word_set(new);
    let mut best: Option<(usize, f64)> = None;
    for (index, entry) in existing.iter().enumerate() {
        if entry == new {
            continue;
        }
        let Some(score) = similarity(&words, &word_set(entry)) else {
            continue;
        };
        if best.is_none_or(|(_, top)| score > top) {
            best = Some((index, score));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rewordings_are_similar_and_different_facts_are_not() {
        let existing = entries(&[
            "The user prefers tabs over spaces",
            "Build with cargo build -p hermes-desktop",
            "The staging server is at staging.example.com",
        ]);
        let hit = most_similar("User prefers tabs, not spaces", &existing);
        assert_eq!(hit.map(|h| h.0), Some(0), "{hit:?}");
        assert!(
            most_similar("User prefers dark mode", &entries(&["User prefers tabs"])).is_none()
        );
        assert!(most_similar("The deploy key lives in vault", &existing).is_none());
        // Plural and tense endings meet.
        assert!(
            most_similar("The user preferred tab characters over space", &existing).is_some()
        );
    }

    #[test]
    fn a_restatement_that_extends_an_entry_is_similar() {
        let existing = entries(&["Project uses pnpm for installs"]);
        let hit = most_similar(
            "Project uses pnpm for installs, never npm, because the lockfile differs",
            &existing,
        );
        assert!(hit.is_some(), "the old entry is contained in the new one");
    }

    #[test]
    fn short_entries_are_not_called_similar_too_easily() {
        assert!(most_similar("likes tea", &entries(&["likes coffee"])).is_none());
        assert!(most_similar("uses tabs", &entries(&["uses tabs always"])).is_none());
    }

    #[test]
    fn an_exact_copy_is_skipped_and_normalised_text_matches() {
        let existing = entries(&["Uses Rust 2024 edition."]);
        assert!(most_similar("Uses Rust 2024 edition.", &existing).is_none());
        assert_eq!(normalized("  Uses  RUST 2024 edition. "), normalized("uses rust 2024 edition"));
        // Same words, different case and punctuation: similar (a flag, not a silent drop).
        assert!(most_similar("uses rust 2024 edition", &existing).is_some());
    }
}
