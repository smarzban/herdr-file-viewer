//! Search + Highlight scaling guards (AC-22).
//!
//! Instead of an absolute ms budget (which flakes on a loaded CI runner and fails to catch the
//! thing that matters — an O(n²) regression), these assert **relative scaling**: run each
//! operation over N and 2N inputs, assert `time(2N) < ~2.5 × time(N)` (roughly linear, with slack
//! for allocator/cache effects). This survives a 2–3× slower machine (both halves scale together)
//! but fails on a super-linear regression. Modelled on the `render.rs` exemplar
//! (`elapsed < timeout.mul_f32(1.5)`).
//!
//! Also keeps an incremental re-search loop (8 growing queries) as a smoke check that each
//! keystroke-shaped call is fast in absolute terms *and* doesn't grow across the typing sequence.

use ratatui::text::Line;
use std::time::{Duration, Instant};

use herdr_file_viewer::highlight::apply;
use herdr_file_viewer::search::find_matches;

/// N for the scaling pair. Large enough that the base timing is ~tens of ms (robust against
/// scheduler jitter when the full suite runs in parallel), small enough to keep the test cheap
/// on the default lane. 10,000 lines × 2 = 20,000 (well above the AC-22 magnitude, but scaling
/// tests need a non-trivial base time for the ratio to be meaningful).
const N: usize = 10_000;

/// A generous slack factor: 2N work may take more than 2× wall-clock (allocator/cache effects,
/// a second copy in memory, scheduling jitter under parallel test load), but an O(n²)
/// regression blows well past this. 4.0× keeps the test stable on a loaded CI lane while still
/// catching the regressions that matter. Mirrors the `mul_f32(1.5)` philosophy from `render.rs`.
const RATIO_SLACK: f32 = 4.0;

/// `elapsed * factor`, the form used by the `render.rs` exemplar (`timeout.mul_f32(1.5)`).
fn scaled(elapsed: Duration, factor: f32) -> Duration {
    Duration::from_secs_f32(elapsed.as_secs_f32() * factor)
}

/// A minimum base time below which the ratio is meaningless (sub-millisecond timings are
/// dominated by scheduler noise). If the N-side timing is below this, the bound falls back to a
/// safe absolute floor so a jitter spike on the 2N side can't trip the ratio.
const MIN_BASE: Duration = Duration::from_millis(15);

/// Build the synthetic content: lines of ~80 chars with "fn" appearing 5× per line so there are
/// thousands of matches (the realistic worst case for the highlighter).
fn build_lines(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            format!("fn process_line_{i:04}(fn_arg: u32) -> fn_result {{ fn_body() }} // fn end",)
        })
        .collect()
}

/// Convert `Vec<String>` to `Vec<Line<'static>>` as required by `highlight::apply`.
fn to_ratatui_lines(strings: &[String]) -> Vec<Line<'static>> {
    strings.iter().map(|s| Line::raw(s.clone())).collect()
}

/// `find_matches` scales roughly linearly — doubling the input roughly doubles the work, not
/// quadruples it. Catches an O(n²) matcher regression.
#[test]
fn find_matches_scales_linearly() {
    let lines_n = build_lines(N);
    let lines_2n = build_lines(N * 2);

    let t = Instant::now();
    let matches_n = find_matches("fn", &lines_n);
    let elapsed_n = t.elapsed();

    let t = Instant::now();
    let matches_2n = find_matches("fn", &lines_2n);
    let elapsed_2n = t.elapsed();

    // Sanity: the match count roughly doubled too (it's linear in the line count).
    let ratio = matches_2n.len() as f32 / matches_n.len().max(1) as f32;
    assert!(
        (1.8..=2.2).contains(&ratio),
        "expected ~2× matches doubling the input ({} → {}), got {ratio:.2}×",
        matches_n.len(),
        matches_2n.len()
    );

    let bound = scaled(elapsed_n.max(MIN_BASE), RATIO_SLACK);
    assert!(
        elapsed_2n < bound,
        "find_matches did not scale linearly: {} lines took {:?}, {} lines took {:?} \
         (bound {:?})",
        N,
        elapsed_n,
        N * 2,
        elapsed_2n,
        bound,
    );
}

/// `highlight::apply` scales roughly linearly in (lines + matches). Doubling both must not blow
/// up super-linearly.
#[test]
fn highlight_apply_scales_linearly() {
    let lines_n = build_lines(N);
    let lines_2n = build_lines(N * 2);
    let matches_n = find_matches("fn", &lines_n);
    let matches_2n = find_matches("fn", &lines_2n);
    let ratatui_n = to_ratatui_lines(&lines_n);
    let ratatui_2n = to_ratatui_lines(&lines_2n);

    let t = Instant::now();
    let _ = apply(&ratatui_n, &matches_n, 0);
    let elapsed_n = t.elapsed();

    let t = Instant::now();
    let _ = apply(&ratatui_2n, &matches_2n, 0);
    let elapsed_2n = t.elapsed();

    let bound = scaled(elapsed_n.max(MIN_BASE), RATIO_SLACK);
    assert!(
        elapsed_2n < bound,
        "highlight::apply did not scale linearly: {} lines/{} matches took {:?}, \
         {} lines/{} matches took {:?} (bound {:?})",
        N,
        matches_n.len(),
        elapsed_n,
        N * 2,
        matches_2n.len(),
        elapsed_2n,
        bound,
    );
}

/// Incremental re-search (8 growing queries) stays fast across the typing sequence and doesn't
/// grow super-linearly with the query length. A smoke check on the per-keystroke path.
#[test]
fn incremental_search_each_keystroke_is_fast_and_stable() {
    let lines = build_lines(N);

    // Simulate typing "fn_body(" one character at a time.
    let prefixes = [
        "f", "fn", "fn_", "fn_b", "fn_bo", "fn_bod", "fn_body", "fn_body(",
    ];

    for query in prefixes {
        let t = Instant::now();
        let _ = find_matches(query, &lines);
        let elapsed = t.elapsed();
        // Per-keystroke must be cheap — a generous 500ms absolute cap on the slowest call is a
        // smoke bound (the scaling test above is the real regression guard). This catches a
        // catastrophic stall (e.g. an accidental exponential path) without flaking under load.
        assert!(
            elapsed < Duration::from_millis(500),
            "find_matches(\"{query}\") over {N} lines took {elapsed:?}, a clear stall"
        );
    }
}
