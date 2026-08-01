//! What's New composition: independent documents under one Help-open deadline.

use herdr_file_viewer::help::released_changelog;
use herdr_file_viewer::render::to_text;
use herdr_file_viewer::update::compose::{
    MarkdownSectionRenderer, WHATS_NEW_COMPOSE_TIMEOUT, compose_whats_new, install_guidance,
};
use herdr_file_viewer::update::release_policy::{CachedReleaseDetails, eligible_release_sections};
use herdr_file_viewer::update::spotlight_policy::{
    SpotlightCache, SpotlightInput, cache_delta, project,
};
use herdr_file_viewer::update::{NoticeSnapshot, Version};
use ratatui::text::Text;
use std::time::{Duration, Instant};

const EMBEDDED: &str = "## [1.4.0]\n- embedded new\n\n## [1.3.0]\n- embedded old\n";
const REMOTE: &str = "## [9.2.0]\n- remote newer\n\n## [9.1.0]\n- remote older\n";
const SPOTLIGHT: &str = "Spotlight body\n";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Call {
    document: String,
    width: u16,
    remaining: Duration,
}

#[derive(Default)]
struct RecordingRenderer {
    calls: Vec<Call>,
    delay_each: Option<Duration>,
}

impl MarkdownSectionRenderer for RecordingRenderer {
    fn render(&mut self, document: &str, width: u16, remaining: Duration) -> Text<'static> {
        self.calls.push(Call {
            document: document.to_owned(),
            width,
            remaining,
        });
        if let Some(delay) = self.delay_each {
            std::thread::sleep(delay);
        }
        to_text(document)
    }
}

fn version() -> Version {
    Version {
        major: 9,
        minor: 2,
        patch: 0,
    }
}

fn snapshot(remote: bool, spotlight: bool, install: bool) -> NoticeSnapshot {
    let mut cached_spotlight = SpotlightCache::default();
    if spotlight {
        cached_spotlight.apply(cache_delta(
            project(SpotlightInput::Available(
                format!("# Project Spotlight\n{SPOTLIGHT}").into_bytes(),
            )),
            1,
        ));
    }
    NoticeSnapshot {
        detected_release: install.then(version),
        release_details: remote.then(|| CachedReleaseDetails {
            release: version(),
            details: REMOTE.to_owned(),
        }),
        spotlight: cached_spotlight,
    }
}

fn flatten(text: &Text<'_>) -> String {
    text.lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn remote_and_embedded_history_share_the_strict_release_section_matrix() {
    let changelog = concat!(
        "# Changelog\r\n\r\n",
        "## [Unreleased]\r\n",
        "- pending\r\n\r\n",
        "## [2.0.0-rc.1]\n",
        "- prerelease\n\n",
        "## [2.0.0+build.7]\n",
        "- build metadata\n\n",
        "## [v2.0.0]\n",
        "- tag spelling is not a changelog heading\n\n",
        "## [2.0.0] malformed suffix\n",
        "- malformed suffix\n\n",
        "## [2.0.0] - stable detail\r\n",
        "- source order second\r\n\r\n",
        "## [1.0.0]\n",
        "- first duplicate\n\n",
        "## [3.0.0] - newest\n",
        "- source order third\n\n",
        "## [1.0.0] - duplicate\r\n",
        "- source order fourth\r\n\r\n",
        "[1.0.0]: https://example.test/compare\r\n"
    );
    let stable_2 = "## [2.0.0] - stable detail\r\n- source order second\r\n\r\n";
    let first_1 = "## [1.0.0]\n- first duplicate\n\n";
    let stable_3 = "## [3.0.0] - newest\n- source order third\n\n";
    let final_1 = concat!(
        "## [1.0.0] - duplicate\r\n",
        "- source order fourth\r\n\r\n",
        "[1.0.0]: https://example.test/compare\r\n"
    );

    assert_eq!(
        released_changelog(changelog),
        format!("{stable_2}{first_1}{stable_3}{final_1}"),
        "embedded history accepts exactly stable headings in source order and retains final-section references"
    );
    assert_eq!(
        eligible_release_sections(
            changelog,
            Version {
                major: 0,
                minor: 0,
                patch: 0,
            },
            Version {
                major: 3,
                minor: 0,
                patch: 0,
            },
        ),
        vec![stable_3, stable_2, first_1, final_1],
        "remote details use the same acceptance rules, preserve source slices, and sort accepted releases newest first"
    );
}

#[test]
fn content_subset_matrix_keeps_three_documents_ordered_and_locally_separated() {
    // The only independent documents are spotlight, Available updates, and embedded history.
    // Details without a detected release are impossible policy leftovers and must not make an
    // update document on their own.
    for details in [false, true] {
        for spotlight in [false, true] {
            for release in [false, true] {
                let mut renderer = RecordingRenderer::default();
                let install_copy = install_guidance();
                let body = compose_whats_new(
                    &snapshot(details, spotlight, release),
                    EMBEDDED,
                    &install_copy,
                    Instant::now(),
                    71,
                    &mut renderer,
                );
                let available_updates = release.then(|| {
                    if details {
                        format!("{REMOTE}\n{install_copy}")
                    } else {
                        install_copy.clone()
                    }
                });
                let mut expected = Vec::new();
                if spotlight {
                    expected.push(SPOTLIGHT.to_owned());
                }
                if let Some(updates) = available_updates {
                    expected.push(updates);
                }
                expected.push(EMBEDDED.to_owned());

                assert_eq!(
                    renderer
                        .calls
                        .iter()
                        .map(|call| call.document.as_str())
                        .collect::<Vec<_>>(),
                    expected.iter().map(String::as_str).collect::<Vec<_>>(),
                    "details={details}, spotlight={spotlight}, release={release}: spotlight, Available updates, and embedded history are the only ordered documents"
                );
                assert!(
                    renderer.calls.len() <= 3 && renderer.calls.iter().all(|call| call.width == 71),
                    "details={details}, spotlight={spotlight}, release={release}: at most three documents receive the Help width"
                );
                let expected_body = expected
                    .iter()
                    .map(|document| document.trim_end_matches('\n'))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                assert_eq!(
                    flatten(&body),
                    expected_body,
                    "details={details}, spotlight={spotlight}, release={release}: safe local blank-line separators join independently safe renders"
                );
            }
        }
    }
}

#[test]
fn remote_details_stay_exact_while_embedded_unreleased_is_excluded() {
    let embedded = concat!(
        "# Changelog\n\n",
        "## [Unreleased]\n- never display this\n\n",
        "## [1.4.0]\n- embedded new\n\n",
        "## [1.3.0]\n- embedded old\n"
    );
    let mut renderer = RecordingRenderer::default();

    let body = compose_whats_new(
        &snapshot(true, false, true),
        embedded,
        &install_guidance(),
        Instant::now(),
        71,
        &mut renderer,
    );

    assert_eq!(
        renderer.calls[0].document,
        format!("{REMOTE}\n{}", install_guidance()),
        "T-1 details remain byte-exact within the one Available updates document"
    );
    assert_eq!(
        renderer.calls[1].document, EMBEDDED,
        "the embedded renderer document retains every released section, newest first"
    );
    let shown = flatten(&body);
    assert!(!shown.contains("Unreleased") && !shown.contains("never display this"));
    assert!(shown.find("embedded new") < shown.find("embedded old"));
}

#[test]
fn embedded_only_renders_every_embedded_released_section() {
    let embedded = concat!(
        "## [Unreleased]\n- pending\n\n",
        "## [3.0.0]\n- newest\n\n",
        "## [2.0.0]\n- middle\n\n",
        "## [1.0.0]\n- oldest\n"
    );
    let mut renderer = RecordingRenderer::default();

    let body = compose_whats_new(
        &snapshot(false, false, false),
        embedded,
        &install_guidance(),
        Instant::now(),
        71,
        &mut renderer,
    );

    assert_eq!(
        renderer.calls.len(),
        1,
        "only the embedded document is present"
    );
    assert_eq!(
        renderer.calls[0].document,
        "## [3.0.0]\n- newest\n\n## [2.0.0]\n- middle\n\n## [1.0.0]\n- oldest\n"
    );
    assert_eq!(
        flatten(&body),
        renderer.calls[0].document.trim_end_matches('\n')
    );
}

#[test]
fn detected_release_with_details_adds_local_install_guidance() {
    let mut renderer = RecordingRenderer::default();

    let body = compose_whats_new(
        &snapshot(true, false, true),
        EMBEDDED,
        &install_guidance(),
        Instant::now(),
        71,
        &mut renderer,
    );

    assert_eq!(
        renderer.calls.len(),
        2,
        "details and install render once together"
    );
    assert_eq!(
        renderer.calls[0].document,
        format!("{REMOTE}\n{}", install_guidance())
    );
    assert!(flatten(&body).contains("herdr plugin install smarzban/herdr-file-viewer"));
}

#[test]
fn detected_release_without_details_still_adds_local_install_guidance() {
    let mut renderer = RecordingRenderer::default();

    let body = compose_whats_new(
        &snapshot(false, false, true),
        EMBEDDED,
        &install_guidance(),
        Instant::now(),
        71,
        &mut renderer,
    );

    assert_eq!(
        renderer
            .calls
            .iter()
            .map(|call| call.document.as_str())
            .collect::<Vec<_>>(),
        vec![install_guidance().as_str(), EMBEDDED]
    );
    assert_eq!(
        renderer.calls.len(),
        2,
        "install guidance is one Available updates document"
    );
    assert!(flatten(&body).contains("herdr plugin install smarzban/herdr-file-viewer"));
}

#[test]
fn accepted_spotlight_remains_in_whats_new() {
    let mut renderer = RecordingRenderer::default();

    let body = compose_whats_new(
        &snapshot(false, true, false),
        EMBEDDED,
        &install_guidance(),
        Instant::now(),
        71,
        &mut renderer,
    );

    assert_eq!(
        renderer
            .calls
            .iter()
            .map(|call| call.document.as_str())
            .collect::<Vec<_>>(),
        vec![SPOTLIGHT, EMBEDDED],
        "an accepted spotlight remains in What's New independently of status-row dismissal"
    );
    assert!(flatten(&body).contains(SPOTLIGHT));
}

#[test]
fn absolute_budget_passes_decreasing_remaining_to_each_document() {
    let mut renderer = RecordingRenderer {
        delay_each: Some(Duration::from_millis(1)),
        ..RecordingRenderer::default()
    };

    let _ = compose_whats_new(
        &snapshot(true, true, true),
        EMBEDDED,
        &install_guidance(),
        Instant::now(),
        71,
        &mut renderer,
    );

    assert_eq!(
        renderer.calls.len(),
        3,
        "only three logical documents may delegate"
    );
    assert!(
        renderer
            .calls
            .iter()
            .all(|call| call.remaining <= WHATS_NEW_COMPOSE_TIMEOUT && !call.remaining.is_zero()),
        "every delegated document receives the current remainder of the one 200 ms Help-open budget"
    );
    assert!(
        renderer
            .calls
            .windows(2)
            .all(|pair| pair[0].remaining > pair[1].remaining),
        "the composer must remeasure the same absolute deadline, never reset a per-document budget"
    );
}

#[test]
fn already_expired_open_skips_delegation_and_keeps_neighboring_documents() {
    let mut renderer = RecordingRenderer::default();
    let install_copy = install_guidance();

    let body = compose_whats_new(
        &snapshot(true, true, true),
        EMBEDDED,
        &install_copy,
        Instant::now() - WHATS_NEW_COMPOSE_TIMEOUT,
        71,
        &mut renderer,
    );

    assert!(
        renderer.calls.is_empty(),
        "an already-expired Help-open deadline falls back without delegation"
    );
    let shown = flatten(&body);
    for document in [SPOTLIGHT, REMOTE, install_copy.as_str(), EMBEDDED] {
        assert!(
            shown.contains(document.trim_end_matches('\n')),
            "a timed-out neighboring render cannot drop this logical document: {document:?}"
        );
    }
}
