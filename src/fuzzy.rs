//! Fuzzy Matcher — case-insensitive subsequence search with basename weighting.
//!
//! Provides [`match_and_rank`] to filter a list of root-relative path strings and return
//! the indices of those that match a query, ordered by a basename-preferring score (AC-3,
//! AC-4, AC-6, AC-N3). Pure function; no I/O; std only.

/// Returns the indices of `candidates` for which `query` is a case-insensitive subsequence
/// of the candidate's full path, ordered best-first (basename hits before directory-only
/// hits, then by shorter path as a tie-breaker). Case folding is ASCII-only.
///
/// An empty `query` returns an empty `Vec` immediately (AC-2 backing).
///
/// # Scoring (AC-4)
///
/// A match whose query chars all land inside the basename (the part after the last `/`)
/// earns a big bonus that always outranks a match that needs directory characters.  Ties
/// are broken by shorter total path length, then by original slice position.
///
/// Matching scans candidates without allocating folded copies of their paths. Ranking
/// groups indices by score, preserving input order inside each group.
pub fn match_and_rank(query: &str, candidates: &[String]) -> Vec<usize> {
    match_and_rank_cancellable(query, candidates, || false).unwrap_or_default()
}

/// The worker can abandon an obsolete query without finishing a full scan. `None` means
/// cancellation, never an empty result for a query that completed.
pub(crate) fn match_and_rank_cancellable(
    query: &str,
    candidates: &[String],
    cancelled: impl Fn() -> bool,
) -> Option<Vec<usize>> {
    if query.is_empty() {
        return Some(Vec::new());
    }

    let query_chars: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
    let ascii_query = query
        .is_ascii()
        .then(|| query.to_ascii_lowercase().into_bytes());

    // Scores depend only on basename membership and path length. Group by score
    // instead of comparison-sorting millions of (index, score) pairs. Appending in
    // traversal order preserves stable ties, and every phase remains cancellable.
    let mut groups: std::collections::BTreeMap<i64, Vec<usize>> = std::collections::BTreeMap::new();
    let mut total = 0;
    for (idx, path) in candidates.iter().enumerate() {
        if idx % 64 == 0 && cancelled() {
            return None;
        }
        if let Some(s) = score(&query_chars, ascii_query.as_deref(), path) {
            groups.entry(s).or_default().push(idx);
            total += 1;
        }
    }
    if cancelled() {
        return None;
    }

    let mut ranked = Vec::with_capacity(total);
    for group in groups.into_values() {
        for chunk in group.chunks(64) {
            if cancelled() {
                return None;
            }
            ranked.extend_from_slice(chunk);
        }
    }
    Some(ranked)
}

/// Returns `Some(score)` when `query_chars` is a case-insensitive subsequence of `path`,
/// or `None` otherwise.  Lower score = better rank.
fn score(query_chars: &[char], ascii_query: Option<&[u8]>, path: &str) -> Option<i64> {
    // Basename weighting: does the query also match as a subsequence of the basename alone?
    let basename = path.rfind('/').map(|i| &path[i + 1..]).unwrap_or(path);

    let matches = |text: &str| match ascii_query {
        Some(query) => is_ascii_subsequence(query, text.as_bytes()),
        None => is_subsequence(query_chars, text),
    };
    // A basename match is already a full-path match. Avoid scanning the directory
    // prefix and then scoring the same basename a second time on this common path.
    let basename_bonus: i64 = if matches(basename) {
        0 // bonus: sort first
    } else if matches(path) {
        1_000_000 // penalty: directory-only hit sorts after basename hits
    } else {
        return None;
    };

    // Tie-break by path length (shorter first).
    let length_score = path.len() as i64;

    Some(basename_bonus + length_score)
}

/// ASCII bytes cannot occur inside a non-ASCII UTF-8 codepoint, so this fast path
/// preserves character-based subsequence matching even for Unicode filenames.
fn is_ascii_subsequence(needle: &[u8], haystack: &[u8]) -> bool {
    let mut bytes = haystack.iter();
    needle
        .iter()
        .all(|&ch| bytes.any(|b| b.to_ascii_lowercase() == ch))
}

/// Returns `true` when every char in `needle` appears in `haystack` in order.
fn is_subsequence(needle: &[char], haystack: &str) -> bool {
    let mut chars = haystack.chars();
    for &ch in needle {
        if !chars.any(|c| c.to_ascii_lowercase() == ch) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn a_superseded_match_stops_before_scanning_all_candidates() {
        let paths = vec!["src/module.rs".into(); 10_000];
        let checks = Cell::new(0);
        let result = match_and_rank_cancellable("m", &paths, || {
            checks.set(checks.get() + 1);
            checks.get() == 3
        });
        assert!(result.is_none());
        assert_eq!(checks.get(), 3);
    }

    #[test]
    fn optimized_ranking_preserves_the_original_character_matcher_and_stable_ties() {
        // Deliberately separate oracle: the original allocating, char-based algorithm.
        fn subsequence(query: &[char], path: &str) -> bool {
            let folded: Vec<char> = path.chars().map(|c| c.to_ascii_lowercase()).collect();
            let mut at = 0;
            for c in query {
                while at < folded.len() && folded[at] != *c {
                    at += 1;
                }
                if at == folded.len() {
                    return false;
                }
                at += 1;
            }
            true
        }
        let parts = [
            "src",
            "Mód",
            "日本語",
            "module",
            "a_b",
            "🦀",
            "e\u{301}",
            "A",
            "\\x1b",
        ];
        let paths: Vec<String> = parts
            .iter()
            .flat_map(|a| parts.iter().map(move |b| format!("{a}/{b}.rs")))
            .collect();
        for query in [
            "", "m", "M", "sr", "modrs", "ó", "日", "🦀", "e\u{301}", "missing", "a_b", "R",
        ] {
            let folded: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
            let mut expected: Vec<_> = paths
                .iter()
                .enumerate()
                .filter_map(|(i, path)| {
                    if query.is_empty() || !subsequence(&folded, path) {
                        return None;
                    }
                    let basename = path.rsplit('/').next().unwrap();
                    let bonus = if subsequence(&folded, basename) {
                        0
                    } else {
                        1_000_000
                    };
                    Some((i, bonus + path.len()))
                })
                .collect();
            expected.sort_by_key(|&(_, score)| score);
            assert_eq!(
                match_and_rank(query, &paths),
                expected.into_iter().map(|(i, _)| i).collect::<Vec<_>>(),
                "{query:?}"
            );
        }
    }
}
