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
use herdr_file_viewer::update::{UpdateState, Version};
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

#[test]
fn initial_cached_version_shows_a_banner() {
    let dir = TempDir::new();
    let mut c = controller_in(dir.path());
    c.set_update(UpdateState {
        initial: Some(v(9, 9, 9)),
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
        initial: None,
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
        initial: None,
        rx: Some(rx),
    });
    assert!(
        c.view_state().update_banner.is_none(),
        "nothing until the check returns"
    );

    tx.send(Some(v(2, 0, 0))).unwrap();
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
        initial: Some(v(9, 9, 9)),
        rx: Some(rx),
    });
    assert!(c.view_state().update_banner.is_some());

    tx.send(None).unwrap();
    c.poll().expect("poll applies the result");
    assert!(
        c.view_state().update_banner.is_none(),
        "a successful 'up-to-date' check clears the stale cached banner"
    );
}

#[test]
fn dismiss_hides_the_banner_for_the_session() {
    let dir = TempDir::new();
    let mut c = controller_in(dir.path());
    c.set_update(UpdateState {
        initial: Some(v(9, 9, 9)),
        rx: None,
    });
    let fx = c.handle(Intent::DismissUpdate);
    assert!(fx.redraw, "dismissing repaints to remove the banner");
    assert!(
        c.view_state().update_banner.is_none(),
        "dismissed → hidden for the session"
    );
    // Dismiss again is inert (no banner to hide).
    assert!(!c.handle(Intent::DismissUpdate).redraw);
}
