//! Docs definition-of-done checks for the copy-line-reference feature (T-10).
//!
//! Cheap, hermetic assertions that the user-facing docs actually carry the line-select surface:
//! the README `## Keys` table documents the `L` line-select key, and the CHANGELOG has a
//! `### Added` entry for it under the release that introduced it (`[1.9.0]`). These guard the
//! "docs match the feature in the same PR" rule so a future edit can't silently drop the key from
//! the front-door docs.

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
fn changelog_documents_line_reference_release() {
    // The feature shipped in `[1.9.0]`; that section is its permanent CHANGELOG home. Slice from
    // its heading to the next release heading so the check stays anchored to this release's block.
    let start = CHANGELOG
        .find("## [1.9.0]")
        .expect("CHANGELOG.md must carry the `## [1.9.0]` section that introduced line-select");
    let rest = &CHANGELOG[start + "## [1.9.0]".len()..];
    let end = rest.find("\n## [").unwrap_or(rest.len());
    let section = &rest[..end];
    assert!(
        section.contains("### Added"),
        "the `## [1.9.0]` section must have an `### Added` heading (Keep-a-Changelog)"
    );
    assert!(
        section.to_lowercase().contains("line reference")
            || section.to_lowercase().contains("line-select"),
        "the `## [1.9.0]` `### Added` block must document the copy-line-reference feature"
    );
}
