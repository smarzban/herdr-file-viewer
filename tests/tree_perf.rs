//! T-8 — Tree Model perf: launch is interactive within 1s on a 10k-file repo (AC-22).
//! A guard that lazy enumeration (T-6) is preserved — eager enumeration would blow this.

mod common;

use common::TempDir;
use herdr_file_viewer::tree::TreeModel;
use std::fs;
use std::time::Instant;

/// AC-22: tree enumeration is interactive within 1s on a 10k-file repo.
/// Gated to the `perf` lane — an absolute budget on a shared CI runner flakes under
/// load; run via `cargo test --features perf`.
#[test]
#[cfg_attr(not(feature = "perf"), ignore)]
fn tree_is_interactive_within_one_second_at_10k_files() {
    let dir = TempDir::new();
    // 100 directories × 100 files = 10,000 files.
    for d in 0..100 {
        let sub = dir.path().join(format!("dir{d:03}"));
        fs::create_dir_all(&sub).unwrap();
        for f in 0..100 {
            fs::write(sub.join(format!("file{f:03}.txt")), "x").unwrap();
        }
    }

    let start = Instant::now();
    let model = TreeModel::new(dir.path());
    let nodes = model.visible_nodes();
    let elapsed = start.elapsed();

    assert!(nodes.len() >= 100, "top-level directories should be listed");
    assert!(
        elapsed.as_secs_f64() < 1.0,
        "AC-22: tree must be interactive within 1s, took {elapsed:?}"
    );
}
