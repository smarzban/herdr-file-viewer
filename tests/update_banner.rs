//! The update-available banner state on the Session Controller (AC-U1, AC-U2, AC-U7): shown
//! from the initial (cached) value, refreshed from the background channel, and dismissable for
//! the session. No real git / renderer / editor / network — the components are no-op stubs and
//! the update result is injected directly.

mod common;

use common::TempDir;
use herdr_file_viewer::controller::{
    Components, ContentProvider, Controller, EditorHandoff, EditorOutcome, GitService,
    RenderResult, RootProviders,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::update::spotlight_policy::{
    SpotlightCache, SpotlightInput, cache_delta, project,
};
use herdr_file_viewer::update::{NoticeSnapshot, UpdateState, Version};
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::text::Text;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, mpsc};

// ---- minimal no-op stubs (the banner logic exercises none of these) -------------------

struct Git;
impl GitService for Git {
    fn status(&self) -> BTreeMap<std::path::PathBuf, Status> {
        BTreeMap::new()
    }
    fn changed_set(&self, _baseline: Baseline) -> BTreeMap<std::path::PathBuf, Status> {
        BTreeMap::new()
    }
    fn diff(&self, _rel: &Path, _baseline: Baseline, _full: bool) -> String {
        String::new()
    }
    fn diff_directory(&self, _rel_dir: &Path, _baseline: Baseline) -> String {
        String::new()
    }
}

struct Content;
impl ContentProvider for Content {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        RenderResult {
            content: Text::raw(""),
            notices: Vec::new(),
            source: None,
        }
    }
}

struct Editor;
impl EditorHandoff for Editor {
    fn open(&mut self, _file: &Path) -> EditorOutcome {
        EditorOutcome::NoTakeover
    }
}

fn controller_in(dir: &Path) -> Controller {
    Controller::new(
        // non-git: keeps the test focused on banner state
        common::resolved(dir.to_path_buf(), false),
        Baseline::Head,
        Components {
            providers: Box::new(move |_resolved| RootProviders {
                git: Arc::new(Git),
                content: Box::new(Content),
            }),
            editor: Box::new(Editor),
            clipboard: Box::new(common::RecordingClipboard::default()),
            renderers: None,
        },
    )
}

fn v(major: u32, minor: u32, patch: u32) -> Version {
    Version {
        major,
        minor,
        patch,
    }
}

fn snapshot(detected_release: Option<Version>, spotlight_title: Option<&str>) -> NoticeSnapshot {
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

fn snapshot_from_spotlight_input(input: SpotlightInput) -> NoticeSnapshot {
    let mut snapshot = NoticeSnapshot::default();
    snapshot.spotlight.apply(cache_delta(project(input), 1));
    snapshot
}

#[test]
fn initial_cached_version_shows_a_banner() {
    let dir = TempDir::new();
    let mut c = controller_in(dir.path());
    c.set_update(UpdateState {
        initial: snapshot(Some(v(9, 9, 9)), None),
        rx: None,
    });
    assert!(
        c.view_state()
            .update_banner
            .is_some_and(|b| b.contains("9.9.9")),
        "a cached newer version is advertised on the first frame"
    );
}

#[test]
fn no_update_means_no_banner() {
    let dir = TempDir::new();
    let mut c = controller_in(dir.path());
    c.set_update(UpdateState {
        initial: snapshot(None, None),
        rx: None,
    });
    assert!(c.view_state().update_banner.is_none());
}

#[test]
fn background_result_turns_the_banner_on() {
    let dir = TempDir::new();
    let (tx, rx) = mpsc::channel();
    let mut c = controller_in(dir.path());
    c.set_update(UpdateState {
        initial: snapshot(None, None),
        rx: Some(rx),
    });
    assert!(
        c.view_state().update_banner.is_none(),
        "nothing until the check returns"
    );

    tx.send(snapshot(Some(v(2, 0, 0)), None)).unwrap();
    let fx = c.poll().expect("poll applies the background update result");
    assert!(fx.redraw, "a fresh verdict triggers a repaint");
    assert!(
        c.view_state()
            .update_banner
            .is_some_and(|b| b.contains("2.0.0")),
        "the banner now names the version the check found"
    );
}

#[test]
fn background_up_to_date_clears_a_stale_cached_banner() {
    // A cached banner, then a successful check that finds nothing newer (`None`) → banner gone.
    let dir = TempDir::new();
    let (tx, rx) = mpsc::channel();
    let mut c = controller_in(dir.path());
    c.set_update(UpdateState {
        initial: snapshot(Some(v(9, 9, 9)), None),
        rx: Some(rx),
    });
    assert!(c.view_state().update_banner.is_some());

    tx.send(snapshot(None, None)).unwrap();
    c.poll().expect("poll applies the result");
    assert!(
        c.view_state().update_banner.is_none(),
        "a successful 'up-to-date' check clears the stale cached banner"
    );
}

#[test]
fn background_notice_snapshot_replaces_release_and_spotlight_together() {
    let dir = TempDir::new();
    let (tx, rx) = mpsc::channel();
    let mut c = controller_in(dir.path());
    c.set_update(UpdateState {
        initial: snapshot(Some(v(9, 9, 9)), Some("Before")),
        rx: Some(rx),
    });

    tx.send(snapshot(Some(v(2, 0, 0)), Some("After"))).unwrap();
    c.poll().expect("the completed snapshot redraws");

    assert!(
        c.view_state()
            .update_banner
            .is_some_and(|banner| banner.contains("2.0.0")),
        "the new snapshot replaces the detected release"
    );
    assert_eq!(
        c.notice_snapshot().spotlight.status_title(),
        Some("After"),
        "the same channel message replaces the spotlight, not only the release"
    );
}

#[test]
fn disconnected_notice_channel_preserves_last_snapshot() {
    let dir = TempDir::new();
    let (tx, rx) = mpsc::channel();
    let mut c = controller_in(dir.path());
    c.set_update(UpdateState {
        initial: snapshot(Some(v(9, 9, 9)), Some("Cached")),
        rx: Some(rx),
    });

    drop(tx);
    c.poll();

    assert!(
        c.view_state()
            .update_banner
            .is_some_and(|banner| banner.contains("9.9.9")),
        "a disconnected probe keeps the last detected release"
    );
    assert_eq!(
        c.notice_snapshot().spotlight.status_title(),
        Some("Cached"),
        "a disconnected probe keeps the last spotlight in the same snapshot"
    );
}

#[test]
fn dismiss_hides_the_banner_for_the_session() {
    let dir = TempDir::new();
    let mut c = controller_in(dir.path());
    c.set_update(UpdateState {
        initial: snapshot(Some(v(9, 9, 9)), Some("Project")),
        rx: None,
    });
    let fx = c.handle(Intent::DismissUpdate);
    assert!(fx.redraw, "dismissing repaints to remove the banner");
    assert!(
        c.view_state().update_banner.is_none(),
        "session dismissal hides every notice form at once"
    );
    // Dismiss again is inert (no banner to hide).
    assert!(!c.handle(Intent::DismissUpdate).redraw);
}

#[test]
fn controller_projects_typed_notice_status_forms_with_default_labels() {
    let update = v(9, 9, 9);
    let mut remembered_spotlight = snapshot(Some(update), Some("Project"));
    remembered_spotlight.spotlight.dismiss();
    let cases = [
        (
            "update",
            snapshot(Some(update), None),
            Some("Update v9.9.9 available · ? details · u dismiss"),
        ),
        (
            "accepted spotlight safe title",
            snapshot(
                None,
                Some("Project \x1b]8;;https://evil.invalid/\x1b\\Meteor"),
            ),
            Some("Spotlight: Project Meteor · ? details · u dismiss"),
        ),
        (
            "combined",
            snapshot(Some(update), Some("Project")),
            Some("Update v9.9.9 available · Spotlight: Project · ? details · u dismiss"),
        ),
        (
            "remembered spotlight dismissal leaves update",
            remembered_spotlight,
            Some("Update v9.9.9 available · ? details · u dismiss"),
        ),
        ("empty", snapshot(None, None), None),
    ];

    for (name, initial, expected) in cases {
        let dir = TempDir::new();
        let mut controller = controller_in(dir.path());
        controller.set_update(UpdateState { initial, rx: None });
        let actual = controller.view_state().update_banner;
        assert_eq!(actual.as_deref(), expected, "{name}");
        assert!(
            !actual.as_deref().is_some_and(
                |line| line.contains("herdr plugin install") || line.contains("install")
            ),
            "{name}: status must not advertise an install or automatic action: {actual:?}"
        );
    }
}

#[test]
fn disabled_and_invalid_spotlights_project_no_status_line() {
    let invalid = [
        ("missing", SpotlightInput::Missing),
        ("empty", SpotlightInput::Available(Vec::new())),
        ("blank", SpotlightInput::Available(b" \r\n\t".to_vec())),
        (
            "non-utf8",
            SpotlightInput::Available(vec![b'#', b' ', 0xff]),
        ),
        (
            "headingless",
            SpotlightInput::Available(b"ordinary body without a heading\n".to_vec()),
        ),
        (
            "empty title",
            SpotlightInput::Available(b"# \r\nbody\n".to_vec()),
        ),
        ("unavailable", SpotlightInput::Unavailable),
    ];

    let dir = TempDir::new();
    let mut disabled = controller_in(dir.path());
    disabled.set_update(UpdateState::disabled());
    assert_eq!(disabled.view_state().update_banner, None, "disabled");

    for (name, input) in invalid {
        let dir = TempDir::new();
        let mut controller = controller_in(dir.path());
        controller.set_update(UpdateState {
            initial: snapshot_from_spotlight_input(input),
            rx: None,
        });
        assert_eq!(
            controller.view_state().update_banner,
            None,
            "{name}: only an accepted typed spotlight can reach the status line"
        );
    }
}
