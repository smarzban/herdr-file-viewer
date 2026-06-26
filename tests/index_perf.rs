//! T-11 — File Index + Fuzzy Matcher perf guards (AC-21, AC-22).
//!
//! Builds a synthetic tree of ~10,000 files ONCE (outside the timed region), then:
//!   • asserts `index::build` completes within the AC-21 1 s open budget, and
//!   • asserts `fuzzy::match_and_rank` over ~10,000 candidates completes within
//!     the AC-22 300 ms per-keystroke budget.
//!
//! These are generous guardrails, not microbenchmarks.

mod common;

use common::TempDir;
use herdr_file_viewer::{fuzzy, index};
use std::fs;
use std::time::{Duration, Instant};

/// 100 directories × 100 files = 10,000 files total.
const DIRS: usize = 100;
const FILES_PER_DIR: usize = 100;

#[test]
fn index_build_within_one_second_at_10k_files() {
    // ── Setup (outside the timed region) ──────────────────────────────────────
    let dir = TempDir::new();
    for d in 0..DIRS {
        let sub = dir.path().join(format!("pkg{d:03}"));
        fs::create_dir_all(&sub).unwrap();
        for f in 0..FILES_PER_DIR {
            fs::write(sub.join(format!("module_{f:03}.rs")), "// placeholder").unwrap();
        }
    }

    // ── Timed region (AC-21) ──────────────────────────────────────────────────
    let t = Instant::now();
    let candidates = index::build(dir.path());
    let d = t.elapsed();

    assert!(
        candidates.len() >= DIRS * FILES_PER_DIR,
        "expected at least {} files, got {}",
        DIRS * FILES_PER_DIR,
        candidates.len()
    );
    assert!(
        d < Duration::from_secs(1),
        "AC-21: index::build over 10k files took {d:?}, exceeds 1 s budget"
    );
}

#[test]
fn fuzzy_match_within_300ms_over_10k_candidates() {
    // ── Setup (outside the timed region) ──────────────────────────────────────
    let dir = TempDir::new();
    for d in 0..DIRS {
        let sub = dir.path().join(format!("pkg{d:03}"));
        fs::create_dir_all(&sub).unwrap();
        for f in 0..FILES_PER_DIR {
            fs::write(sub.join(format!("module_{f:03}.rs")), "// placeholder").unwrap();
        }
    }
    let candidates = index::build(dir.path());
    assert!(candidates.len() >= DIRS * FILES_PER_DIR);

    // ── Timed region (AC-22) ──────────────────────────────────────────────────
    // "apprs" is a realistic short query that exercises the subsequence path.
    let t = Instant::now();
    let _ = fuzzy::match_and_rank("apprs", &candidates);
    let d = t.elapsed();

    assert!(
        d < Duration::from_millis(300),
        "AC-22: fuzzy::match_and_rank over 10k candidates took {d:?}, exceeds 300 ms budget"
    );
}
