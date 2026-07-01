//! The release workflow's contract, asserted as text (mirrors tests/manifest.rs): it triggers on
//! version tags, builds the four published targets (incl. x86_64-pc-windows-msvc, preview —
//! T-10), guards the tag against the crate version, and publishes a SHA256SUMS. These are the
//! invariants scripts/fetch-or-build.sh and fetch-or-build.ps1 rely on; GitHub Actions itself is
//! exercised by cutting a real tag (a manual verification step).

use std::fs;
use std::path::PathBuf;

fn workflow() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn triggers_on_version_tags() {
    let w = workflow();
    assert!(w.contains("tags:"), "release must be tag-triggered");
    assert!(w.contains("v*"), "release must trigger on v* tags");
}

#[test]
fn builds_the_four_published_targets() {
    let w = workflow();
    for triple in [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-musl",
        "x86_64-pc-windows-msvc",
    ] {
        assert!(w.contains(triple), "release must build {triple}");
    }
}

#[test]
fn publishes_the_windows_exe_asset() {
    // AC-18: the Windows target publishes a herdr-file-viewer-x86_64-pc-windows-msvc.exe asset.
    assert!(
        workflow().contains("herdr-file-viewer-$triple.exe"),
        "release must stage the Windows asset with its .exe suffix"
    );
}

#[test]
fn does_not_declare_an_aarch64_windows_target() {
    // AC-N4: v1 targets x86_64-pc-windows-msvc only — no Windows-on-ARM in the matrix.
    assert!(
        !workflow().contains("aarch64-pc-windows"),
        "release must not declare an aarch64 Windows target"
    );
}

#[test]
fn publishes_a_sha256sums_file() {
    assert!(
        workflow().contains("SHA256SUMS"),
        "release must publish a SHA256SUMS for the binaries"
    );
}

#[test]
fn guards_the_tag_against_the_crate_version() {
    assert!(
        workflow().contains("Cargo.toml"),
        "release must verify the pushed tag matches Cargo.toml's version"
    );
}

#[test]
fn publishes_a_commit_marker() {
    let w = workflow();
    // The install script reads this marker to note when a checkout is ahead of the release it's
    // installing (herdr's checkout lacks tags), so the release must publish the built commit.
    assert!(
        w.contains("release/COMMIT") && w.contains("GITHUB_SHA"),
        "release must publish a COMMIT marker (the GITHUB_SHA it was built from)"
    );
}
