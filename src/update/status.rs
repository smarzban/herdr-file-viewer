//! Pure presentation policy for the one-line remote-notice status.
//!
//! Callers supply the already-resolved binding labels, so this module never reads configuration or
//! the key registry. Spotlight text comes only from the policy's safe status-title accessor.

use super::NoticeSnapshot;

/// The canonical fallback label for an unbound remote-notice status hint.
///
/// This module owns status-hint normalization. Binding adapters must call
/// [`normalize_status_hint_label`] rather than duplicate this literal, while the formatter keeps
/// its own fallback defense for an empty effective key set.
pub const UNBOUND_KEY_LABEL: &str = "(unbound)";

/// Format the current remote notices as one status line.
///
/// A session dismissal hides every notice without changing the notice snapshot.
pub fn format_status(
    snapshot: &NoticeSnapshot,
    session_dismissed: bool,
    details_key: Option<&str>,
    dismiss_key: Option<&str>,
) -> Option<String> {
    if session_dismissed {
        return None;
    }

    let mut parts = Vec::new();
    if let Some(release) = snapshot.detected_release {
        parts.push(format!("Update v{release} available"));
    }
    if let Some(title) = snapshot.spotlight.status_title() {
        parts.push(format!("Spotlight: {title}"));
    }
    if parts.is_empty() {
        return None;
    }

    parts.push(format!(
        "{} details",
        normalize_status_hint_label(details_key)
    ));
    parts.push(format!(
        "{} dismiss",
        normalize_status_hint_label(dismiss_key)
    ));
    Some(parts.join(" · "))
}

/// Normalize a supplied effective binding label for a remote-notice status hint.
///
/// The caller normally supplies a non-empty label, but this fallback keeps the pure formatter
/// total when an effective key set is empty.
pub fn normalize_status_hint_label(label: Option<&str>) -> &str {
    label
        .filter(|label| !label.is_empty())
        .unwrap_or(UNBOUND_KEY_LABEL)
}

#[cfg(test)]
mod tests {
    use super::{UNBOUND_KEY_LABEL, format_status, normalize_status_hint_label};
    use crate::update::release_policy::CachedReleaseDetails;
    use crate::update::spotlight_policy::{SpotlightCache, SpotlightInput, cache_delta, project};
    use crate::update::{NoticeSnapshot, Version};

    fn snapshot(
        detected_release: Option<Version>,
        spotlight_title: Option<&str>,
    ) -> NoticeSnapshot {
        let mut spotlight = SpotlightCache::default();
        if let Some(title) = spotlight_title {
            spotlight.apply(cache_delta(
                project(SpotlightInput::Available(
                    format!("# {title}\nbody\n").into_bytes(),
                )),
                1,
            ));
        }
        NoticeSnapshot {
            detected_release,
            spotlight,
            ..NoticeSnapshot::default()
        }
    }

    #[test]
    fn formats_notice_status_table() {
        let update = Version {
            major: 2,
            minor: 0,
            patch: 0,
        };
        let cases = [
            (
                "none",
                snapshot(None, None),
                false,
                Some("?"),
                Some("u"),
                None,
            ),
            (
                "update",
                snapshot(Some(update), None),
                false,
                Some("?"),
                Some("u"),
                Some("Update v2.0.0 available · ? details · u dismiss"),
            ),
            (
                "spotlight",
                snapshot(
                    None,
                    Some("Project \x1b]8;;https://evil.invalid/\x1b\\Meteor"),
                ),
                false,
                Some("?"),
                Some("u"),
                Some("Spotlight: Project Meteor · ? details · u dismiss"),
            ),
            (
                "combined",
                snapshot(Some(update), Some("Project Meteor")),
                false,
                Some("?"),
                Some("u"),
                Some("Update v2.0.0 available · Spotlight: Project Meteor · ? details · u dismiss"),
            ),
            (
                "session dismissal hides every notice",
                snapshot(Some(update), Some("Project Meteor")),
                true,
                Some("?"),
                Some("u"),
                None,
            ),
            (
                "release details do not alter update status",
                NoticeSnapshot {
                    detected_release: Some(update),
                    release_details: Some(CachedReleaseDetails {
                        release: update,
                        details: "## [2.0.0]\n- details\n".into(),
                    }),
                    ..NoticeSnapshot::default()
                },
                false,
                Some("?"),
                Some("u"),
                Some("Update v2.0.0 available · ? details · u dismiss"),
            ),
            (
                "supplied effective labels",
                snapshot(Some(update), None),
                false,
                Some("F2"),
                Some("d"),
                Some("Update v2.0.0 available · F2 details · d dismiss"),
            ),
            (
                "unbound details",
                snapshot(Some(update), None),
                false,
                None,
                Some("u"),
                Some("Update v2.0.0 available · (unbound) details · u dismiss"),
            ),
            (
                "unbound dismiss",
                snapshot(Some(update), None),
                false,
                Some("?"),
                None,
                Some("Update v2.0.0 available · ? details · (unbound) dismiss"),
            ),
            (
                "empty effective labels normalize as unbound",
                snapshot(Some(update), None),
                false,
                Some(""),
                Some(""),
                Some("Update v2.0.0 available · (unbound) details · (unbound) dismiss"),
            ),
        ];

        for (name, snapshot, session_dismissed, details_key, dismiss_key, expected) in cases {
            let actual = format_status(&snapshot, session_dismissed, details_key, dismiss_key);
            assert_eq!(actual.as_deref(), expected, "{name}");
            assert!(
                !actual
                    .as_deref()
                    .is_some_and(|line| line.contains(['\r', '\n'])),
                "{name}: status must remain one line"
            );
            assert!(
                !actual.as_deref().is_some_and(|line| {
                    line.contains("herdr plugin install")
                        || line.contains(crate::update::repo_slug())
                        || line.contains("http")
                        || line.contains("install")
                }),
                "{name}: status must not contain install, slug, or URL copy"
            );
        }
    }

    #[test]
    fn normalizes_empty_status_hint_labels_to_the_canonical_label() {
        assert_eq!(normalize_status_hint_label(None), UNBOUND_KEY_LABEL);
        assert_eq!(normalize_status_hint_label(Some("")), UNBOUND_KEY_LABEL);
        assert_eq!(normalize_status_hint_label(Some("F2")), "F2");
    }
}
