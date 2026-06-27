//! Help content source — embedded changelog and about text for the in-app help overlay.
//!
//! This module is the **content source only**; it has no state, no I/O, and no side effects.
//! The overlay widget and section state live in later tasks (T-2 onward).

/// The full `CHANGELOG.md`, embedded at compile time (AC-12, AC-13).
pub const CHANGELOG_MD: &str = include_str!("../CHANGELOG.md");

/// The two sections of the help overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpSection {
    WhatsNew,
    About,
}

impl HelpSection {
    /// Human-readable tab label for the section.
    pub fn label(self) -> &'static str {
        match self {
            HelpSection::WhatsNew => "What's New",
            HelpSection::About => "About",
        }
    }
}

/// Assemble the "About" pane text.
///
/// Lines, in order:
/// 1. `herdr-file-viewer vX.Y.Z`
/// 2. package description
/// 3. `Repository: <url>`
/// 4. star CTA
/// 5. `License: <spdx>`
/// 6. update status (`Update available: vX.Y.Z` or `Up to date`)
///
/// (AC-16, AC-17, AC-18, AC-19)
pub fn about_text(update: Option<crate::update::version::Version>) -> String {
    let update_line = match update {
        Some(v) => format!("Update available: v{v}"),
        None => "Up to date".to_owned(),
    };
    format!(
        "{name} v{version}\n\
         {description}\n\
         Repository: {repository}\n\
         {star_cta}\n\
         License: {license}\n\
         {update_line}",
        name = env!("CARGO_PKG_NAME"),
        version = env!("CARGO_PKG_VERSION"),
        description = env!("CARGO_PKG_DESCRIPTION"),
        repository = env!("CARGO_PKG_REPOSITORY"),
        star_cta = "⭐️ Your star shines on us — star us on GitHub!",
        license = env!("CARGO_PKG_LICENSE"),
        update_line = update_line,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::version::Version;

    // (a) CHANGELOG_MD is non-empty and version headings are newest-first.
    #[test]
    fn changelog_is_embedded_and_newest_first() {
        assert!(!CHANGELOG_MD.is_empty(), "CHANGELOG_MD must not be empty");

        let idx_150 = CHANGELOG_MD
            .find("## [1.5.0]")
            .expect("CHANGELOG_MD must contain the 1.5.0 heading");
        let idx_140 = CHANGELOG_MD
            .find("## [1.4.0]")
            .expect("CHANGELOG_MD must contain the 1.4.0 heading");

        assert!(
            idx_150 < idx_140,
            "1.5.0 heading (byte {idx_150}) must appear before 1.4.0 heading (byte {idx_140})"
        );
    }

    // (b) about_text(None) contains the required identity fields.
    #[test]
    fn about_text_contains_identity_fields() {
        let text = about_text(None);
        assert!(
            text.contains(env!("CARGO_PKG_VERSION")),
            "about_text must contain the package version"
        );
        assert!(
            text.contains("herdr-file-viewer"),
            "about_text must contain the package name"
        );
        assert!(
            text.contains(env!("CARGO_PKG_REPOSITORY")),
            "about_text must contain the repository URL"
        );
        assert!(
            text.contains(env!("CARGO_PKG_LICENSE")),
            "about_text must contain the license"
        );
    }

    // (c) The star-CTA line is immediately before the License: line.
    #[test]
    fn star_cta_is_immediately_before_license_line() {
        let text = about_text(None);
        let lines: Vec<&str> = text.split('\n').collect();

        let cta_pos = lines
            .iter()
            .position(|l| l.contains("⭐️") && l.contains("star us on GitHub"))
            .expect("about_text must contain the star CTA line");
        let license_pos = lines
            .iter()
            .position(|l| l.starts_with("License:"))
            .expect("about_text must contain the License: line");

        assert_eq!(
            cta_pos + 1,
            license_pos,
            "star CTA (line {cta_pos}) must be exactly one before License: (line {license_pos})"
        );
    }

    // (d) Update status line reflects the argument.
    #[test]
    fn about_text_update_status() {
        let v = Version {
            major: 2,
            minor: 0,
            patch: 0,
        };
        let with_update = about_text(Some(v));
        assert!(
            with_update.contains("Update available"),
            "about_text(Some(_)) must contain 'Update available'"
        );
        assert!(
            with_update.contains("2.0.0"),
            "about_text(Some(v)) must contain the version string"
        );

        let up_to_date = about_text(None);
        assert!(
            up_to_date.contains("Up to date"),
            "about_text(None) must contain 'Up to date'"
        );
    }
}
