use herdr_file_viewer::search::{Match, find_matches};

// ── AC-9: every occurrence found, multiple per line, document order ──────────

#[test]
fn single_match_in_single_line() {
    let lines = vec!["hello world".to_string()];
    let result = find_matches("world", &lines);
    assert_eq!(
        result,
        vec![Match {
            line: 0,
            start: 6,
            end: 11
        }],
        "should find 'world' at byte offset 6..11"
    );
}

#[test]
fn multiple_matches_on_same_line_in_order() {
    let lines = vec!["abcabc".to_string()];
    let result = find_matches("abc", &lines);
    assert_eq!(
        result,
        vec![
            Match {
                line: 0,
                start: 0,
                end: 3
            },
            Match {
                line: 0,
                start: 3,
                end: 6
            },
        ],
        "should find both non-overlapping occurrences of 'abc'"
    );
}

#[test]
fn matches_across_multiple_lines_in_document_order() {
    let lines = vec![
        "foo bar".to_string(),
        "baz foo".to_string(),
        "qux".to_string(),
    ];
    let result = find_matches("foo", &lines);
    assert_eq!(
        result,
        vec![
            Match {
                line: 0,
                start: 0,
                end: 3
            },
            Match {
                line: 1,
                start: 4,
                end: 7
            },
        ],
        "should find 'foo' in line 0 and line 1, skipping line 2"
    );
}

// ── AC-12: smartcase ─────────────────────────────────────────────────────────

#[test]
fn all_lowercase_query_matches_case_insensitively() {
    let lines = vec!["Hello World".to_string()];
    let result = find_matches("hello", &lines);
    // "hello" is all-lowercase → case-insensitive match
    assert_eq!(
        result,
        vec![Match {
            line: 0,
            start: 0,
            end: 5
        }],
        "all-lowercase query 'hello' should match 'Hello' case-insensitively"
    );
}

#[test]
fn all_lowercase_query_matches_mixed_case_occurrences() {
    let lines = vec!["FOO foo Foo".to_string()];
    let result = find_matches("foo", &lines);
    // All three should match since query is all-lowercase
    assert_eq!(
        result,
        vec![
            Match {
                line: 0,
                start: 0,
                end: 3
            },
            Match {
                line: 0,
                start: 4,
                end: 7
            },
            Match {
                line: 0,
                start: 8,
                end: 11
            },
        ],
        "all-lowercase query should match FOO, foo, Foo"
    );
}

#[test]
fn uppercase_query_matches_case_sensitively() {
    let lines = vec!["Hello hello HELLO".to_string()];
    let result = find_matches("Hello", &lines);
    // "Hello" has uppercase → case-sensitive; only the first occurrence matches
    assert_eq!(
        result,
        vec![Match {
            line: 0,
            start: 0,
            end: 5
        }],
        "query 'Hello' (has uppercase) should match only exact 'Hello'"
    );
}

#[test]
fn all_uppercase_query_matches_case_sensitively() {
    let lines = vec!["Hello hello HELLO".to_string()];
    let result = find_matches("HELLO", &lines);
    // "HELLO" has uppercase → case-sensitive
    assert_eq!(
        result,
        vec![Match {
            line: 0,
            start: 12,
            end: 17
        }],
        "query 'HELLO' (all-uppercase) should match only 'HELLO'"
    );
}

// ── AC-18: empty query and no-match → empty Vec ───────────────────────────────

#[test]
fn empty_query_returns_empty_vec() {
    let lines = vec!["hello world".to_string()];
    let result = find_matches("", &lines);
    assert!(result.is_empty(), "empty query must return empty Vec");
}

#[test]
fn query_matching_nothing_returns_empty_vec() {
    let lines = vec!["hello world".to_string()];
    let result = find_matches("xyz", &lines);
    assert!(result.is_empty(), "no-match query must return empty Vec");
}

#[test]
fn empty_lines_slice_returns_empty_vec() {
    let result = find_matches("foo", &[]);
    assert!(result.is_empty(), "no lines means no matches");
}

// ── AC-N4: matches only within the supplied lines ────────────────────────────

#[test]
fn matches_only_within_supplied_lines() {
    // Only two lines are passed; match must be in line 0 only
    let lines = vec!["needle here".to_string(), "nothing".to_string()];
    let result = find_matches("needle", &lines);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].line, 0, "match must be in line 0");
}

// ── AC-N5: regex metacharacters matched literally ─────────────────────────────

#[test]
fn dot_metachar_matched_literally() {
    let lines = vec!["a.b".to_string(), "axb".to_string()];
    let result = find_matches("a.b", &lines);
    // "." is literal: matches "a.b" but NOT "axb"
    assert_eq!(
        result,
        vec![Match {
            line: 0,
            start: 0,
            end: 3
        }],
        "'.' must be a literal dot, not a regex wildcard"
    );
}

#[test]
fn star_metachar_matched_literally() {
    let lines = vec!["a*b".to_string(), "ab".to_string(), "aXb".to_string()];
    let result = find_matches("a*b", &lines);
    assert_eq!(
        result,
        vec![Match {
            line: 0,
            start: 0,
            end: 3
        }],
        "'*' must be a literal asterisk"
    );
}

#[test]
fn bracket_metachar_matched_literally() {
    let lines = vec!["[abc]".to_string(), "a".to_string()];
    let result = find_matches("[abc]", &lines);
    assert_eq!(
        result,
        vec![Match {
            line: 0,
            start: 0,
            end: 5
        }],
        "'[...]' must be matched literally"
    );
}

#[test]
fn backslash_metachar_matched_literally() {
    let lines = vec!["path\\file".to_string(), "pathfile".to_string()];
    let result = find_matches("path\\file", &lines);
    assert_eq!(
        result,
        vec![Match {
            line: 0,
            start: 0,
            end: 9
        }],
        "backslash must be matched literally"
    );
}

// ── Offset semantics: start..end slices the original line ───────────────────

#[test]
fn byte_offsets_slice_original_line_correctly() {
    let lines = vec!["hello world".to_string()];
    let result = find_matches("world", &lines);
    assert_eq!(result.len(), 1);
    let m = result[0];
    assert_eq!(
        &lines[m.line][m.start..m.end],
        "world",
        "byte offsets must be valid UTF-8 boundaries slicing the original line"
    );
}

#[test]
fn case_insensitive_offsets_point_to_original_text() {
    // The line contains "Hello" (capital H); query is all-lowercase "hello"
    // Offsets must slice "Hello" from the original line (not the lowercased version)
    let lines = vec!["Hello".to_string()];
    let result = find_matches("hello", &lines);
    assert_eq!(result.len(), 1);
    let m = result[0];
    assert_eq!(
        &lines[m.line][m.start..m.end],
        "Hello",
        "offsets must point into the original (un-lowercased) line"
    );
}

// ── Non-overlapping: overlapping patterns find left-to-right non-overlapping ─

#[test]
fn non_overlapping_matches_left_to_right() {
    // "aaa" searched in "aaaa": first match at 0..3, second attempt starts at 3
    // Only one full non-overlapping match at 0..3; the leftover "a" at index 3 is too short
    let lines = vec!["aaaa".to_string()];
    let result = find_matches("aaa", &lines);
    assert_eq!(
        result,
        vec![Match {
            line: 0,
            start: 0,
            end: 3
        }],
        "overlapping pattern must yield only non-overlapping (left-to-right) matches"
    );
}
