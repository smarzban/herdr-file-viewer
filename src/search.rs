//! Search Matcher — pure substring search over a slice of lines.
//!
//! Implements smartcase: a query that contains no uppercase ASCII letters is matched
//! case-insensitively (ASCII case folding); a query containing any uppercase ASCII letter
//! is matched case-sensitively. Regex metacharacters are not interpreted — the query
//! is always treated as a literal substring.
//!
//! # Offset semantics
//! `Match::start` and `Match::end` are **byte offsets** into the original (un-folded) line
//! string such that `&lines[m.line][m.start..m.end]` is a valid UTF-8 slice equal to the
//! matched text. ASCII case folding is used for the case-insensitive path so byte offsets
//! stay aligned with the original line; non-ASCII uppercase letters trigger the case-sensitive
//! path (the check is `is_ascii_uppercase`), so non-ASCII uppercase queries are matched
//! case-sensitively (byte offsets remain valid into the original line).

/// A single non-overlapping substring match.
///
/// `line` is the 0-based index into the `lines` slice passed to [`find_matches`].
/// `start` and `end` are byte offsets (half-open `[start, end)`) into that line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// Find all non-overlapping occurrences of `query` within `lines`.
///
/// Returns matches in document order (line index ascending, then column ascending).
/// An empty query or a query with no occurrences returns an empty `Vec`.
///
/// Smartcase rule: if `query` contains no uppercase ASCII letters the search is
/// case-insensitive (ASCII fold); otherwise it is case-sensitive.
pub fn find_matches(query: &str, lines: &[String]) -> Vec<Match> {
    if query.is_empty() {
        return Vec::new();
    }

    // Determine case-sensitivity once for the whole call.
    let case_sensitive = query.chars().any(|c| c.is_ascii_uppercase());
    // In the case-insensitive path fold the needle once; ASCII fold is byte-length-preserving,
    // so match_indices offsets into the folded line stay valid into the original line.
    let needle = if case_sensitive {
        query.to_string()
    } else {
        query.to_ascii_lowercase()
    };

    let mut matches = Vec::new();
    for (line_idx, line) in lines.iter().enumerate() {
        // Avoid allocating in the case-sensitive path; fold only when needed.
        if case_sensitive {
            for (start, m) in line.match_indices(needle.as_str()) {
                matches.push(Match {
                    line: line_idx,
                    start,
                    end: start + m.len(),
                });
            }
        } else {
            let folded = line.to_ascii_lowercase();
            for (start, m) in folded.match_indices(needle.as_str()) {
                matches.push(Match {
                    line: line_idx,
                    start,
                    end: start + m.len(),
                });
            }
        }
    }
    matches
}
