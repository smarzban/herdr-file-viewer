//! Docs definition-of-done checks for the copy-line-reference feature (T-10).
//!
//! Cheap, hermetic assertions that the user-facing docs actually carry the line-select surface:
//! the README `## Keys` table documents the `L` line-select key, and the CHANGELOG has an
//! `## [Unreleased]` / `### Added` entry for it. These guard the "docs match the feature in the
//! same PR" rule so a future edit can't silently drop the key from the front-door docs.

const README: &str = include_str!("../README.md");
const CHANGELOG: &str = include_str!("../CHANGELOG.md");

#[test]
fn readme_documents_line_select_key() {
    assert!(
        README.contains("line-select"),
        "README.md must document the `L` line-select mode"
    );
    assert!(
        README.contains("`L`"),
        "README.md must mention the `L` key for line-select"
    );
}

#[test]
fn changelog_has_unreleased_line_reference_entry() {
    let idx = CHANGELOG
        .find("## [Unreleased]")
        .expect("CHANGELOG.md must carry an `## [Unreleased]` section for the pending release");
    let unreleased = &CHANGELOG[idx..];
    assert!(
        unreleased.contains("### Added"),
        "the `## [Unreleased]` section must have an `### Added` heading (Keep-a-Changelog)"
    );
    assert!(
        unreleased.to_lowercase().contains("line reference")
            || unreleased.to_lowercase().contains("line-select"),
        "the `## [Unreleased]` `### Added` block must document the copy-line-reference feature"
    );
}
