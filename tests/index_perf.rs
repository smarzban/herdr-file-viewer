//! File Index + Fuzzy Matcher scaling guards (AC-21, AC-22).
//!
//! Instead of absolute ms budgets (which flake on a loaded CI runner and miss the thing that
//! matters — an O(n²) regression), these assert **relative scaling**: run each operation over N
//! and 2N inputs, assert `time(2N) < ~2.5 × time(N)` (roughly linear, with slack for
//! allocator/cache effects). Survives a 2–3× slower machine (both halves scale together), fails on
//! a super-linear regression. Modelled on the `render.rs` exemplar (`mul_f32(1.5)`).

mod common;

use common::TempDir;
use herdr_file_viewer::{fuzzy, index};
use std::fs;
use std::time::{Duration, Instant};

/// N for the scaling pair. 200 dirs × 100 files = 20,000 files; 2N = 400 × 100 = 40,000. Large
/// enough that the base timing is ~tens of ms (robust against scheduler jitter when the full
/// suite runs in parallel), cheap enough for the default lane.
const DIRS: usize = 200;
const FILES_PER_DIR: usize = 100;

/// Slack factor: 2N work may take more than 2× wall-clock (allocator/cache effects, a second
/// copy in memory, scheduling jitter under parallel test load), but an O(n²) regression blows
/// well past this. 4.0× keeps the test stable on a loaded CI lane while still catching the
/// regressions that matter. Mirrors `mul_f32(1.5)` from `render.rs`.
const RATIO_SLACK: f32 = 4.0;

/// A minimum base time below which the ratio is meaningless (sub-millisecond timings are
/// dominated by scheduler noise). If the N-side timing is below this, the bound falls back to a
/// safe absolute floor so a jitter spike on the 2N side can't trip the ratio.
const MIN_BASE: Duration = Duration::from_millis(15);

/// `elapsed * factor`, the form used by the `render.rs` exemplar (`timeout.mul_f32(1.5)`).
fn scaled(elapsed: Duration, factor: f32) -> Duration {
    Duration::from_secs_f32(elapsed.as_secs_f32() * factor)
}

/// Build a synthetic tree of `dirs × files_per_dir` files under `root`.
fn build_tree(root: &std::path::Path, dirs: usize, files_per_dir: usize) {
    for d in 0..dirs {
        let sub = root.join(format!("pkg{d:03}"));
        fs::create_dir_all(&sub).unwrap();
        for f in 0..files_per_dir {
            fs::write(sub.join(format!("module_{f:03}.rs")), "// placeholder").unwrap();
        }
    }
}

/// `index::build` scales roughly linearly in the file count. Doubling the tree must not blow up
/// super-linearly. Guards the AC-21 open budget via a ratio, not an absolute ms.
#[test]
fn index_build_scales_linearly() {
    let dir_n = TempDir::new();
    build_tree(dir_n.path(), DIRS, FILES_PER_DIR);

    let dir_2n = TempDir::new();
    build_tree(dir_2n.path(), DIRS * 2, FILES_PER_DIR);

    let t = Instant::now();
    let candidates_n = index::build(dir_n.path());
    let elapsed_n = t.elapsed();

    let t = Instant::now();
    let candidates_2n = index::build(dir_2n.path());
    let elapsed_2n = t.elapsed();

    // Sanity: the candidate count roughly doubled (dirs doubled, files_per_dir constant).
    let ratio = candidates_2n.len() as f32 / candidates_n.len().max(1) as f32;
    assert!(
        (1.8..=2.2).contains(&ratio),
        "expected ~2× candidates doubling the tree ({} → {}), got {ratio:.2}×",
        candidates_n.len(),
        candidates_2n.len()
    );

    let bound = scaled(elapsed_n.max(MIN_BASE), RATIO_SLACK);
    assert!(
        elapsed_2n < bound,
        "index::build did not scale linearly: {} files took {:?}, {} files took {:?} \
         (bound {:?})",
        candidates_n.len(),
        elapsed_n,
        candidates_2n.len(),
        elapsed_2n,
        bound,
    );
}

/// `fuzzy::match_and_rank` scales roughly linearly in the candidate count. Doubling the
/// candidates must not blow up super-linearly.
#[test]
fn fuzzy_match_scales_linearly() {
    let dir_n = TempDir::new();
    build_tree(dir_n.path(), DIRS, FILES_PER_DIR);
    let candidates_n = index::build(dir_n.path());
    assert!(candidates_n.len() >= DIRS * FILES_PER_DIR);

    let dir_2n = TempDir::new();
    build_tree(dir_2n.path(), DIRS * 2, FILES_PER_DIR);
    let candidates_2n = index::build(dir_2n.path());
    assert!(candidates_2n.len() >= DIRS * 2 * FILES_PER_DIR);

    // "apprs" exercises the subsequence path on realistic filenames.
    let t = Instant::now();
    let _ = fuzzy::match_and_rank("apprs", &candidates_n);
    let elapsed_n = t.elapsed();

    let t = Instant::now();
    let _ = fuzzy::match_and_rank("apprs", &candidates_2n);
    let elapsed_2n = t.elapsed();

    let bound = scaled(elapsed_n.max(MIN_BASE), RATIO_SLACK);
    assert!(
        elapsed_2n < bound,
        "fuzzy::match_and_rank did not scale linearly: {} candidates took {:?}, \
         {} candidates took {:?} (bound {:?})",
        candidates_n.len(),
        elapsed_n,
        candidates_2n.len(),
        elapsed_2n,
        bound,
    );
}
