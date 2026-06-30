//! Content Renderer perf: the in-process portion (classify + ANSI ingest) of a
//! 1 MB file stays within the AC-23 300 ms responsiveness bound. The external renderer
//! runs off-thread; this guards the part that runs on the UI path.

mod common;

use common::TempDir;
use herdr_file_viewer::render::{Prepared, classify, to_text};
use std::fs;
use std::time::Instant;

/// AC-23: in-process classify+ingest of a 1 MB file stays within 300 ms.
/// Gated to the `perf` lane — an absolute budget on a shared CI runner flakes under
/// load; run via `cargo test --features perf`. The scaling tests stay on the default lane.
#[test]
#[cfg_attr(not(feature = "perf"), ignore)]
fn classify_and_ingest_one_megabyte_within_300ms() {
    let dir = TempDir::new();
    // ~1.1 MB of realistic text across many lines.
    let line = format!("{}\n", "x".repeat(200));
    let content = line.repeat(5400);
    let path = dir.path().join("big.txt");
    fs::write(&path, &content).unwrap();

    let start = Instant::now();
    let prepared = classify(dir.path(), &path);
    let text = match &prepared {
        Prepared::Full { text } | Prepared::Truncated { text, .. } => text.clone(),
        Prepared::Binary => String::new(),
    };
    let _ingested = to_text(&text);
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_millis() < 300,
        "AC-23: in-process classify+ingest must be < 300ms, took {elapsed:?}"
    );
}
