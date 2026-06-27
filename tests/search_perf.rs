//! T-14 — Search + Highlight perf guards (AC-22).
//!
//! Builds ~5,000 synthetic lines with a common token ("fn") appearing on most
//! lines (the realistic worst case), then:
//!   • asserts `search::find_matches` completes within 300 ms, and
//!   • asserts `highlight::apply` over the same ~5,000 lines + all matches
//!     completes within 300 ms.
//!
//! Also exercises an incremental re-search loop (8 successive growing queries)
//! to simulate the keystroke-by-keystroke path.
//!
//! These are generous guardrails (AC-22 budget = 300 ms), not microbenchmarks.

use ratatui::text::Line;
use std::time::{Duration, Instant};

use herdr_file_viewer::highlight::apply;
use herdr_file_viewer::search::find_matches;

/// Number of synthetic lines — at the AC-22 cap.
const N_LINES: usize = 5_000;

/// Build the synthetic content: ~N_LINES lines of ~80 chars each with "fn"
/// appearing multiple times per line so there are thousands of matches total.
///
/// Example line pattern:
///   "fn process_line_0042(fn_arg: u32) -> fn_result { fn_body() } // fn end"
/// That is ~72 chars, ASCII-only, with "fn" appearing 5× per line.
fn build_lines() -> Vec<String> {
    (0..N_LINES)
        .map(|i| {
            format!("fn process_line_{i:04}(fn_arg: u32) -> fn_result {{ fn_body() }} // fn end",)
        })
        .collect()
}

/// Convert `Vec<String>` to `Vec<Line<'static>>` as required by `highlight::apply`.
fn to_ratatui_lines(strings: &[String]) -> Vec<Line<'static>> {
    strings.iter().map(|s| Line::raw(s.clone())).collect()
}

/// AC-22: `find_matches` over ~5,000 lines with a common short token stays within 300 ms.
#[test]
fn find_matches_within_300ms_at_5k_lines() {
    let lines = build_lines();

    // "fn" is all-lowercase → case-insensitive path; appears 5× per line → ~25,000 matches.
    let t = Instant::now();
    let matches = find_matches("fn", &lines);
    let elapsed = t.elapsed();

    // Sanity: we really do have many matches.
    assert!(
        matches.len() >= N_LINES * 4,
        "expected at least {} matches (many-match worst case), got {}",
        N_LINES * 4,
        matches.len()
    );

    assert!(
        elapsed < Duration::from_millis(300),
        "AC-22: find_matches over {N_LINES} lines took {elapsed:?}, exceeds 300 ms budget"
    );
}

/// AC-22: `highlight::apply` over ~5,000 lines + thousands of matches stays within 300 ms.
#[test]
fn highlight_apply_within_300ms_at_5k_lines() {
    let lines = build_lines();
    let matches = find_matches("fn", &lines);

    // Convert to ratatui Lines outside the timed region.
    let ratatui_lines = to_ratatui_lines(&lines);

    let t = Instant::now();
    let _highlighted = apply(&ratatui_lines, &matches, 0);
    let elapsed = t.elapsed();

    assert!(
        elapsed < Duration::from_millis(300),
        "AC-22: highlight::apply over {N_LINES} lines / {} matches took {elapsed:?}, exceeds 300 ms budget",
        matches.len()
    );
}

/// AC-22: incremental re-search loop (8 successive growing queries simulating typing)
/// — each call to `find_matches` stays within 300 ms.
#[test]
fn incremental_search_each_keystroke_within_300ms() {
    let lines = build_lines();

    // Simulate typing "fn_body" one character at a time: 8 keystrokes.
    let prefixes = [
        "f", "fn", "fn_", "fn_b", "fn_bo", "fn_bod", "fn_body", "fn_body(",
    ];

    for query in prefixes {
        let t = Instant::now();
        let _matches = find_matches(query, &lines);
        let elapsed = t.elapsed();

        assert!(
            elapsed < Duration::from_millis(300),
            "AC-22: find_matches(\"{query}\") over {N_LINES} lines took {elapsed:?}, exceeds 300 ms budget"
        );
    }
}
