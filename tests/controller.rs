//! T-18 — Session Controller: intent → coordinated state change (AC-5, AC-6, AC-11,
//! AC-16, AC-26, AC-N3). Every side-effecting component (Git Service, Content Renderer,
//! Editor Launcher) is behind a trait and stubbed, so these tests touch no real git, no
//! external renderer, and launch no editor. The file tree is real (over a temp dir) — the
//! one read-only component the controller drives directly.

mod common;

use common::TempDir;
use herdr_file_viewer::controller::{
    Components, ContentProvider, Controller, EditorHandoff, GitService, RenderResult,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::presenter::Focus;
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::text::Text;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A shared, mutable recorder the stubs append to and tests read back. `Arc<Mutex<_>>`
/// (not `Rc<RefCell<_>>`) so the Git stub is `Send + Sync` — the controller's render worker
/// holds the Git Service on another thread.
type Recorder<T> = Arc<Mutex<Vec<T>>>;

// ---- stubs ----------------------------------------------------------------------------

/// A Git Service stub that records every `changed_set` baseline it is asked for and replays
/// canned status/changed maps, so a test can assert the controller queried git correctly
/// without a real repository.
#[derive(Default, Clone)]
struct StubGit {
    status: BTreeMap<PathBuf, Status>,
    changed: BTreeMap<PathBuf, Status>,
    changed_calls: Recorder<Baseline>,
}

impl GitService for StubGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        self.status.clone()
    }
    fn changed_set(&self, baseline: Baseline) -> BTreeMap<PathBuf, Status> {
        self.changed_calls.lock().unwrap().push(baseline);
        self.changed.clone()
    }
    fn diff(&self, _rel_path: &Path, _baseline: Baseline) -> String {
        String::new()
    }
}

/// A Content Renderer stub: returns fixed text and no notices, so the controller's content
/// coordination runs without an external renderer.
struct StubContent;
impl ContentProvider for StubContent {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        RenderResult { content: Text::raw("stub-content"), notices: Vec::new() }
    }
}

/// An Editor Launcher stub that either succeeds or fails on demand, and records the file it
/// was asked to open.
struct StubEditor {
    fail: bool,
    opened: Recorder<PathBuf>,
}
impl EditorHandoff for StubEditor {
    fn open(&mut self, file: &Path) -> io::Result<bool> {
        self.opened.lock().unwrap().push(file.to_path_buf());
        if self.fail {
            Err(io::Error::other("no editor configured"))
        } else {
            Ok(true)
        }
    }
}

/// Build a controller over `root` with stubbed components. `changed_calls`/`opened` let the
/// caller inspect what the controller asked of git / the editor.
fn controller(
    root: &Path,
    is_git_repo: bool,
    git: StubGit,
    editor_fails: bool,
) -> (Controller, Recorder<Baseline>, Recorder<PathBuf>) {
    let changed_calls = git.changed_calls.clone();
    let opened = Arc::new(Mutex::new(Vec::new()));
    let components = Components {
        git: Arc::new(git),
        content: Box::new(StubContent),
        editor: Box::new(StubEditor { fail: editor_fails, opened: opened.clone() }),
    };
    let ctrl = Controller::new(root.to_path_buf(), is_git_repo, Baseline::Head, components);
    (ctrl, changed_calls, opened)
}

// ---- tests ----------------------------------------------------------------------------

#[test]
fn toggle_ignore_flips_show_ignored_and_signals_redraw() {
    // AC-5: revealing/hiding ignored files is a controller toggle that redraws.
    let dir = TempDir::new();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    assert!(!ctrl.show_ignored(), "ignored files hidden by default (AC-4)");
    let fx = ctrl.handle(Intent::ToggleIgnore);
    assert!(ctrl.show_ignored(), "ToggleIgnore reveals ignored files (AC-5)");
    assert!(fx.redraw, "a state change signals a redraw");

    ctrl.handle(Intent::ToggleIgnore);
    assert!(!ctrl.show_ignored(), "ToggleIgnore again hides them");
}

#[test]
fn toggle_changed_only_flips_in_a_repo() {
    // AC-6: restrict the tree to the changed-set, then restore the full tree.
    let dir = TempDir::new();
    let (mut ctrl, _, _) = controller(dir.path(), true, StubGit::default(), false);

    assert!(!ctrl.changed_only());
    let fx = ctrl.handle(Intent::ToggleChangedOnly);
    assert!(ctrl.changed_only(), "ToggleChangedOnly restricts to changed files (AC-6)");
    assert!(fx.redraw);

    ctrl.handle(Intent::ToggleChangedOnly);
    assert!(!ctrl.changed_only(), "toggling again restores the full tree");
}

#[test]
fn cycle_view_advances_the_selected_files_mode_through_the_applicable_set() {
    // AC-11: the view-mode override steps through applicable_modes and wraps around.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
    // Non-git → unchanged, non-markdown: applicable modes are [SyntaxContent, RawContent].
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    assert_eq!(ctrl.selected_view_mode(), Some(ViewMode::SyntaxContent), "default mode");
    let fx = ctrl.handle(Intent::CycleView);
    assert_eq!(ctrl.selected_view_mode(), Some(ViewMode::RawContent), "advances to the override");
    assert!(fx.redraw);
    ctrl.handle(Intent::CycleView);
    assert_eq!(ctrl.selected_view_mode(), Some(ViewMode::SyntaxContent), "cycle wraps around");
}

#[test]
fn toggle_baseline_recomputes_the_changed_set_and_updates_state() {
    // AC-16: switching the baseline re-queries git for the changed-set against it.
    let dir = TempDir::new();
    let (mut ctrl, changed_calls, _) = controller(dir.path(), true, StubGit::default(), false);
    changed_calls.lock().unwrap().clear(); // ignore the initial load in new()

    assert_eq!(ctrl.baseline(), Baseline::Head);
    let fx = ctrl.handle(Intent::ToggleBaseline);
    assert_eq!(ctrl.baseline(), Baseline::Base, "baseline toggles Head→Base (AC-16)");
    assert!(fx.redraw);
    assert_eq!(
        *changed_calls.lock().unwrap(),
        vec![Baseline::Base],
        "the changed-set is recomputed against the new baseline (AC-16)"
    );
}

#[test]
fn git_intents_are_inert_without_a_repo() {
    // AC-26: in a non-git directory, git-only intents do nothing and never error.
    let dir = TempDir::new();
    let (mut ctrl, changed_calls, _) = controller(dir.path(), false, StubGit::default(), false);

    ctrl.handle(Intent::ToggleChangedOnly);
    assert!(!ctrl.changed_only(), "changed-only is inert without git (AC-26)");

    ctrl.handle(Intent::ToggleBaseline);
    assert_eq!(ctrl.baseline(), Baseline::Head, "baseline is inert without git (AC-26)");
    assert!(changed_calls.lock().unwrap().is_empty(), "no git query is issued without a repo");
}

#[test]
fn an_editor_handoff_error_becomes_a_nonfatal_notice() {
    // The loop must survive a failing component, surfacing it as a notice (design: every
    // component error is a non-fatal status, never a crash).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
    let (mut ctrl, _, opened) = controller(dir.path(), false, StubGit::default(), true);

    let fx = ctrl.handle(Intent::OpenInEditor);
    assert_eq!(opened.lock().unwrap().len(), 1, "the editor hand-off was attempted");
    assert!(!ctrl.notices().is_empty(), "the failure is surfaced as a notice");
    assert!(!fx.quit, "a component error does not end the session");
}

#[test]
fn successful_editor_return_refreshes_git_state() {
    // After the editor returns the file may have changed, so the controller must re-query
    // git — status markers (AC-7) and the changed-set — not just the content pane. Otherwise
    // a freshly-edited file keeps its pre-edit markers/changed-only visibility.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
    let (mut ctrl, changed_calls, opened) = controller(dir.path(), true, StubGit::default(), false);
    changed_calls.lock().unwrap().clear(); // ignore the initial load in new()

    ctrl.handle(Intent::OpenInEditor);
    assert_eq!(opened.lock().unwrap().len(), 1, "the editor was invoked");
    assert!(
        !changed_calls.lock().unwrap().is_empty(),
        "git state is re-queried after a successful editor return (AC-7/AC-16 freshness)"
    );
}

#[test]
fn toggle_focus_switches_columns() {
    // AC-21 trigger: focus moves between the tree and content columns.
    let dir = TempDir::new();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    assert_eq!(ctrl.focus(), Focus::Tree, "the tree holds focus initially");
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Tree);
}

#[test]
fn close_intent_signals_quit() {
    // AC-20: the close key ends the session.
    let dir = TempDir::new();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    let fx = ctrl.handle(Intent::Close);
    assert!(fx.quit, "Close signals the run loop to exit (AC-20)");
}

#[test]
fn navigation_moves_the_cursor_and_signals_redraw() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "1\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "2\n").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    assert_eq!(ctrl.tree().cursor(), 0);
    let fx = ctrl.handle(Intent::NavDown);
    assert_eq!(ctrl.tree().cursor(), 1, "NavDown advances the cursor");
    assert!(fx.redraw);
    ctrl.handle(Intent::NavUp);
    assert_eq!(ctrl.tree().cursor(), 0, "NavUp retreats the cursor");
}

#[test]
fn no_handled_intent_mutates_the_filesystem() {
    // AC-N1 / AC-N3: handling the entire intent vocabulary writes nothing — the viewer is
    // read-only and exposes no edit path (the editor stub launches nothing real).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("c.txt"), "c\n").unwrap();
    let before = snapshot(dir.path());

    let (mut ctrl, _, _) = controller(dir.path(), true, StubGit::default(), false);
    for intent in Intent::ALL {
        if intent == Intent::Close {
            continue; // Close ends the session; exercise every other intent
        }
        let _ = ctrl.handle(intent);
    }

    assert_eq!(snapshot(dir.path()), before, "no intent mutated any file (AC-N1, AC-N3)");
}

/// A sorted (path, bytes) snapshot of every file under `root`, for an exact read-only check.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut entries: Vec<_> =
            std::fs::read_dir(dir).unwrap().filter_map(Result::ok).map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push((p.clone(), std::fs::read(&p).unwrap()));
            }
        }
    }
    walk(root, &mut out);
    out
}
