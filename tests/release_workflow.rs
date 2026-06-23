//! The release workflow's contract, asserted as text (mirrors tests/manifest.rs): it triggers on
//! version tags, builds the three published targets, guards the tag against the crate version, and
//! publishes a SHA256SUMS. These are the invariants scripts/fetch-or-build.sh relies on; GitHub
//! Actions itself is exercised by cutting a real tag (a manual verification step).

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
fn builds_the_three_published_targets() {
    let w = workflow();
    for triple in [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-musl",
    ] {
        assert!(w.contains(triple), "release must build {triple}");
    }
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
    // The install gate compares the checkout's HEAD to this marker (herdr's checkout lacks tags),
    // so the release must publish the built commit as a COMMIT asset.
    assert!(
        w.contains("release/COMMIT") && w.contains("GITHUB_SHA"),
        "release must publish a COMMIT marker (the GITHUB_SHA it was built from)"
    );
}
