//! The CI workflow's Windows-job contract, asserted as text (mirrors tests/release_workflow.rs):
//! a `windows-latest` test job exists and is advisory/non-blocking (`continue-on-error: true`),
//! so a Windows-only flake can never block merge of an unrelated PR (AC-19, AC-N2). GitHub
//! Actions itself is exercised by the job actually running on `windows-latest` (a manual /
//! live-CI verification step — see the spec's honest-oracle note).

use std::fs;
use std::path::PathBuf;

fn workflow() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn declares_a_windows_latest_job() {
    assert!(
        workflow().contains("windows-latest"),
        "ci.yml must declare a windows-latest job"
    );
}

#[test]
fn the_windows_job_is_advisory_non_blocking() {
    // AC-19, AC-N2: continue-on-error: true on the Windows job specifically — not on the
    // required ubuntu/macOS `test` matrix.
    let w = workflow();
    let idx = w
        .find("windows-latest")
        .expect("windows-latest job must exist");
    // The continue-on-error directive must appear in the same job block as windows-latest —
    // checked by a tight window after the runs-on line rather than scanning the whole file
    // (the existing `test` matrix must NOT be advisory).
    let window = &w[idx..(idx + 400).min(w.len())];
    assert!(
        window.contains("continue-on-error: true"),
        "the windows-latest job must be continue-on-error: true (advisory): {window}"
    );
}

#[test]
fn the_required_test_matrix_does_not_include_windows() {
    // The required `test` job's OS matrix stays ubuntu/macOS only — Windows is a separate,
    // advisory job (AC-N2: it must not be able to block an unrelated PR via the required matrix).
    let w = workflow();
    let matrix_idx = w.find("os: [ubuntu-latest, macos-latest]").expect(
        "the required test job's OS matrix must list exactly ubuntu-latest and macos-latest",
    );
    let _ = matrix_idx; // presence is the assertion
}

#[test]
fn the_windows_job_runs_cargo_test() {
    let w = workflow();
    let idx = w
        .find("windows-latest")
        .expect("windows-latest job must exist");
    let window = &w[idx..(idx + 400).min(w.len())];
    assert!(
        window.contains("cargo test"),
        "the windows-latest job must run cargo test: {window}"
    );
}
