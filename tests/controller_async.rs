//! T-19 — Session Controller: off-thread rendering (AC-23). A select intent must dispatch
//! the (potentially slow) content render to a worker thread so `handle()` returns promptly
//! and never blocks input; the rendered content then arrives as a later effect, drained by
//! `poll()`. A deliberately slow renderer stub stands in for glow/delta/bat.

mod common;

use common::TempDir;
use herdr_file_viewer::controller::{
    Components, ContentProvider, Controller, EditorHandoff, GitService, RenderResult,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::text::Text;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A renderer that sleeps before producing output — the stand-in for a slow external CLI.
struct SlowContent {
    delay: Duration,
}
impl ContentProvider for SlowContent {
    fn render(&self, path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        std::thread::sleep(self.delay);
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        RenderResult { content: Text::raw(format!("rendered:{name}")), notices: Vec::new() }
    }
}

struct NoGit;
impl GitService for NoGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }
    fn changed_set(&self, _: Baseline) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }
    fn diff(&self, _: &Path, _: Baseline) -> String {
        String::new()
    }
}

struct NoEditor;
impl EditorHandoff for NoEditor {
    fn open(&mut self, _: &Path) -> io::Result<bool> {
        Ok(false)
    }
}

/// Flatten a content `Text` to a plain string for assertions.
fn flatten(text: &Text) -> String {
    text.lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_select_intent_does_not_block_on_a_slow_render_and_content_arrives_later() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "1\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "2\n").unwrap();

    let delay = Duration::from_millis(150);
    let components = Components {
        git: Box::new(NoGit),
        content: Box::new(SlowContent { delay }),
        editor: Box::new(NoEditor),
    };
    let mut ctrl =
        Controller::new(dir.path().to_path_buf(), false, Baseline::Head, components);

    // A select intent must return far faster than the render takes — it only dispatches.
    let start = Instant::now();
    let fx = ctrl.handle(Intent::NavDown);
    let handle_took = start.elapsed();
    assert!(fx.redraw, "the select still asks for a redraw (stale content shown meanwhile)");
    assert!(
        handle_took < delay / 2,
        "handle() must not block on the slow render (took {handle_took:?}, render is {delay:?})"
    );
    // The fresh content has not arrived yet — proof the render is off-thread (AC-23).
    assert!(
        !flatten(ctrl.content()).contains("b.rs"),
        "selected file's content must not be ready synchronously"
    );

    // Drain results until the latest selection's content arrives as a later effect.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut redrew = false;
    loop {
        if let Some(p) = ctrl.poll() {
            redrew |= p.redraw;
        }
        if flatten(ctrl.content()).contains("b.rs") {
            break;
        }
        assert!(Instant::now() < deadline, "rendered content never arrived");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(redrew, "the arriving content signalled a redraw via poll()");
    assert_eq!(flatten(ctrl.content()), "rendered:b.rs", "the selected file rendered");
}

#[test]
fn a_superseded_render_does_not_overwrite_a_newer_selection() {
    // Rapid navigation: an earlier file's slow render must not clobber the content of the
    // file the user has since moved to (stale results are dropped by sequence).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "1\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "2\n").unwrap();
    std::fs::write(dir.path().join("c.rs"), "3\n").unwrap();

    let components = Components {
        git: Box::new(NoGit),
        content: Box::new(SlowContent { delay: Duration::from_millis(80) }),
        editor: Box::new(NoEditor),
    };
    let mut ctrl =
        Controller::new(dir.path().to_path_buf(), false, Baseline::Head, components);

    // Fire several selections back-to-back; only the last (c.rs) should win.
    ctrl.handle(Intent::NavDown); // b.rs
    ctrl.handle(Intent::NavDown); // c.rs

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if flatten(ctrl.content()) == "rendered:c.rs" {
            break;
        }
        assert!(Instant::now() < deadline, "final selection never rendered");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Give any stale (a.rs/b.rs) results a chance to wrongly land, then re-check.
    std::thread::sleep(Duration::from_millis(50));
    ctrl.poll();
    assert_eq!(
        flatten(ctrl.content()),
        "rendered:c.rs",
        "a superseded render must not overwrite the newer selection"
    );
}
