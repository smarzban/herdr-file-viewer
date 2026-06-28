//! T-19 — Session Controller: off-thread rendering (AC-23). A select intent must dispatch
//! the (potentially slow) content render to a worker thread so `handle()` returns promptly
//! and never blocks input; the rendered content then arrives as a later effect, drained by
//! `poll()`. A deliberately slow renderer stub stands in for glow/delta/bat.

mod common;

use common::TempDir;
use herdr_file_viewer::controller::{
    Components, ContentProvider, Controller, EditorHandoff, GitService, RenderResult, RootProviders,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::text::Text;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A renderer that sleeps before producing output — the stand-in for a slow external CLI.
struct SlowContent {
    delay: Duration,
}
impl ContentProvider for SlowContent {
    fn render(&self, path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        std::thread::sleep(self.delay);
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        RenderResult {
            content: Text::raw(format!("rendered:{name}")),
            notices: Vec::new(),
        }
    }
}

/// A renderer that panics only on `panic_file` and renders normally otherwise — so a test can
/// prove BOTH that a panic is contained (the panic file → placeholder) AND that the worker
/// survives it (a *different* file still renders real content afterwards, which can only arrive
/// if the worker thread lived through the panic).
struct PanicOnContent {
    panic_file: &'static str,
}
impl ContentProvider for PanicOnContent {
    fn render(&self, path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name == self.panic_file {
            panic!("renderer blew up on {name}");
        }
        RenderResult {
            content: Text::raw(format!("rendered:{name}")),
            notices: Vec::new(),
        }
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
    fn diff(&self, _: &Path, _: Baseline, _full: bool) -> String {
        String::new()
    }
}

struct NoEditor;
impl EditorHandoff for NoEditor {
    fn open(&mut self, _: &Path) -> io::Result<bool> {
        Ok(false)
    }
}

/// A Git stub that records the `full_context` flag of every `diff()` call (made on the render
/// worker thread) and reports one changed file — so a test can prove the FullDiff view asks
/// git for whole-file context rather than the compact hunks-only diff.
struct RecordingGit {
    changed: BTreeMap<PathBuf, Status>,
    diff_full_calls: Arc<Mutex<Vec<bool>>>,
}
impl GitService for RecordingGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        self.changed.clone()
    }
    fn changed_set(&self, _: Baseline) -> BTreeMap<PathBuf, Status> {
        self.changed.clone()
    }
    fn diff(&self, _: &Path, _: Baseline, full_context: bool) -> String {
        self.diff_full_calls.lock().unwrap().push(full_context);
        if full_context {
            "FULL".into()
        } else {
            "COMPACT".into()
        }
    }
}

/// Flatten a content `Text` to a plain string for assertions.
fn flatten(text: &Text) -> String {
    text.lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
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
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(NoGit),
            content: Box::new(SlowContent { delay }), // `delay` is Copy → fresh each call
        }),
        editor: Box::new(NoEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );

    // A select intent must return far faster than the render takes — it only dispatches.
    let start = Instant::now();
    let fx = ctrl.handle(Intent::NavDown);
    let handle_took = start.elapsed();
    assert!(
        fx.redraw,
        "the select still asks for a redraw (stale content shown meanwhile)"
    );
    // Non-blocking proof: had handle() waited for the render it would take at least `delay`
    // (the worker's sleep). The dispatch is an in-process channel send (sub-millisecond), so
    // a comfortable margin below `delay` is a robust, non-flaky bound.
    assert!(
        handle_took < delay,
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
    assert_eq!(
        flatten(ctrl.content()),
        "rendered:b.rs",
        "the selected file rendered"
    );
}

#[test]
fn full_diff_mode_asks_git_for_whole_file_context() {
    // PR2 (AC-23 path): cycling a changed file to FullDiff dispatches a render whose worker
    // reads the diff with full_context=true — so the whole file (not just hunks) is diffed.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("c.rs"), "fn main() {}\n").unwrap();
    let mut changed = BTreeMap::new();
    changed.insert(PathBuf::from("c.rs"), Status::Modified);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let git: Arc<dyn GitService> = Arc::new(RecordingGit {
        changed,
        diff_full_calls: calls.clone(),
    });
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::clone(&git),
            content: Box::new(SlowContent {
                delay: Duration::from_millis(0),
            }),
        }),
        editor: Box::new(NoEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), true),
        Baseline::Head,
        components,
    );

    // The changed file defaults to the compact Diff; one cycle advances to FullDiff, which
    // dispatches a render whose worker requests a full-context diff.
    ctrl.handle(Intent::CycleView);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if calls.lock().unwrap().iter().any(|&full| full) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the worker never requested a full-context diff"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        calls.lock().unwrap().contains(&true),
        "FullDiff mode must ask git for whole-file context (full_context=true)"
    );
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
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(NoGit),
            content: Box::new(SlowContent {
                delay: Duration::from_millis(80),
            }),
        }),
        editor: Box::new(NoEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );

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

#[test]
fn a_panicking_renderer_is_contained_and_the_worker_survives() {
    // AC-23 resilience: a renderer panic must not kill the worker (rendering would stop
    // forever) nor crash the app. (The deliberate panic prints to stderr; that is expected.)
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "1\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "2\n").unwrap();
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(NoGit),
            content: Box::new(PanicOnContent { panic_file: "b.rs" }), // `&'static str` is Copy
        }),
        editor: Box::new(NoEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );

    // Select b.rs → its render() panics; the worker must catch it and surface a placeholder.
    ctrl.handle(Intent::NavDown); // b.rs
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if flatten(ctrl.content()).contains("[content unavailable — renderer error]") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the contained-panic placeholder never arrived (the worker likely died)"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    // Now select a.rs → a NORMAL render. Its DISTINCT content can only arrive if the worker
    // survived the earlier panic — a dead worker would leave the placeholder showing forever.
    // This (not the placeholder, which was already on screen) is what proves survival.
    ctrl.handle(Intent::NavUp); // a.rs renders normally
    let deadline2 = Instant::now() + Duration::from_secs(5);
    loop {
        ctrl.poll();
        if flatten(ctrl.content()) == "rendered:a.rs" {
            break;
        }
        assert!(
            Instant::now() < deadline2,
            "the worker did not survive the panic (the post-panic render never arrived)"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
