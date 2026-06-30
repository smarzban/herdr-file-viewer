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
/// are broken by shorter total path length, then by original slice position (stable sort
/// preserves this).
///
/// SHORTCUT: linear scan over candidates — fine for typical directory sizes (~1 k);
/// index if the caller's candidate list can grow into the tens of thousands.
pub fn match_and_rank(query: &str, candidates: &[String]) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }

    let query_chars: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();

    // Collect matching indices together with their score (lower = better).
    let mut scored: Vec<(usize, i64)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(idx, path)| score(&query_chars, path).map(|s| (idx, s)))
        .collect();

    // Stable sort so equal-scored candidates keep their original order.
    scored.sort_by_key(|&(_, s)| s);

    scored.into_iter().map(|(idx, _)| idx).collect()
}

/// Returns `Some(score)` when `query_chars` is a case-insensitive subsequence of `path`,
/// or `None` otherwise.  Lower score = better rank.
fn score(query_chars: &[char], path: &str) -> Option<i64> {
    let path_lower: Vec<char> = path.chars().map(|c| c.to_ascii_lowercase()).collect();

    // Check full-path subsequence.
    if !is_subsequence(query_chars, &path_lower) {
        return None;
    }

    // Basename weighting: does the query also match as a subsequence of the basename alone?
    let basename_lower: Vec<char> = path
        .rfind('/')
        .map(|i| &path[i + 1..])
        .unwrap_or(path)
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .collect();

    let basename_bonus: i64 = if is_subsequence(query_chars, &basename_lower) {
        0 // bonus: sort first
    } else {
        1_000_000 // penalty: directory-only hit sorts after basename hits
    };

    // Tie-break by path length (shorter first).
    let length_score = path.len() as i64;

    Some(basename_bonus + length_score)
}

/// Returns `true` when every char in `needle` appears in `haystack` in order.
fn is_subsequence(needle: &[char], haystack: &[char]) -> bool {
    let mut hi = 0;
    for &ch in needle {
        // Advance haystack until we find a match for ch.
        while hi < haystack.len() && haystack[hi] != ch {
            hi += 1;
        }
        if hi == haystack.len() {
            return false;
        }
        hi += 1;
    }
    true
}
