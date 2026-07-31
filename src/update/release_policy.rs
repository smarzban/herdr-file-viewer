//! Pure policy for selecting and retaining changelog release details.

use super::Version;

/// Return the exact source slices for releases newer than `installed` through `detected`, sorted
/// newest first. A section begins at a valid `## [MAJOR.MINOR.PATCH]` heading and ends at the next
/// level-two heading, valid or not, so malformed headings never leak into an accepted section.
///
/// This is deliberately a narrow changelog parser, not a Markdown parser. The returned slices
/// borrow `changelog`, preserving every accepted byte for the downstream renderer/cache.
pub fn eligible_release_sections(
    changelog: &str,
    installed: Version,
    detected: Version,
) -> Vec<&str> {
    let mut sections = Vec::new();
    let mut current = None;
    let mut offset = 0;

    for line in changelog.split_inclusive('\n') {
        if is_level_two_heading(line) {
            if let Some((version, start)) = current.take() {
                sections.push(ReleaseSection {
                    version,
                    text: &changelog[start..offset],
                });
            }
            current = release_heading_version(line).map(|version| (version, offset));
        }
        offset += line.len();
    }

    if let Some((version, start)) = current {
        sections.push(ReleaseSection {
            version,
            text: &changelog[start..],
        });
    }

    sections.retain(|section| section.version > installed && section.version <= detected);
    sections.sort_by_key(|section| std::cmp::Reverse(section.version));
    sections.into_iter().map(|section| section.text).collect()
}

/// Immutable details that were fetched for one exact detected release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedReleaseDetails {
    pub release: Version,
    pub details: String,
}

/// The only valid ways a caller may use an in-memory release-details cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachedReleaseDetailsDecision<'a> {
    /// No newer release exists for this running build, so no details may be shown.
    Hidden,
    /// The cache was fetched for the exact detected release and remains valid on source failure.
    Cached(&'a CachedReleaseDetails),
    /// The detected release has no matching cached details and must be fetched before display.
    Fetch(Version),
}

/// Decide whether immutable cached details can still describe the detected release.
///
/// A detected release is intentionally supplied independently of its details: a valid version
/// remains useful even when its changelog source is missing or malformed. Cache reuse requires an
/// exact release match, preventing old text from being displayed for a newer release.
pub fn cached_release_details(
    installed: Version,
    detected: Option<Version>,
    cached: Option<&CachedReleaseDetails>,
) -> CachedReleaseDetailsDecision<'_> {
    let Some(detected) = detected else {
        return CachedReleaseDetailsDecision::Hidden;
    };
    if detected <= installed {
        return CachedReleaseDetailsDecision::Hidden;
    }
    match cached.filter(|details| details.release == detected) {
        Some(details) => CachedReleaseDetailsDecision::Cached(details),
        None => CachedReleaseDetailsDecision::Fetch(detected),
    }
}

#[derive(Debug)]
struct ReleaseSection<'a> {
    version: Version,
    text: &'a str,
}

fn is_level_two_heading(line: &str) -> bool {
    line.starts_with("## ")
}

fn release_heading_version(line: &str) -> Option<Version> {
    let heading = line.trim_end_matches(['\r', '\n']);
    let rest = heading.strip_prefix("## [")?;
    let (version, suffix) = rest.split_once(']')?;
    (suffix.is_empty() || suffix.starts_with(" - ")).then(|| Version::parse(version))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::Version;

    fn version(text: &str) -> Version {
        Version::parse(text).expect("test version is an exact triple")
    }

    #[test]
    fn eligible_release_sections_selects_sorts_and_preserves_exact_bytes() {
        // The release sections are deliberately not source-ordered. The accepted sections use
        // mixed line endings to prove this narrow parser returns the original byte slices rather
        // than reconstructed Markdown.
        let changelog = concat!(
            "# Changelog\n\n",
            "## [1.9.0] - 2026-07-01\r\n",
            "### Added\r\n",
            "- earlier\r\n",
            "\r\n",
            "## [Unreleased]\n",
            "- not a release\n\n",
            "## [not-a-version]\n",
            "- malformed headings delimit a section but are not releases\n\n",
            "## [1.10.0] - 2026-07-02\n",
            "### Fixed\n",
            "- current release\n\n",
            "## [1.11.0] - 2026-07-03\n",
            "- newer than the detected release\n"
        );
        let expected_current = concat!(
            "## [1.10.0] - 2026-07-02\n",
            "### Fixed\n",
            "- current release\n\n"
        );
        let expected_earlier = concat!(
            "## [1.9.0] - 2026-07-01\r\n",
            "### Added\r\n",
            "- earlier\r\n",
            "\r\n"
        );

        assert_eq!(
            eligible_release_sections(changelog, version("1.8.0"), version("1.10.0")),
            vec![expected_current, expected_earlier],
            "only installed < release <= detected sections appear, in numeric descending order"
        );
    }

    #[test]
    fn eligible_release_sections_preserves_file_final_section_bytes() {
        let changelog = "## [1.9.0] - 2026-07-01\r\n- final section\r\n";

        assert_eq!(
            eligible_release_sections(changelog, version("1.8.0"), version("1.9.0")),
            vec![changelog],
            "the EOF flush returns the final eligible section verbatim"
        );
    }

    #[test]
    fn cached_release_details_follow_exact_release_lifetime() {
        let installed = version("1.9.0");
        let detected = version("1.10.0");
        let cached = CachedReleaseDetails {
            release: detected,
            details: "## [1.10.0]\n- immutable cached details\n".into(),
        };

        // A same-release detail-source failure falls back to the existing immutable details.
        assert_eq!(
            cached_release_details(installed, Some(detected), Some(&cached)),
            CachedReleaseDetailsDecision::Cached(&cached)
        );
        // A running version caught up to that release must hide the cached details.
        assert_eq!(
            cached_release_details(detected, Some(detected), Some(&cached)),
            CachedReleaseDetailsDecision::Hidden
        );
        // A later detected release must not present old details as if they described it.
        assert_eq!(
            cached_release_details(installed, Some(version("1.11.0")), Some(&cached)),
            CachedReleaseDetailsDecision::Fetch(version("1.11.0"))
        );
    }
}
