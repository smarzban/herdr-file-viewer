//! The remote-notice status state on the Session Controller: shown from the initial (cached)
//! value, refreshed from the background channel, and dismissable for the session. No real git /
//! renderer / editor / network, the components are no-op stubs and the update result is injected
//! directly.

mod common;

use common::TempDir;
use herdr_file_viewer::controller::{
    Components, ContentProvider, Controller, EditorHandoff, EditorOutcome, GitService,
    RenderResult, RootProviders,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::update::cache::{self, Cache, CacheWriter};
use herdr_file_viewer::update::spotlight_policy::{
    SpotlightCache, SpotlightInput, cache_delta, project,
};
use herdr_file_viewer::update::{self, NoticeSnapshot, UpdateState, Version};
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::text::Text;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, mpsc};

// ---- minimal no-op stubs (the remote-notice logic exercises none of these) ------------

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
        // non-git: keeps the test focused on remote-notice state
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

fn snapshot_from_cache(cache: Cache, session_started_at_unix: u64) -> NoticeSnapshot {
    update::decide(false, session_started_at_unix, &Some(cache)).initial
}

#[test]
fn initial_cached_version_shows_a_remote_notice_status() {
    let dir = TempDir::new();
    let mut c = controller_in(dir.path());
    c.set_update(UpdateState {
        initial: snapshot(Some(v(9, 9, 9)), None),
        rx: None,
    });
    assert!(
        c.view_state()
            .remote_notice_status
            .is_some_and(|b| b.contains("9.9.9")),
        "a cached newer version is advertised on the first frame"
    );
}

#[test]
fn no_update_means_no_remote_notice_status() {
    let dir = TempDir::new();
    let mut c = controller_in(dir.path());
    c.set_update(UpdateState {
        initial: snapshot(None, None),
        rx: None,
    });
    assert!(c.view_state().remote_notice_status.is_none());
}

#[test]
fn background_result_turns_the_remote_notice_status_on() {
    let dir = TempDir::new();
    let (tx, rx) = mpsc::channel();
    let mut c = controller_in(dir.path());
    c.set_update(UpdateState {
        initial: snapshot(None, None),
        rx: Some(rx),
    });
    assert!(
        c.view_state().remote_notice_status.is_none(),
        "nothing until the check returns"
    );

    tx.send(snapshot(Some(v(2, 0, 0)), None)).unwrap();
    let fx = c.poll().expect("poll applies the background update result");
    assert!(fx.redraw, "a fresh verdict triggers a repaint");
    assert!(
        c.view_state()
            .remote_notice_status
            .is_some_and(|status| status.contains("2.0.0")),
        "the remote-notice status now names the version the check found"
    );
}

#[test]
fn background_up_to_date_clears_a_stale_cached_remote_notice_status() {
    // A cached status, then a successful check that finds nothing newer (`None`) removes it.
    let dir = TempDir::new();
    let (tx, rx) = mpsc::channel();
    let mut c = controller_in(dir.path());
    c.set_update(UpdateState {
        initial: snapshot(Some(v(9, 9, 9)), None),
        rx: Some(rx),
    });
    assert!(c.view_state().remote_notice_status.is_some());

    tx.send(snapshot(None, None)).unwrap();
    c.poll().expect("poll applies the result");
    assert!(
        c.view_state().remote_notice_status.is_none(),
        "a successful 'up-to-date' check clears the stale cached remote-notice status"
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
            .remote_notice_status
            .is_some_and(|status| status.contains("2.0.0")),
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
            .remote_notice_status
            .is_some_and(|status| status.contains("9.9.9")),
        "a disconnected probe keeps the last detected release"
    );
    assert_eq!(
        c.notice_snapshot().spotlight.status_title(),
        Some("Cached"),
        "a disconnected probe keeps the last spotlight in the same snapshot"
    );
}

#[test]
fn dismiss_hides_the_remote_notice_status_for_the_session() {
    let dir = TempDir::new();
    let mut c = controller_in(dir.path());
    c.set_update(UpdateState {
        initial: snapshot(Some(v(9, 9, 9)), Some("Project")),
        rx: None,
    });
    let fx = c.handle(Intent::DismissUpdate);
    assert!(
        fx.redraw,
        "dismissing repaints to remove the remote-notice status"
    );
    assert!(
        c.view_state().remote_notice_status.is_none(),
        "session dismissal hides every notice form at once"
    );
    // Dismiss again is inert (no remote-notice status to hide).
    assert!(!c.handle(Intent::DismissUpdate).redraw);
}

#[test]
fn dismisses_spotlight_only_update_only_and_combined_statuses_without_losing_spotlight_body() {
    let cases = [
        (
            "spotlight",
            snapshot(None, Some("Project")),
            Some(b"body\n".as_slice()),
        ),
        ("update", snapshot(Some(v(9, 9, 9)), None), None),
        (
            "combined",
            snapshot(Some(v(9, 9, 9)), Some("Project")),
            Some(b"body\n".as_slice()),
        ),
    ];

    for (name, initial, expected_body) in cases {
        let dir = TempDir::new();
        let mut controller = controller_in(dir.path());
        controller.set_update(UpdateState { initial, rx: None });

        assert!(
            controller.handle(Intent::DismissUpdate).redraw,
            "{name}: a visible remote-notice line dismisses immediately"
        );
        assert_eq!(
            controller.view_state().remote_notice_status,
            None,
            "{name}: the current process hides the complete line"
        );
        assert_eq!(
            controller.notice_snapshot().spotlight.whats_new_body(),
            expected_body,
            "{name}: footer dismissal never discards the accepted What's New body"
        );
    }
}

#[test]
fn refresh_cannot_clear_a_remote_notice_session_dismissal() {
    let dir = TempDir::new();
    let (tx, rx) = mpsc::channel();
    let mut controller = controller_in(dir.path());
    controller.set_update(UpdateState {
        initial: snapshot(Some(v(9, 9, 9)), Some("Before")),
        rx: Some(rx),
    });

    assert!(controller.handle(Intent::DismissUpdate).redraw);
    tx.send(snapshot(Some(v(10, 0, 0)), Some("After")))
        .expect("send refreshed snapshot");
    assert!(controller.poll().expect("apply refreshed snapshot").redraw);
    assert_eq!(
        controller.view_state().remote_notice_status,
        None,
        "a replacement, even with a new spotlight identity, cannot revive a session dismissal"
    );
    assert_eq!(
        controller.notice_snapshot().spotlight.whats_new_body(),
        Some(b"body\n".as_slice()),
        "the replacement still retains its accepted body for What's New"
    );
}

#[test]
fn controller_keeps_the_writer_for_the_current_spotlight_after_a_snapshot_replacement() {
    let dir = TempDir::new();
    let current = b"# After\nbody\n".to_vec();
    cache::store(
        dir.path(),
        &Cache {
            spotlight: Some(current.clone()),
            spotlight_retrieved_at_unix: Some(1),
            ..Cache::default()
        },
    );
    let (tx, rx) = mpsc::channel();
    let mut initial = snapshot(None, Some("Before"));
    initial.cache_writer = Some(CacheWriter::new(dir.path().to_path_buf()));
    let mut controller = controller_in(dir.path());
    controller.set_update(UpdateState {
        initial,
        rx: Some(rx),
    });

    tx.send(snapshot(None, Some("After")))
        .expect("send current spotlight replacement");
    assert!(controller.poll().expect("apply replacement").redraw);
    assert!(controller.handle(Intent::DismissUpdate).redraw);
    drop(controller);

    assert_eq!(
        cache::load(dir.path())
            .expect("writer drains the queued dismissal")
            .dismissed_spotlight_identity,
        Some(current),
        "the writer survives snapshot replacement and receives only the current identity"
    );
}

#[test]
fn update_dismissal_is_session_only_and_never_persists() {
    let dir = TempDir::new();
    let mut initial = snapshot(Some(v(9, 9, 9)), None);
    initial.cache_writer = Some(CacheWriter::new(dir.path().to_path_buf()));
    let mut controller = controller_in(dir.path());
    controller.set_update(UpdateState { initial, rx: None });

    assert!(controller.handle(Intent::DismissUpdate).redraw);
    drop(controller);
    assert_eq!(
        cache::load(dir.path()),
        None,
        "update-only dismissal queues no cache mutation"
    );

    let mut next_controller = controller_in(dir.path());
    next_controller.set_update(UpdateState {
        initial: snapshot(Some(v(9, 9, 9)), None),
        rx: None,
    });
    assert!(
        next_controller.view_state().remote_notice_status.is_some(),
        "an update remains visible to the next controller"
    );
}

#[test]
fn spotlight_dismissal_persists_exact_identity_and_later_sessions_project_it() {
    let dir = TempDir::new();
    let original = b"# Project\nbody\n".to_vec();
    cache::store(
        dir.path(),
        &Cache {
            spotlight: Some(original.clone()),
            spotlight_retrieved_at_unix: Some(1),
            ..Cache::default()
        },
    );

    let mut initial = snapshot(None, Some("Project"));
    initial.cache_writer = Some(CacheWriter::new(dir.path().to_path_buf()));
    let mut controller = controller_in(dir.path());
    controller.set_update(UpdateState { initial, rx: None });
    assert!(controller.handle(Intent::DismissUpdate).redraw);
    drop(controller);

    let remembered = cache::load(dir.path()).expect("dismissal persists through the T-5 writer");
    assert_eq!(
        remembered.dismissed_spotlight_identity.as_deref(),
        Some(original.as_slice()),
        "only the exact currently accepted spotlight document is remembered"
    );

    let same = snapshot_from_cache(remembered.clone(), 2);
    let mut same_controller = controller_in(dir.path());
    same_controller.set_update(UpdateState {
        initial: same,
        rx: None,
    });
    assert_eq!(
        same_controller.view_state().remote_notice_status,
        None,
        "the exact previously dismissed identity stays hidden"
    );

    let mut changed = remembered.clone();
    changed.apply(cache::CacheDelta::RefreshSpotlight {
        spotlight: b"# Project\nchanged body\n".to_vec(),
        retrieved_at_unix: 2,
    });
    let mut changed_controller = controller_in(dir.path());
    changed_controller.set_update(UpdateState {
        initial: snapshot_from_cache(changed, 3),
        rx: None,
    });
    assert!(
        changed_controller
            .view_state()
            .remote_notice_status
            .is_some(),
        "a changed identity is new spotlight content"
    );

    let mut combined = remembered;
    combined.latest_seen = Some("9.9.9".into());
    let mut combined_controller = controller_in(dir.path());
    combined_controller.set_update(UpdateState {
        initial: snapshot_from_cache(combined, 2),
        rx: None,
    });
    assert_eq!(
        combined_controller
            .view_state()
            .remote_notice_status
            .as_deref(),
        Some("Update v9.9.9 available · ? details · u dismiss"),
        "a remembered spotlight hides only itself when a later session also has an update"
    );
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
        let actual = controller.view_state().remote_notice_status;
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
    assert_eq!(disabled.view_state().remote_notice_status, None, "disabled");

    for (name, input) in invalid {
        let dir = TempDir::new();
        let mut controller = controller_in(dir.path());
        controller.set_update(UpdateState {
            initial: snapshot_from_spotlight_input(input),
            rx: None,
        });
        assert_eq!(
            controller.view_state().remote_notice_status,
            None,
            "{name}: only an accepted typed spotlight can reach the status line"
        );
    }
}
