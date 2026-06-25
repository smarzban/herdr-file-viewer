//! T-18 — Session Controller: intent → coordinated state change (AC-5, AC-6, AC-11,
//! AC-16, AC-26, AC-N3). Every side-effecting component (Git Service, Content Renderer,
//! Editor Launcher) is behind a trait and stubbed, so these tests touch no real git, no
//! external renderer, and launch no editor. The file tree is real (over a temp dir) — the
//! one read-only component the controller drives directly.

mod common;

use common::{TempDir, git, init_repo_with_commit};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use herdr_file_viewer::controller::{
    Components, ContentProvider, Controller, EditorHandoff, GitService, RenderResult, RootProviders,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::herdr::HerdrCli;
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::presenter::{Focus, PaneGeometry};
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::layout::Rect;
use ratatui::text::Text;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    fn diff(&self, _rel_path: &Path, _baseline: Baseline, _full_context: bool) -> String {
        String::new()
    }
}

/// A Content Renderer stub: returns fixed text and no notices, so the controller's content
/// coordination runs without an external renderer.
struct StubContent;
impl ContentProvider for StubContent {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        RenderResult {
            content: Text::raw("stub-content"),
            notices: Vec::new(),
        }
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
    let git: Arc<dyn GitService> = Arc::new(git); // build the stub Arc once; clone it inside the factory
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::clone(&git),
            content: Box::new(StubContent),
        }),
        editor: Box::new(StubEditor {
            fail: editor_fails,
            opened: opened.clone(),
        }),
        clipboard: Box::new(common::RecordingClipboard::default()),
    };
    let ctrl = Controller::new(
        common::resolved(root.to_path_buf(), is_git_repo),
        Baseline::Head,
        components,
    );
    (ctrl, changed_calls, opened)
}

// ---- tests ----------------------------------------------------------------------------

#[test]
fn toggle_ignore_flips_show_ignored_and_signals_redraw() {
    // AC-5: revealing/hiding ignored files is a controller toggle that redraws.
    let dir = TempDir::new();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    assert!(
        !ctrl.show_ignored(),
        "ignored files hidden by default (AC-4)"
    );
    let fx = ctrl.handle(Intent::ToggleIgnore);
    assert!(
        ctrl.show_ignored(),
        "ToggleIgnore reveals ignored files (AC-5)"
    );
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
    assert!(
        ctrl.changed_only(),
        "ToggleChangedOnly restricts to changed files (AC-6)"
    );
    assert!(fx.redraw);

    ctrl.handle(Intent::ToggleChangedOnly);
    assert!(
        !ctrl.changed_only(),
        "toggling again restores the full tree"
    );
}

#[test]
fn cycle_view_advances_the_selected_files_mode_through_the_applicable_set() {
    // AC-11: the view-mode override steps through applicable_modes and wraps around.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("notes.md"), "# Title\n").unwrap();
    // Non-git → unchanged markdown: applicable modes are [RenderedMarkdown, SyntaxContent].
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::RenderedMarkdown),
        "markdown default"
    );
    let fx = ctrl.handle(Intent::CycleView);
    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::SyntaxContent),
        "advances to the override"
    );
    assert!(fx.redraw);
    ctrl.handle(Intent::CycleView);
    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::RenderedMarkdown),
        "cycle wraps around"
    );
}

#[test]
fn cycle_view_on_a_changed_file_reaches_the_full_context_diff() {
    // PR2 / AC-11: a changed file cycles Diff → FullDiff (whole file + line numbers + the diff
    // inline) → SyntaxContent → wraps. The full-context diff sits right after the compact diff.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("changed.rs"), "fn main() {}\n").unwrap();
    let mut changed = BTreeMap::new();
    changed.insert(PathBuf::from("changed.rs"), Status::Modified);
    let git = StubGit {
        status: changed.clone(),
        changed,
        ..StubGit::default()
    };
    let (mut ctrl, _, _) = controller(dir.path(), true, git, false);

    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::Diff),
        "a changed file defaults to diff"
    );
    ctrl.handle(Intent::CycleView);
    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::FullDiff),
        "→ full-context diff"
    );
    ctrl.handle(Intent::CycleView);
    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::SyntaxContent),
        "→ syntax content"
    );
    ctrl.handle(Intent::CycleView);
    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::Diff),
        "cycle wraps back to the compact diff"
    );
}

#[test]
fn toggle_baseline_recomputes_the_changed_set_and_updates_state() {
    // AC-16: switching the baseline re-queries git for the changed-set against it.
    let dir = TempDir::new();
    let (mut ctrl, changed_calls, _) = controller(dir.path(), true, StubGit::default(), false);
    changed_calls.lock().unwrap().clear(); // ignore the initial load in new()

    assert_eq!(ctrl.baseline(), Baseline::Head);
    let fx = ctrl.handle(Intent::ToggleBaseline);
    assert_eq!(
        ctrl.baseline(),
        Baseline::Base,
        "baseline toggles Head→Base (AC-16)"
    );
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
    assert!(
        !ctrl.changed_only(),
        "changed-only is inert without git (AC-26)"
    );

    ctrl.handle(Intent::ToggleBaseline);
    assert_eq!(
        ctrl.baseline(),
        Baseline::Head,
        "baseline is inert without git (AC-26)"
    );
    assert!(
        changed_calls.lock().unwrap().is_empty(),
        "no git query is issued without a repo"
    );
}

#[test]
fn an_editor_handoff_error_becomes_a_nonfatal_notice() {
    // The loop must survive a failing component, surfacing it as a notice (design: every
    // component error is a non-fatal status, never a crash).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
    let (mut ctrl, _, opened) = controller(dir.path(), false, StubGit::default(), true);

    let fx = ctrl.handle(Intent::OpenInEditor);
    assert_eq!(
        opened.lock().unwrap().len(),
        1,
        "the editor hand-off was attempted"
    );
    assert!(
        !ctrl.notices().is_empty(),
        "the failure is surfaced as a notice"
    );
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
fn zoom_toggle_hides_tree_and_pins_content_focus() {
    // The `z` zoom toggle collapses the tree so the content pane fills the frame. Entering
    // zoom moves focus to the content pane (so j/k scroll the now-full-screen file); leaving
    // zoom returns focus to the tree (back to picking files). It is pure layout state — the
    // selection and content are unchanged.
    let dir = TempDir::new();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    assert!(!ctrl.zoomed(), "the viewer is not zoomed by default");
    assert_eq!(ctrl.focus(), Focus::Tree, "the tree holds focus initially");

    let fx = ctrl.handle(Intent::ToggleZoom);
    assert!(fx.redraw, "toggling zoom redraws");
    assert!(ctrl.zoomed(), "the viewer is zoomed after the toggle");
    assert_eq!(
        ctrl.focus(),
        Focus::Content,
        "entering zoom focuses the content pane"
    );
    assert!(
        ctrl.view_state().zoomed,
        "the view state reflects the zoom for the Presenter"
    );

    ctrl.handle(Intent::ToggleZoom);
    assert!(!ctrl.zoomed(), "the toggle un-zooms");
    assert_eq!(
        ctrl.focus(),
        Focus::Tree,
        "leaving zoom returns focus to the tree"
    );
    assert!(
        !ctrl.view_state().zoomed,
        "the view state reflects the un-zoom"
    );
}

#[test]
fn tab_is_inert_while_zoomed_so_focus_stays_on_content() {
    // Regression guard (review-gate R1, 4-model): zoom hides the tree and pins focus to the
    // content pane. Tab must NOT move focus to the now-hidden tree — otherwise j/k would drive
    // the invisible cursor and `dispatch_render` would silently swap the full-screen file.
    let dir = TempDir::new();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    ctrl.handle(Intent::ToggleZoom);
    assert_eq!(
        ctrl.focus(),
        Focus::Content,
        "entering zoom focuses the content pane"
    );

    let fx = ctrl.handle(Intent::ToggleFocus); // Tab while zoomed
    assert_eq!(
        ctrl.focus(),
        Focus::Content,
        "Tab is inert while zoomed — focus stays on content"
    );
    assert!(!fx.redraw, "an inert Tab need not redraw");

    // Un-zoom: Tab works normally again (the guard is scoped to the zoom session).
    ctrl.handle(Intent::ToggleZoom);
    assert_eq!(
        ctrl.focus(),
        Focus::Tree,
        "leaving zoom returns focus to the tree"
    );
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(
        ctrl.focus(),
        Focus::Content,
        "Tab switches columns again once un-zoomed"
    );
}

#[test]
fn close_intent_signals_quit() {
    // AC-20: the close key ends the session (when not zoomed).
    let dir = TempDir::new();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    let fx = ctrl.handle(Intent::Close);
    assert!(fx.quit, "Close signals the run loop to exit (AC-20)");
}

#[test]
fn close_backs_out_of_zoom_first_then_quits() {
    // When zoomed, the close key (q/Esc) backs out of zoom rather than quitting — the
    // instinctive "escape the full-screen view". A second press (now un-zoomed) quits (AC-20).
    let dir = TempDir::new();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    ctrl.handle(Intent::ToggleZoom);
    assert!(ctrl.zoomed());

    let fx = ctrl.handle(Intent::Close);
    assert!(!fx.quit, "Close while zoomed does NOT quit");
    assert!(fx.redraw, "it redraws (the tree reappears)");
    assert!(!ctrl.zoomed(), "Close while zoomed un-zooms");
    assert_eq!(
        ctrl.focus(),
        Focus::Tree,
        "un-zoom returns focus to the tree"
    );

    let fx2 = ctrl.handle(Intent::Close);
    assert!(fx2.quit, "Close again (no longer zoomed) quits (AC-20)");
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

    assert_eq!(
        snapshot(dir.path()),
        before,
        "no intent mutated any file (AC-N1, AC-N3)"
    );
}

// ---- content scrolling + wrap (focus-aware navigation) --------------------------------

/// A Content Renderer stub returning a fixed number of single-token lines (`L0`..`L{n-1}`),
/// so a test can scroll a known amount of content.
struct LinesContent {
    n: usize,
}
impl ContentProvider for LinesContent {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        let body = (0..self.n)
            .map(|i| format!("L{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        RenderResult {
            content: Text::raw(body),
            notices: Vec::new(),
        }
    }
}

/// A Content Renderer stub returning five 100-column-wide lines (marker `WIDE` at the start
/// of each), for horizontal-scroll tests.
struct WideContent;
impl ContentProvider for WideContent {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        let line = format!("WIDE{}", "x".repeat(96)); // 100 columns
        let body = std::iter::repeat_n(line, 5).collect::<Vec<_>>().join("\n");
        RenderResult {
            content: Text::raw(body),
            notices: Vec::new(),
        }
    }
}

/// Flatten the content pane to a string for assertions.
fn flatten(t: &Text) -> String {
    t.lines
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

/// Spin `poll()` until the worker's render for the current selection lands (or time out).
fn await_marker(ctrl: &mut Controller, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !flatten(ctrl.content()).contains(marker) {
        ctrl.poll();
        assert!(
            Instant::now() < deadline,
            "content '{marker}' never rendered"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Build a controller over `root` whose Content Renderer returns `n` lines.
fn controller_with_lines(root: &Path, n: usize) -> Controller {
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(StubGit::default()),
            content: Box::new(LinesContent { n }), // `n` is Copy → fresh each call
        }),
        editor: Box::new(StubEditor {
            fail: false,
            opened: Arc::new(Mutex::new(Vec::new())),
        }),
        clipboard: Box::new(common::RecordingClipboard::default()),
    };
    Controller::new(
        common::resolved(root.to_path_buf(), false),
        Baseline::Head,
        components,
    )
}

#[test]
fn nav_does_not_scroll_content_while_the_tree_is_focused() {
    // Default focus is the tree: j/k move the tree cursor and never scroll the content pane.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_lines(dir.path(), 50);
    await_marker(&mut ctrl, "L0");
    ctrl.set_content_viewport(40, 10);

    assert_eq!(ctrl.focus(), Focus::Tree);
    ctrl.handle(Intent::NavDown);
    ctrl.handle(Intent::NavDown);
    assert_eq!(
        ctrl.view_state().content_scroll,
        0,
        "tree focus: content never scrolls"
    );
}

#[test]
fn nav_scrolls_the_content_pane_when_focused_and_clamps_both_ends() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_lines(dir.path(), 50);
    await_marker(&mut ctrl, "L0");
    ctrl.set_content_viewport(40, 10); // 50 lines, 10 visible → max scroll = 40

    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    assert_eq!(ctrl.view_state().content_scroll, 0, "starts at the top");

    ctrl.handle(Intent::NavDown);
    ctrl.handle(Intent::NavDown);
    assert_eq!(
        ctrl.view_state().content_scroll,
        2,
        "NavDown scrolls the content down"
    );
    ctrl.handle(Intent::NavUp);
    assert_eq!(
        ctrl.view_state().content_scroll,
        1,
        "NavUp scrolls the content up"
    );

    for _ in 0..10 {
        ctrl.handle(Intent::NavUp);
    }
    assert_eq!(
        ctrl.view_state().content_scroll,
        0,
        "cannot scroll above the first line"
    );

    for _ in 0..200 {
        ctrl.handle(Intent::NavDown);
    }
    assert_eq!(
        ctrl.view_state().content_scroll,
        40,
        "cannot scroll past the last screenful"
    );
}

#[test]
fn selecting_a_different_file_resets_the_scroll_to_the_top() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "y\n").unwrap();
    let mut ctrl = controller_with_lines(dir.path(), 50);
    await_marker(&mut ctrl, "L0");
    ctrl.set_content_viewport(40, 10);

    ctrl.handle(Intent::ToggleFocus); // focus content
    for _ in 0..5 {
        ctrl.handle(Intent::NavDown);
    }
    assert_eq!(ctrl.view_state().content_scroll, 5, "scrolled down");

    ctrl.handle(Intent::ToggleFocus); // back to the tree
    ctrl.handle(Intent::NavDown); // select the next file
    assert_eq!(
        ctrl.view_state().content_scroll,
        0,
        "a new selection resets the scroll"
    );
}

#[test]
fn wrap_is_on_for_markdown_and_off_for_code() {
    // The content pane wraps prose (markdown / plain) but not code/diffs, whose column
    // alignment must be preserved.
    let md = TempDir::new();
    std::fs::write(md.path().join("a.md"), "# hi\n").unwrap();
    let (ctrl_md, _, _) = controller(md.path(), false, StubGit::default(), false);
    assert_eq!(
        ctrl_md.selected_view_mode(),
        Some(ViewMode::RenderedMarkdown)
    );
    assert!(ctrl_md.view_state().wrap, "markdown content wraps");

    let rs = TempDir::new();
    std::fs::write(rs.path().join("a.rs"), "fn main() {}\n").unwrap();
    let (ctrl_rs, _, _) = controller(rs.path(), false, StubGit::default(), false);
    assert_eq!(ctrl_rs.selected_view_mode(), Some(ViewMode::SyntaxContent));
    assert!(!ctrl_rs.view_state().wrap, "code content does not wrap");
}

#[test]
fn wrap_toggle_forces_wrapping_on_for_code_then_back_to_the_mode_default() {
    // The mode default leaves code/diffs unwrapped (aligned); `w` forces wrap on so long
    // lines can be read, and toggles back to the default.
    let rs = TempDir::new();
    std::fs::write(rs.path().join("a.rs"), "fn main() {}\n").unwrap();
    let (mut ctrl, _, _) = controller(rs.path(), false, StubGit::default(), false);

    assert!(!ctrl.view_state().wrap, "code does not wrap by default");
    let fx = ctrl.handle(Intent::ToggleWrap);
    assert!(fx.redraw);
    assert!(ctrl.view_state().wrap, "`w` forces wrap on for code");
    ctrl.handle(Intent::ToggleWrap);
    assert!(
        !ctrl.view_state().wrap,
        "toggling again returns to the mode default"
    );
}

#[test]
fn left_right_scroll_the_content_horizontally_when_focused_and_unwrapped() {
    // A .rs file renders unwrapped (SyntaxContent), so its long lines can overflow the pane;
    // with the content focused, ←/→ scroll it sideways to read them. (When the tree is
    // focused those keys still collapse/expand — covered by the navigation tests.)
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "code\n").unwrap();
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(StubGit::default()),
            content: Box::new(WideContent),
        }),
        editor: Box::new(StubEditor {
            fail: false,
            opened: Arc::new(Mutex::new(Vec::new())),
        }),
        clipboard: Box::new(common::RecordingClipboard::default()),
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );
    await_marker(&mut ctrl, "WIDE");
    ctrl.set_content_viewport(20, 10); // widest line 100, viewport 20 → max hscroll = 80
    assert!(
        !ctrl.view_state().wrap,
        "a .rs file does not wrap, so horizontal scroll applies"
    );

    ctrl.handle(Intent::ToggleFocus); // focus the content pane
    assert_eq!(
        ctrl.view_state().content_hscroll,
        0,
        "starts at the left edge"
    );

    let fx = ctrl.handle(Intent::Expand); // → scrolls right
    assert!(fx.redraw);
    let after_one = ctrl.view_state().content_hscroll;
    assert!(after_one > 0, "→ scrolls the content right when focused");
    ctrl.handle(Intent::Expand);
    assert!(
        ctrl.view_state().content_hscroll > after_one,
        "→ again scrolls further right"
    );
    ctrl.handle(Intent::Collapse); // ← scrolls left
    assert_eq!(
        ctrl.view_state().content_hscroll,
        after_one,
        "← scrolls back left"
    );

    for _ in 0..50 {
        ctrl.handle(Intent::Collapse);
    }
    assert_eq!(
        ctrl.view_state().content_hscroll,
        0,
        "cannot scroll left of the start"
    );
    for _ in 0..500 {
        ctrl.handle(Intent::Expand);
    }
    assert_eq!(
        ctrl.view_state().content_hscroll,
        80,
        "clamps at the widest line minus the viewport"
    );
}

#[test]
fn wrapped_content_scrolls_vertically_to_the_bottom_and_not_horizontally() {
    // With wrap on (a markdown file), the vertical clamp must count WRAPPED rows so the bottom
    // of long prose is reachable (regression: a ceil estimate undercounted word-wrap), and
    // horizontal scrolling is inert (nothing overflows the pane when wrapped).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.md"), "# x\n").unwrap(); // markdown → wraps by default
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(StubGit::default()),
            content: Box::new(WideContent), // 5 lines × 100 columns
        }),
        editor: Box::new(StubEditor {
            fail: false,
            opened: Arc::new(Mutex::new(Vec::new())),
        }),
        clipboard: Box::new(common::RecordingClipboard::default()),
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );
    await_marker(&mut ctrl, "WIDE");
    ctrl.set_content_viewport(25, 10); // 5 lines × ceil(100/25)=4 = 20 wrapped rows; max = 10
    assert!(ctrl.view_state().wrap, "a .md file wraps");

    ctrl.handle(Intent::ToggleFocus); // focus content
    for _ in 0..500 {
        ctrl.handle(Intent::NavDown);
    }
    // Wrapped rows (20) are counted, not raw lines (5, which would clamp to 0): the bottom is
    // reachable. Exact count via ratatui means no over-scroll into blank past row 20.
    let vmax = ctrl.view_state().content_scroll;
    assert_eq!(
        vmax, 10,
        "scrolls to exactly the last wrapped row (20 rows − 10 tall)"
    );

    let h_before = ctrl.view_state().content_hscroll;
    ctrl.handle(Intent::Expand); // → : would scroll right, but wrap leaves nothing to scroll past
    assert_eq!(
        ctrl.view_state().content_hscroll,
        h_before,
        "no horizontal scroll while wrapping"
    );
}

#[test]
fn shrinking_the_viewport_reclamps_an_existing_scroll_offset() {
    // Resizing the pane smaller lowers the max scroll; an existing offset must be re-clamped
    // so it never points past the end (which would leave blank space below the content).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_lines(dir.path(), 50);
    await_marker(&mut ctrl, "L0");
    ctrl.set_content_viewport(40, 10); // 50 lines, 10 tall → max 40
    ctrl.handle(Intent::ToggleFocus);
    for _ in 0..200 {
        ctrl.handle(Intent::NavDown);
    }
    assert_eq!(
        ctrl.view_state().content_scroll,
        40,
        "scrolled to the bottom"
    );

    ctrl.set_content_viewport(40, 30); // taller viewport → max 20; the offset must re-clamp
    assert_eq!(
        ctrl.view_state().content_scroll,
        20,
        "offset re-clamped to the new, smaller max"
    );
}

#[test]
fn resize_intents_move_the_tree_content_divider_and_clamp() {
    // The tree/content split is adjustable from the keyboard (the viewer owns both columns,
    // so herdr's pane-resize can't move this internal divider).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    let start = ctrl.view_state().split_pct;
    let fx = ctrl.handle(Intent::GrowTree);
    assert!(fx.redraw);
    assert!(
        ctrl.view_state().split_pct > start,
        "GrowTree widens the tree column"
    );
    ctrl.handle(Intent::ShrinkTree);
    assert_eq!(
        ctrl.view_state().split_pct,
        start,
        "ShrinkTree narrows it back"
    );

    // Clamp at the wide end.
    for _ in 0..50 {
        ctrl.handle(Intent::GrowTree);
    }
    let max = ctrl.view_state().split_pct;
    assert!(
        (20..=80).contains(&max),
        "split stays within bounds ({max})"
    );
    ctrl.handle(Intent::GrowTree);
    assert_eq!(
        ctrl.view_state().split_pct,
        max,
        "cannot grow past the maximum"
    );

    // Clamp at the narrow end.
    for _ in 0..50 {
        ctrl.handle(Intent::ShrinkTree);
    }
    let min = ctrl.view_state().split_pct;
    assert!(
        (20..=80).contains(&min) && min < max,
        "split clamps to a minimum ({min})"
    );
    ctrl.handle(Intent::ShrinkTree);
    assert_eq!(
        ctrl.view_state().split_pct,
        min,
        "cannot shrink past the minimum"
    );
}

/// A sorted (path, bytes) snapshot of every file under `root`, for an exact read-only check.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
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

// ---- mouse (AC-18 is keyboard-first; mouse is additive) -------------------------------

fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// A standard wide two-column layout: tree interior at x=1,y=1 (so visible node `i` is at row
/// `1 + i`), content interior at x=41, and the draggable divider at column 40, over a 100-wide
/// pane anchored at x=0.
fn wide_geometry() -> PaneGeometry {
    PaneGeometry {
        area_x: 0,
        area_width: 100,
        tree_inner: Some(Rect {
            x: 1,
            y: 1,
            width: 38,
            height: 20,
        }),
        tree_scroll: 0,
        content_inner: Some(Rect {
            x: 41,
            y: 1,
            width: 58,
            height: 20,
        }),
        divider_x: Some(40),
    }
}

#[test]
fn a_tree_click_maps_through_the_scroll_offset() {
    // #45 coupling: once the tree scrolls (selection past the fold), a click on a visible row must
    // select the node ACTUALLY drawn there — index `(row - tree_inner.y) + tree_scroll`. Without
    // the offset, clicking the first visible row would wrongly select node 0 from the scrolled-off
    // top of the list. `geometry()` feeds `tree_scroll` back; `hit_test` must add it.
    let dir = TempDir::new();
    for i in 0..40 {
        std::fs::write(dir.path().join(format!("f{i:02}.txt")), "x").unwrap();
    }
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    // A short tree interior (height 5) scrolled down by 10, as geometry() feeds back when the
    // selection sits past the fold.
    let mut g = wide_geometry();
    g.tree_inner = Some(Rect {
        x: 1,
        y: 1,
        width: 38,
        height: 5,
    });
    g.tree_scroll = 10;
    ctrl.set_pane_geometry(g);

    // Click the FIRST visible tree row (row == tree_inner.y == 1). With a scroll of 10 that row
    // shows visible node index 10 (f10.txt), so the click must select node 10, not node 0.
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 6, 1));
    assert_eq!(
        ctrl.tree().cursor(),
        10,
        "a click on the first visible row selects node (row-offset + tree_scroll)"
    );
}

#[test]
fn left_click_selects_the_tree_row_it_lands_on() {
    let dir = TempDir::new();
    for f in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(dir.path().join(f), "x").unwrap();
    }
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.set_pane_geometry(wide_geometry());

    // Row 3 = tree_inner.y (1) + index 2 → selects the third visible node and focuses the tree.
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 6, 3));
    assert_eq!(
        ctrl.tree().cursor(),
        2,
        "clicking row 3 selects visible node index 2"
    );
    assert_eq!(ctrl.focus(), Focus::Tree, "a tree click focuses the tree");
    assert!(fx.redraw);
}

#[test]
fn left_click_in_the_content_column_focuses_it() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.set_pane_geometry(wide_geometry());

    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50, 5));
    assert_eq!(
        ctrl.focus(),
        Focus::Content,
        "clicking the content column focuses it"
    );
}

#[test]
fn double_click_a_folder_toggles_expansion() {
    let dir = TempDir::new();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/inner.txt"), "x").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.set_pane_geometry(wide_geometry());
    assert_eq!(
        ctrl.tree().visible_nodes().len(),
        1,
        "only the collapsed folder is visible"
    );

    // Two rapid clicks on row 1 (the folder): the first selects, the second (double) expands.
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 6, 1));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 6, 1));
    assert_eq!(
        ctrl.tree().visible_nodes().len(),
        2,
        "double-clicking the folder expands it to reveal its child"
    );
}

#[test]
fn a_content_click_then_a_same_row_tree_click_is_not_a_double_click() {
    // Regression (opus review of PR #16): the tree and content panes share row numbers, so with
    // the column-agnostic double-click match a content click followed by a tree click on the
    // SAME row must NOT register as a double-click (no spurious activation). A non-tree click
    // clears the pending double-click.
    let dir = TempDir::new();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/inner.txt"), "x").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.set_pane_geometry(wide_geometry());
    assert_eq!(
        ctrl.tree().visible_nodes().len(),
        1,
        "folder starts collapsed"
    );

    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50, 1)); // content pane, row 1
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 4, 1)); // tree folder, same row
    assert_eq!(
        ctrl.tree().visible_nodes().len(),
        1,
        "a content→tree same-row sequence must NOT activate (no spurious expand)"
    );
}

#[test]
fn double_tap_on_the_same_row_activates_even_with_column_jitter() {
    // A touchpad double-tap often lands a column or two apart between taps; as long as both
    // taps are on the same row within the double-click window, it activates (here: expands a
    // folder) just like an exact double-click.
    let dir = TempDir::new();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/inner.txt"), "x").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.set_pane_geometry(wide_geometry());
    assert_eq!(
        ctrl.tree().visible_nodes().len(),
        1,
        "folder starts collapsed"
    );

    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 4, 1)); // tap 1, column 4
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 9, 1)); // tap 2, column 9 (jitter)
    assert_eq!(
        ctrl.tree().visible_nodes().len(),
        2,
        "a same-row double-tap with column jitter still expands the folder"
    );
}

#[test]
fn double_click_a_file_opens_it_in_zoom_mode_single_click_does_not() {
    // Activate (double-click / Enter) on a file opens it in zoom mode — content full-screen —
    // NOT the editor (the editor hand-off is the `e` key only).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let (mut ctrl, _, opened) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.set_pane_geometry(wide_geometry());

    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 6, 1)); // single → select only
    assert!(!ctrl.zoomed(), "a single click does not zoom");
    assert!(
        opened.lock().unwrap().is_empty(),
        "a single click does not open the editor"
    );

    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 6, 1)); // double → zoom
    assert!(
        ctrl.zoomed(),
        "double-clicking a file opens it in zoom mode"
    );
    assert_eq!(
        ctrl.focus(),
        Focus::Content,
        "zoom focuses the content pane"
    );
    assert!(
        opened.lock().unwrap().is_empty(),
        "double-clicking a file does NOT open the editor"
    );
}

#[test]
fn activate_a_folder_toggles_expansion() {
    // Enter on a directory expands it (and collapses it again) — same as double-click / `l`.
    let dir = TempDir::new();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/inner.txt"), "x").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    assert_eq!(
        ctrl.tree().visible_nodes().len(),
        1,
        "folder starts collapsed"
    );

    ctrl.handle(Intent::Activate); // cursor is on the folder (only node)
    assert_eq!(
        ctrl.tree().visible_nodes().len(),
        2,
        "Enter on a folder expands it"
    );
    ctrl.handle(Intent::Activate);
    assert_eq!(
        ctrl.tree().visible_nodes().len(),
        1,
        "Enter again collapses it"
    );
    assert!(!ctrl.zoomed(), "activating a folder never zooms");
}

#[test]
fn activate_a_file_opens_it_in_zoom_mode() {
    // Enter on a file opens it in zoom mode (content pane full-screen, focused) — no editor.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let (mut ctrl, _, opened) = controller(dir.path(), false, StubGit::default(), false);

    assert!(!ctrl.zoomed());
    let fx = ctrl.handle(Intent::Activate); // cursor on the file
    assert!(fx.redraw, "activating redraws");
    assert!(ctrl.zoomed(), "Enter on a file opens it in zoom mode");
    assert_eq!(
        ctrl.focus(),
        Focus::Content,
        "zoom focuses the content pane"
    );
    assert!(
        opened.lock().unwrap().is_empty(),
        "activating a file does NOT open the editor"
    );
}

#[test]
fn wheel_scrolls_the_pane_under_the_cursor() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_lines(dir.path(), 50);
    await_marker(&mut ctrl, "L0");
    ctrl.set_content_viewport(58, 10); // 50 lines, 10 visible → max scroll = 40
    ctrl.set_pane_geometry(wide_geometry());

    // Over the content column → scrolls the content by WHEEL_STEP (3) per notch.
    assert_eq!(ctrl.content_scroll(), 0);
    ctrl.handle_mouse(mouse(MouseEventKind::ScrollDown, 50, 5));
    assert_eq!(
        ctrl.content_scroll(),
        3,
        "wheel-down over content scrolls it down"
    );
    ctrl.handle_mouse(mouse(MouseEventKind::ScrollUp, 50, 5));
    assert_eq!(
        ctrl.content_scroll(),
        0,
        "wheel-up scrolls it back to the top"
    );
}

#[test]
fn wheel_over_the_tree_moves_the_selection() {
    let dir = TempDir::new();
    for f in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(dir.path().join(f), "x").unwrap();
    }
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.set_pane_geometry(wide_geometry());
    assert_eq!(ctrl.tree().cursor(), 0);

    // The tree does not scroll independently, so the wheel moves the selection (its equivalent).
    ctrl.handle_mouse(mouse(MouseEventKind::ScrollDown, 5, 5));
    assert_eq!(
        ctrl.tree().cursor(),
        1,
        "wheel-down over the tree moves the selection down"
    );
    ctrl.handle_mouse(mouse(MouseEventKind::ScrollUp, 5, 5));
    assert_eq!(ctrl.tree().cursor(), 0, "wheel-up moves it back up");
}

#[test]
fn dragging_the_divider_resizes_the_split() {
    let dir = TempDir::new();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.set_pane_geometry(wide_geometry()); // divider at col 40, pane x=0 width=100

    assert_eq!(ctrl.view_state().split_pct, 40, "default split");
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 40, 0)); // grab the divider
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 60, 0)); // drag right
    assert_eq!(
        ctrl.view_state().split_pct,
        60,
        "the divider tracks the cursor → 60% tree"
    );

    // Releasing ends the drag; a later move is not a resize.
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 60, 0));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 80, 0));
    assert_eq!(
        ctrl.view_state().split_pct,
        60,
        "no drag in progress → no resize"
    );
}

#[test]
fn shift_mouse_is_left_to_the_terminal_for_selection() {
    let dir = TempDir::new();
    for f in ["a.txt", "b.txt"] {
        std::fs::write(dir.path().join(f), "x").unwrap();
    }
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.set_pane_geometry(wide_geometry());
    let before = ctrl.tree().cursor();

    let ev = MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 6,
        row: 2,
        modifiers: KeyModifiers::SHIFT,
    };
    let fx = ctrl.handle_mouse(ev);
    assert_eq!(
        ctrl.tree().cursor(),
        before,
        "Shift+click is the terminal's selection, not ours"
    );
    assert!(
        !fx.redraw && !fx.quit,
        "Shift+mouse is a no-op for the viewer"
    );
}

#[test]
fn a_click_below_the_last_row_selects_nothing() {
    let dir = TempDir::new();
    for f in ["a.txt", "b.txt"] {
        std::fs::write(dir.path().join(f), "x").unwrap();
    }
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.set_pane_geometry(wide_geometry());
    let before = ctrl.tree().cursor();

    // Row 12 maps to index 11, past the 2 visible nodes → no selection change.
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 6, 12));
    assert_eq!(
        ctrl.tree().cursor(),
        before,
        "clicking the empty area below the tree is inert"
    );
}

#[test]
fn horizontal_wheel_scrolls_the_content_sideways() {
    // ScrollLeft/ScrollRight (trackpad swipe / horizontal wheel) over the content pane scroll it
    // sideways for unwrapped long lines — like the ←/→ keys. (Vertical wheel is covered above.)
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "code\n").unwrap(); // .rs → unwrapped, so hscroll applies
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(StubGit::default()),
            content: Box::new(WideContent),
        }),
        editor: Box::new(StubEditor {
            fail: false,
            opened: Arc::new(Mutex::new(Vec::new())),
        }),
        clipboard: Box::new(common::RecordingClipboard::default()),
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );
    await_marker(&mut ctrl, "WIDE");
    ctrl.set_content_viewport(20, 10); // widest line 100, viewport 20 → max hscroll = 80
    ctrl.set_pane_geometry(wide_geometry());
    assert!(
        !ctrl.view_state().wrap,
        "a .rs file is unwrapped, so horizontal scroll applies"
    );
    assert_eq!(
        ctrl.view_state().content_hscroll,
        0,
        "starts at the left edge"
    );

    // Wheel right over the content column (no focus change needed — scroll what's under the cursor).
    ctrl.handle_mouse(mouse(MouseEventKind::ScrollRight, 50, 5));
    assert!(
        ctrl.view_state().content_hscroll > 0,
        "horizontal wheel-right scrolls the content right"
    );
    ctrl.handle_mouse(mouse(MouseEventKind::ScrollLeft, 50, 5));
    assert_eq!(
        ctrl.view_state().content_hscroll,
        0,
        "wheel-left scrolls back to the start"
    );

    // Over the tree, horizontal wheel is inert (the tree has no horizontal scroll).
    ctrl.handle_mouse(mouse(MouseEventKind::ScrollRight, 5, 5));
    assert_eq!(
        ctrl.view_state().content_hscroll,
        0,
        "horizontal wheel over the tree does nothing"
    );
}

// ---- refresh: pick up external git changes (the `r` key + focus-gain) ------------------

#[test]
fn refresh_re_queries_git_state_and_redraws() {
    // `r` re-reads git so a change made outside the viewer (merge/pull/commit elsewhere) shows.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let (mut ctrl, changed_calls, _) = controller(dir.path(), true, StubGit::default(), false);
    let before = changed_calls.lock().unwrap().len();

    let fx = ctrl.handle(Intent::Refresh);
    assert!(fx.redraw, "Refresh redraws");
    assert!(
        changed_calls.lock().unwrap().len() > before,
        "Refresh re-queries git for the changed-set"
    );
}

#[test]
fn focus_gained_re_queries_git_but_preserves_content_scroll() {
    // Regaining focus refreshes the tree's git state (external changes show) WITHOUT re-rendering
    // the content — so the user's scroll position is not reset on every focus change.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let git = StubGit::default();
    let changed_calls = git.changed_calls.clone(); // clone the recorder before Arc::new moves the stub
    let git: Arc<dyn GitService> = Arc::new(git);
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::clone(&git),
            content: Box::new(LinesContent { n: 50 }),
        }),
        editor: Box::new(StubEditor {
            fail: false,
            opened: Arc::new(Mutex::new(Vec::new())),
        }),
        clipboard: Box::new(common::RecordingClipboard::default()),
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), true),
        Baseline::Head,
        components,
    );
    await_marker(&mut ctrl, "L0");
    ctrl.set_content_viewport(40, 10);
    ctrl.handle(Intent::ToggleFocus); // focus the content pane
    ctrl.handle(Intent::NavDown);
    ctrl.handle(Intent::NavDown);
    assert_eq!(
        ctrl.view_state().content_scroll,
        2,
        "scrolled down two lines"
    );
    let before = changed_calls.lock().unwrap().len();

    let fx = ctrl.handle_focus_gained();
    assert!(fx.redraw, "focus-gain redraws (fresh tree colours)");
    assert!(
        changed_calls.lock().unwrap().len() > before,
        "focus-gain re-queries git"
    );
    assert_eq!(
        ctrl.view_state().content_scroll,
        2,
        "focus-gain does NOT reset the content scroll"
    );
}

#[test]
fn focus_gained_without_a_repo_is_inert() {
    // No repo → nothing to refresh (AC-26); focus-gain must not force a redraw or a git query.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    let fx = ctrl.handle_focus_gained();
    assert!(!fx.redraw && !fx.quit, "no repo → focus-gain is a no-op");
}

/// A Git stub whose changed-set flips from `first` to `rest` after the first query — so a
/// changed-only re-filter on focus-gain moves the selection. `status` is fixed.
struct EvolvingGit {
    status: BTreeMap<PathBuf, Status>,
    first: BTreeMap<PathBuf, Status>,
    rest: BTreeMap<PathBuf, Status>,
    calls: Arc<Mutex<usize>>,
}
impl GitService for EvolvingGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        self.status.clone()
    }
    fn changed_set(&self, _baseline: Baseline) -> BTreeMap<PathBuf, Status> {
        let mut n = self.calls.lock().unwrap();
        *n += 1;
        if *n <= 1 {
            self.first.clone()
        } else {
            self.rest.clone()
        }
    }
    fn diff(&self, _p: &Path, _b: Baseline, _full: bool) -> String {
        String::new()
    }
}

/// A Content Renderer that renders the file's path, so a test can see which file the content
/// pane is showing (and thus catch a tree/content desync).
struct PathContent;
impl ContentProvider for PathContent {
    fn render(&self, path: &Path, _m: ViewMode, _d: Option<&str>) -> RenderResult {
        RenderResult {
            content: Text::raw(format!("showing {}", path.display())),
            notices: Vec::new(),
        }
    }
}

#[test]
fn focus_gained_keeps_tree_and_content_in_sync_after_a_changed_only_refilter() {
    // Regression (the gate's medium): in changed-only mode, an external change that drops the
    // selected file from the changed-set re-filters the tree on focus-gain and moves the cursor.
    // The content pane must FOLLOW the new selection (pre-fix it stayed on the old file → desync).
    // A single changed file at each step makes the selection deterministic.
    let dir = TempDir::new();
    for f in ["a.rs", "b.rs"] {
        std::fs::write(dir.path().join(f), "x").unwrap();
    }
    let (a, b) = (PathBuf::from("a.rs"), PathBuf::from("b.rs"));
    let git = EvolvingGit {
        status: BTreeMap::from([(a.clone(), Status::Modified), (b.clone(), Status::Modified)]),
        first: BTreeMap::from([(a.clone(), Status::Modified)]), // only a.rs changed → it's selected
        rest: BTreeMap::from([(b.clone(), Status::Modified)]),  // now only b.rs is changed
        calls: Arc::new(Mutex::new(0)),
    };
    let git: Arc<dyn GitService> = Arc::new(git);
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::clone(&git),
            content: Box::new(PathContent),
        }),
        editor: Box::new(StubEditor {
            fail: false,
            opened: Arc::new(Mutex::new(Vec::new())),
        }),
        clipboard: Box::new(common::RecordingClipboard::default()),
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), true),
        Baseline::Head,
        components,
    );
    ctrl.handle(Intent::ToggleChangedOnly); // changed-only: only a.rs visible → it's selected
    await_marker(&mut ctrl, "a.rs");
    assert_eq!(
        ctrl.tree().selected().unwrap().path.file_name().unwrap(),
        "a.rs"
    );

    // Focus-gain: the changed-set is now {b.rs}, so a.rs filters out and the cursor moves to
    // b.rs. The render is async — await it; pre-fix the content stayed on a.rs and this times out.
    ctrl.handle_focus_gained();
    await_marker(&mut ctrl, "b.rs");
    assert_eq!(
        ctrl.tree().selected().unwrap().path.file_name().unwrap(),
        "b.rs",
        "cursor moved to b.rs"
    );
    assert!(
        flatten(ctrl.content()).contains("b.rs"),
        "content pane shows the selected file — in sync"
    );
}

/// Build a controller over `root` whose clipboard records what it was asked to copy, so the
/// path-copy keys (`y` / `Y`) can be asserted without a real clipboard.
fn controller_with_clipboard(root: &Path, is_git_repo: bool) -> (Controller, Recorder<String>) {
    let clipboard = common::RecordingClipboard::default();
    let copied = clipboard.copied.clone();
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(StubGit::default()),
            content: Box::new(StubContent),
        }),
        editor: Box::new(StubEditor {
            fail: false,
            opened: Arc::new(Mutex::new(Vec::new())),
        }),
        clipboard: Box::new(clipboard),
    };
    let ctrl = Controller::new(
        common::resolved(root.to_path_buf(), is_git_repo),
        Baseline::Head,
        components,
    );
    (ctrl, copied)
}

#[test]
fn copy_repo_path_copies_the_repo_relative_path_and_confirms() {
    // `y`: the selected node's path relative to the tree root goes to the clipboard, and the
    // action is confirmed in a notice. Copying a path touches no file (AC-N3).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), false);

    let fx = ctrl.handle(Intent::CopyRepoPath);
    assert!(fx.redraw, "copying redraws to show the confirmation notice");
    assert_eq!(
        copied.lock().unwrap().as_slice(),
        ["a.rs"],
        "the repo-relative path was copied"
    );
    assert!(
        ctrl.notices().iter().any(|n| n.contains("Copied a.rs")),
        "the copy is confirmed: {:?}",
        ctrl.notices()
    );
}

#[test]
fn copy_abs_path_copies_the_absolute_path() {
    // `Y`: the full absolute path goes to the clipboard.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), false);

    ctrl.handle(Intent::CopyAbsPath);
    let log = copied.lock().unwrap();
    assert_eq!(log.len(), 1, "exactly one copy");
    let want = dir.path().join("a.rs").to_string_lossy().into_owned();
    assert_eq!(log[0], want, "the absolute path was copied");
    assert!(
        Path::new(&log[0]).is_absolute(),
        "the copied path is absolute: {}",
        log[0]
    );
}

#[test]
fn copy_path_strips_control_bytes_from_a_hostile_filename() {
    // A filename is attacker-controllable in a browsed repo and may legally contain control bytes —
    // ESC/BEL (a terminal escape, e.g. a forged OSC 52) or a newline (a shell paste-injection when
    // the copied path is later pasted). Both the clipboard payload and the confirmation notice must
    // be stripped of control characters, matching the `sanitize_label` defense the tree and update
    // banner already apply to filesystem-derived strings.
    let dir = TempDir::new();
    let hostile = "a\u{1b}]52;c;evil\u{07}\nrm -rf b";
    std::fs::write(dir.path().join(hostile), "x\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), false);

    ctrl.handle(Intent::CopyRepoPath);
    let log = copied.lock().unwrap();
    assert_eq!(log.len(), 1, "exactly one copy");
    assert_eq!(
        log[0], "a]52;c;evilrm -rf b",
        "control bytes (ESC/BEL/newline) are stripped, printable chars kept"
    );
    assert!(
        !log[0].chars().any(|c| c.is_control()),
        "the copied path carries no control bytes: {:?}",
        log[0]
    );
    assert!(
        ctrl.notices()
            .iter()
            .all(|n| !n.chars().any(|c| c.is_control())),
        "the confirmation notice carries no control bytes: {:?}",
        ctrl.notices()
    );
}

// ---- worktree picker: SwitchWorktree opens it (AC-1, AC-3, AC-4, AC-14) ----------------

/// A fake `HerdrCli` returning canned JSON per subcommand, so the agent-active overlay can be
/// exercised without spawning a real herdr. Keyed on the first arg (`worktree` / `agent`).
struct FakeHerdr {
    worktree_json: String,
    agent_json: String,
}
impl HerdrCli for FakeHerdr {
    fn run_json(&self, args: &[&str]) -> io::Result<String> {
        match args.first().copied() {
            Some("worktree") => Ok(self.worktree_json.clone()),
            Some("agent") => Ok(self.agent_json.clone()),
            _ => Err(io::Error::other("unexpected herdr subcommand")),
        }
    }
}

#[test]
fn switch_worktree_in_a_repo_opens_picker_preselecting_current() {
    // AC-1 / AC-4: SwitchWorktree inside a git repo opens the picker with the repo's worktrees,
    // pre-selecting the current one (no herdr overlay → the current-root fallback). The picker
    // shells REAL git on the controller's root (like tests/worktree.rs), so the root is a real
    // repo; the stub factory's git is independent and only serves status/diff.
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    // A linked worktree so the list has more than one row and the pre-select is meaningful.
    let linked = TempDir::new();
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            linked.path().to_str().unwrap(),
            "-b",
            "linked-branch",
        ],
    );

    let (mut ctrl, _, _) = controller(repo.path(), true, StubGit::default(), false);
    assert!(ctrl.picker().is_none(), "no picker before the switch");

    let fx = ctrl.handle(Intent::SwitchWorktree);
    assert!(fx.redraw, "opening the picker redraws");
    let picker = ctrl
        .picker()
        .expect("SwitchWorktree opens the picker (AC-1)");
    assert!(
        picker.rows.len() >= 2,
        "the picker lists the repo's worktrees: {:?}",
        picker.rows
    );
    let current_idx = picker
        .rows
        .iter()
        .position(|w| w.is_current)
        .expect("one row is the current worktree");
    assert_eq!(
        picker.cursor, current_idx,
        "with no agent overlay the cursor pre-selects the current worktree (AC-4)"
    );
}

#[test]
fn switch_worktree_outside_repo_is_a_noop_with_notice() {
    // AC-14: outside a git repository the worktree switch is a no-op — no picker is opened, and
    // a non-fatal notice explains why.
    let dir = TempDir::new();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    let fx = ctrl.handle(Intent::SwitchWorktree);
    assert!(fx.redraw, "the notice still redraws");
    assert!(
        ctrl.picker().is_none(),
        "no picker is opened outside a repo (AC-14)"
    );
    assert!(
        ctrl.action_notice().is_some(),
        "a non-fatal notice is set outside a repo (AC-14)"
    );
}

#[test]
fn switch_worktree_preselects_agent_active() {
    // AC-3: when the herdr overlay reports a running agent in a specific worktree, the picker
    // pre-selects THAT worktree's row (not the current one). The canned JSON is built from the
    // REAL temp worktree paths so `agent_active`'s path-normalization matches the git rows.
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    let linked = TempDir::new();
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            linked.path().to_str().unwrap(),
            "-b",
            "agent-branch",
        ],
    );

    let (mut ctrl, _, _) = controller(repo.path(), true, StubGit::default(), false);

    // The agent runs in workspace "ws-agent", which the overlay maps to the LINKED worktree —
    // so the pre-select must be the linked row, not the current (main) one. Tier-2 (a unique
    // agent worktree) fires with no own-workspace hint.
    let linked_path = linked.path().to_str().unwrap();
    let worktree_json = format!(
        r#"{{"id": 1, "result": {{"worktrees": [{{"path": "{}", "open_workspace_id": "ws-agent"}}]}}}}"#,
        linked_path
    );
    let agent_json =
        r#"{"id": 2, "result": {"agents": [{"id": "agent-1", "agent": "claude", "agent_status": "working", "workspace_id": "ws-agent"}]}}"#
            .to_string();
    ctrl.set_host(
        Box::new(FakeHerdr {
            worktree_json,
            agent_json,
        }),
        None,
    );

    ctrl.handle(Intent::SwitchWorktree);
    let picker = ctrl.picker().expect("the picker opens");
    let linked_canon = common::canon(linked.path());
    let linked_idx = picker
        .rows
        .iter()
        .position(|w| common::canon(&w.path) == linked_canon)
        .expect("the linked worktree is a row");
    assert_eq!(
        picker.cursor, linked_idx,
        "the cursor pre-selects the agent-active worktree (AC-3): {:?}",
        picker.rows
    );
    // Sanity: the agent worktree is NOT the current one, so this is a real difference from the
    // AC-4 fallback.
    assert!(
        !picker.rows[linked_idx].is_current,
        "the agent worktree differs from the current one"
    );
}

#[test]
fn picker_populates_per_row_agent_statuses_from_the_overlay() {
    // AC-19/AC-20: opening the picker surfaces each worktree's hosting-agent status as a per-row
    // badge, derived ONLY from the same two herdr list queries the pre-select uses — no extra
    // subprocess. The linked worktree hosts a `working` agent → Some("working"); the current
    // worktree hosts none → None. The overlay is queried with exactly two read-only calls.
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    let linked = TempDir::new();
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            linked.path().to_str().unwrap(),
            "-b",
            "status-branch",
        ],
    );

    let (mut ctrl, _, _) = controller(repo.path(), true, StubGit::default(), false);

    // The linked worktree's workspace hosts a REAL agent (`agent` present) reporting `working`.
    let linked_path = linked.path().to_str().unwrap();
    let worktree_json = format!(
        r#"{{"id": 1, "result": {{"worktrees": [{{"path": "{linked_path}", "open_workspace_id": "ws-agent"}}]}}}}"#
    );
    let agent_json =
        r#"{"id": 2, "result": {"agents": [{"id": "a1", "agent": "claude", "agent_status": "working", "workspace_id": "ws-agent"}]}}"#
            .to_string();

    // Count the herdr calls to prove AC-20 (no extra cost): wrap the FakeHerdr in a recorder.
    let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    ctrl.set_host(
        Box::new(RecordingFakeHerdr {
            calls: Arc::clone(&calls),
            worktree_json,
            agent_json,
        }),
        None,
    );

    ctrl.handle(Intent::SwitchWorktree);
    let picker = ctrl.picker().expect("the picker opens");
    assert_eq!(
        picker.agent_statuses.len(),
        picker.rows.len(),
        "statuses are aligned 1:1 with rows"
    );
    let linked_canon = common::canon(linked.path());
    for (i, row) in picker.rows.iter().enumerate() {
        if common::canon(&row.path) == linked_canon {
            assert_eq!(
                picker.agent_statuses[i],
                Some("working".to_string()),
                "the agent worktree row carries its status badge"
            );
        } else {
            assert_eq!(
                picker.agent_statuses[i], None,
                "a worktree with no agent carries no status badge"
            );
        }
    }
    // AC-20: exactly the two read-only overlay queries — no per-worktree call.
    let log = calls.lock().unwrap();
    assert_eq!(
        log.len(),
        2,
        "per-row statuses add no extra herdr call (AC-20): {:?}",
        *log
    );
    assert_eq!(log[0], &["worktree", "list"]);
    assert_eq!(log[1], &["agent", "list"]);
}

#[test]
fn picker_has_no_agent_statuses_without_a_host() {
    // AC-15/AC-20: with no herdr host wired in, the picker is git-only — every row's status is
    // None (no badge), and no overlay query is made.
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    let linked = TempDir::new();
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            linked.path().to_str().unwrap(),
            "-b",
            "no-host-branch",
        ],
    );

    let (mut ctrl, _, _) = controller(repo.path(), true, StubGit::default(), false);
    // NOTE: no `set_host` — herdr is absent.
    ctrl.handle(Intent::SwitchWorktree);
    let picker = ctrl.picker().expect("the picker opens git-only");
    assert_eq!(
        picker.agent_statuses.len(),
        picker.rows.len(),
        "statuses are aligned 1:1 with rows even without a host"
    );
    assert!(
        picker.agent_statuses.iter().all(Option::is_none),
        "no host → every row's agent status is None (git-only, AC-15): {:?}",
        picker.agent_statuses
    );
}

/// A recording `FakeHerdr` for the status-population test: captures each `run_json` argv and
/// returns canned worktree/agent JSON, so the test can assert exactly two read-only calls (AC-20).
struct RecordingFakeHerdr {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    worktree_json: String,
    agent_json: String,
}
impl HerdrCli for RecordingFakeHerdr {
    fn run_json(&self, args: &[&str]) -> io::Result<String> {
        self.calls
            .lock()
            .unwrap()
            .push(args.iter().map(|s| s.to_string()).collect());
        match args.first().copied() {
            Some("worktree") => Ok(self.worktree_json.clone()),
            Some("agent") => Ok(self.agent_json.clone()),
            _ => Err(io::Error::other("unexpected herdr subcommand")),
        }
    }
}

#[test]
fn picker_falls_back_to_git_only_when_herdr_errors() {
    // AC-15: when a `HerdrCli` is present but every `run_json` call returns `Err`, the
    // worktree picker is still opened from git, pre-selects the CURRENT worktree (AC-4
    // fallback — NO agent overlay), and is fully usable (NavDown moves the cursor).
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    let linked = TempDir::new();
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            linked.path().to_str().unwrap(),
            "-b",
            "fallback-branch",
        ],
    );

    struct ErroringHerdr;
    impl HerdrCli for ErroringHerdr {
        fn run_json(&self, _args: &[&str]) -> io::Result<String> {
            Err(io::Error::other("herdr unavailable"))
        }
    }

    let (mut ctrl, _, _) = controller(repo.path(), true, StubGit::default(), false);
    ctrl.set_host(Box::new(ErroringHerdr), Some("ws-anything".into()));
    assert!(ctrl.picker().is_none(), "no picker before SwitchWorktree");

    let fx = ctrl.handle(Intent::SwitchWorktree);
    assert!(fx.redraw, "opening the picker redraws");

    let (rows_len, current_idx) = {
        let picker = ctrl
            .picker()
            .expect("picker is opened despite herdr errors (AC-15)");
        assert!(
            picker.rows.len() >= 2,
            "git-only worktree list is populated: {:?}",
            picker.rows
        );
        let current_idx = picker
            .rows
            .iter()
            .position(|w| w.is_current)
            .expect("one row is the current worktree");
        assert_eq!(
            picker.cursor, current_idx,
            "cursor pre-selects current worktree when herdr errors (AC-4/AC-15): {:?}",
            picker.rows
        );
        (picker.rows.len(), current_idx)
    };

    // Prove the picker is fully usable: NavDown must move the cursor.
    ctrl.handle(Intent::NavDown);
    let after_nav = ctrl.picker().expect("picker still open after NavDown");
    let expected_after_nav = if current_idx + 1 < rows_len {
        current_idx + 1
    } else {
        current_idx // already at bottom — clamped
    };
    assert_eq!(
        after_nav.cursor, expected_after_nav,
        "NavDown moves the cursor — picker is fully usable (AC-15)"
    );
}

// ---- worktree picker: modal routing (AC-5, AC-6, AC-7, AC-11) -------------------------

/// Build a controller rooted at `repo` with a linked worktree already added, open the
/// picker via SwitchWorktree, and return the controller together with the linked path
/// and the main (current) path.
fn setup_picker_with_two_worktrees() -> (Controller, PathBuf, PathBuf) {
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    let linked = TempDir::new();
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            linked.path().to_str().unwrap(),
            "-b",
            "picker-test-branch",
        ],
    );
    let (mut ctrl, _, _) = controller(repo.path(), true, StubGit::default(), false);
    ctrl.handle(Intent::SwitchWorktree);
    assert!(
        ctrl.picker().is_some(),
        "picker should be open after SwitchWorktree"
    );
    let main_path = ctrl.root().to_path_buf();
    let linked_path = linked.path().to_path_buf();
    // Leak TempDirs so the directories exist for the test duration.
    std::mem::forget(repo);
    std::mem::forget(linked);
    (ctrl, main_path, linked_path)
}

#[test]
fn picker_navdown_moves_cursor_and_navup_decrements_and_clamps() {
    // AC-5: NavDown increments the cursor; NavUp decrements; cursor clamps at both ends.
    // Both clamp edges are exercised unconditionally: top (NavUp at row 0) and bottom
    // (NavDown at the last row), regardless of which row the picker pre-selects.
    let (mut ctrl, _, _) = setup_picker_with_two_worktrees();
    let rows_len = ctrl.picker().unwrap().rows.len();
    assert!(rows_len >= 2, "fixture must have at least 2 worktrees");

    // --- top clamp: drive the cursor to row 0, then assert NavUp is inert ---
    while ctrl.picker().unwrap().cursor > 0 {
        let fx = ctrl.handle(Intent::NavUp);
        assert!(fx.redraw, "NavUp returns redraw while moving");
    }
    assert_eq!(ctrl.picker().unwrap().cursor, 0, "cursor is at row 0");
    let fx = ctrl.handle(Intent::NavUp); // one more NavUp — must not move
    assert_eq!(
        ctrl.picker().unwrap().cursor,
        0,
        "NavUp at row 0 clamps (cursor stays at 0)"
    );
    assert!(
        !fx.redraw,
        "NavUp at row 0 returns noop (no move → no redraw)"
    );

    // --- basic movement: NavDown moves from row 0 to row 1 ---
    let fx = ctrl.handle(Intent::NavDown);
    assert!(fx.redraw, "NavDown returns redraw when moving");
    assert_eq!(
        ctrl.picker().unwrap().cursor,
        1,
        "NavDown increments the cursor from 0 to 1"
    );

    // --- bottom clamp: drive the cursor to the last row, then assert NavDown is inert ---
    while ctrl.picker().unwrap().cursor + 1 < rows_len {
        let fx = ctrl.handle(Intent::NavDown);
        assert!(fx.redraw, "NavDown returns redraw while moving");
    }
    assert_eq!(
        ctrl.picker().unwrap().cursor,
        rows_len - 1,
        "cursor is at the last row"
    );
    let fx = ctrl.handle(Intent::NavDown); // one more NavDown — must not move
    assert_eq!(
        ctrl.picker().unwrap().cursor,
        rows_len - 1,
        "NavDown at the last row clamps (cursor stays at last)"
    );
    assert!(
        !fx.redraw,
        "NavDown at the last row returns noop (no move → no redraw)"
    );

    // Picker must still be open after all nav-only intents.
    assert!(
        ctrl.picker().is_some(),
        "picker remains open after nav-only intents"
    );
}

#[test]
fn picker_expand_scrolls_right_and_collapse_scrolls_left_clamped() {
    // Picker-layout §3: while the picker is open, Expand (Right / `l`) scrolls the overlay
    // content right (hscroll increases) and Collapse (Left / `h`) scrolls it left (decreases,
    // clamped at 0). The cursor (row selection) is untouched, and the picker stays open. The
    // controller keeps a raw monotonic hscroll; the Presenter clamps it to the live inner
    // width at draw, so here we only assert the raw value moves in the right direction.
    let (mut ctrl, _, _) = setup_picker_with_two_worktrees();
    assert_eq!(
        ctrl.picker().unwrap().hscroll,
        0,
        "hscroll starts at 0 when the picker opens"
    );
    let cursor_before = ctrl.picker().unwrap().cursor;

    // Collapse at hscroll 0 is a clamped no-op (already left-most → no redraw).
    let fx = ctrl.handle(Intent::Collapse);
    assert_eq!(
        ctrl.picker().unwrap().hscroll,
        0,
        "Collapse at hscroll 0 clamps (stays 0)"
    );
    assert!(!fx.redraw, "a clamped Collapse does not redraw");

    // Expand scrolls right by one step.
    let fx = ctrl.handle(Intent::Expand);
    assert!(fx.redraw, "Expand scrolls right → redraw");
    let after_one = ctrl.picker().unwrap().hscroll;
    assert!(after_one > 0, "Expand increments hscroll");

    // Another Expand scrolls further right.
    ctrl.handle(Intent::Expand);
    assert!(
        ctrl.picker().unwrap().hscroll > after_one,
        "a second Expand scrolls further right"
    );

    // Collapse scrolls back left.
    let fx = ctrl.handle(Intent::Collapse);
    assert!(fx.redraw, "Collapse scrolls left → redraw");
    assert_eq!(
        ctrl.picker().unwrap().hscroll,
        after_one,
        "Collapse returns to the previous step"
    );

    // Cursor never moved, picker stays open.
    assert_eq!(
        ctrl.picker().unwrap().cursor,
        cursor_before,
        "horizontal scroll does not move the row cursor"
    );
    assert!(
        ctrl.picker().is_some(),
        "picker stays open through hscroll intents"
    );
}

#[test]
fn picker_activate_reroots_to_selected_and_closes_picker() {
    // AC-7 + AC-5: Activate confirms — re-roots to the selected (non-current) worktree and
    // closes the picker.
    let (mut ctrl, main_path, linked_path) = setup_picker_with_two_worktrees();

    // Find the index of the linked (non-current) worktree row.
    let linked_canon = common::canon(&linked_path);
    let linked_idx = ctrl
        .picker()
        .unwrap()
        .rows
        .iter()
        .position(|w| common::canon(&w.path) == linked_canon)
        .expect("linked worktree is a row in the picker");

    // Navigate the cursor to the linked row.
    let current_cursor = ctrl.picker().unwrap().cursor;
    if current_cursor < linked_idx {
        for _ in 0..(linked_idx - current_cursor) {
            ctrl.handle(Intent::NavDown);
        }
    } else {
        for _ in 0..(current_cursor - linked_idx) {
            ctrl.handle(Intent::NavUp);
        }
    }
    assert_eq!(
        ctrl.picker().unwrap().cursor,
        linked_idx,
        "cursor is on the linked row before Activate"
    );

    // Confirm.
    let fx = ctrl.handle(Intent::Activate);
    assert!(fx.redraw, "Activate returns redraw");

    // Picker is closed.
    assert!(
        ctrl.picker().is_none(),
        "picker is closed after Activate (AC-7)"
    );

    // Root has changed to the linked worktree (AC-7).
    let new_root = ctrl.root().to_path_buf();
    assert_ne!(
        common::canon(&new_root),
        common::canon(&main_path),
        "root changed away from the main worktree"
    );
    assert_eq!(
        common::canon(&new_root),
        linked_canon,
        "root is now the linked worktree (AC-7)"
    );
}

#[test]
fn picker_close_cancels_leaving_state_unchanged() {
    // AC-6: Close cancels — picker closes, root and all other state are unchanged.
    let (mut ctrl, main_path, _linked_path) = setup_picker_with_two_worktrees();

    let root_before = ctrl.root().to_path_buf();
    let fx = ctrl.handle(Intent::Close);
    assert!(fx.redraw, "Close returns redraw");

    // Picker is closed.
    assert!(
        ctrl.picker().is_none(),
        "picker is closed after Close (AC-6)"
    );

    // Root is unchanged.
    assert_eq!(
        common::canon(ctrl.root()),
        common::canon(&main_path),
        "root is unchanged after Close (AC-6)"
    );
    assert_eq!(
        common::canon(ctrl.root()),
        common::canon(&root_before),
        "root is unchanged after Close"
    );
}

#[test]
fn picker_activate_on_current_worktree_is_a_noop_and_closes_picker() {
    // AC-11: confirming the already-current worktree is a clean no-op (root unchanged) but the
    // picker still closes.
    let (mut ctrl, main_path, _linked_path) = setup_picker_with_two_worktrees();

    // Cursor should already be on the current worktree row (AC-4 pre-select).
    let cursor = ctrl.picker().unwrap().cursor;
    assert!(
        ctrl.picker().unwrap().rows[cursor].is_current,
        "cursor is pre-selected on the current worktree (AC-4)"
    );

    let fx = ctrl.handle(Intent::Activate);
    assert!(fx.redraw, "Activate returns redraw even for no-op");

    // Picker is closed.
    assert!(
        ctrl.picker().is_none(),
        "picker closes after confirm-current (AC-11)"
    );

    // Root is unchanged.
    assert_eq!(
        common::canon(ctrl.root()),
        common::canon(&main_path),
        "root is unchanged after confirm-current (AC-11)"
    );
}

// ---- T-10: AC-10 — no herdr pane-open on switch (recording HerdrCli spy) ---------------

/// A recording `HerdrCli` that captures every `run_json` argv into shared state and returns
/// canned valid JSON so the overlay path runs to completion. The test holds a clone of the
/// `Arc` to read the recorded calls back after the switch completes.
struct RecordingHerdr {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    worktree_json: String,
    agent_json: String,
}

impl RecordingHerdr {
    /// Canned valid JSON for the overlay: a worktree list pointing at `linked_path` in
    /// workspace "ws-spy", and an agent in that workspace (Tier-2 pre-select fires).
    fn new(calls: Arc<Mutex<Vec<Vec<String>>>>, linked_path: &std::path::Path) -> Self {
        let path_str = linked_path.to_str().unwrap_or("");
        Self {
            calls,
            worktree_json: format!(
                r#"{{"id": 1, "result": {{"worktrees": [{{"path": "{path_str}", "open_workspace_id": "ws-spy"}}]}}}}"#
            ),
            agent_json:
                r#"{"id": 2, "result": {"agents": [{"id": "spy-agent", "agent": "claude", "agent_status": "working", "workspace_id": "ws-spy"}]}}"#
                    .to_string(),
        }
    }
}

impl HerdrCli for RecordingHerdr {
    fn run_json(&self, args: &[&str]) -> io::Result<String> {
        self.calls
            .lock()
            .unwrap()
            .push(args.iter().map(|s| s.to_string()).collect());
        match args.first().copied() {
            Some("worktree") => Ok(self.worktree_json.clone()),
            Some("agent") => Ok(self.agent_json.clone()),
            _ => Err(io::Error::other("unexpected subcommand")),
        }
    }
}

#[test]
fn full_switch_issues_only_read_only_herdr_queries_and_no_pane_calls() {
    // AC-10: a complete W → navigate → confirm (re_root) cycle must issue NO herdr pane-open
    // / pane-split / pane-run call. The only herdr calls allowed are the read-only overlay
    // queries the picker makes when it opens: `["worktree","list"]` and `["agent","list"]`
    // (herdr prints JSON by default; no `--json`). The re_root itself must not touch HerdrCli.
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    let linked = TempDir::new();
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            linked.path().to_str().unwrap(),
            "-b",
            "ac10-branch",
        ],
    );

    let (mut ctrl, _, _) = controller(repo.path(), true, StubGit::default(), false);

    // Wire in the recording spy before the switch.
    let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    ctrl.set_host(
        Box::new(RecordingHerdr::new(Arc::clone(&calls), linked.path())),
        None,
    );

    // --- Step 1: open the picker (SwitchWorktree — this is where the overlay queries fire) ---
    let root_before = ctrl.root().to_path_buf();
    ctrl.handle(Intent::SwitchWorktree);
    assert!(
        ctrl.picker().is_some(),
        "picker must open on SwitchWorktree"
    );

    // After opening the picker, check that the ONLY recorded calls so far are the two
    // read-only overlay queries (order is worktree-list then agent-list).
    {
        let log = calls.lock().unwrap();
        assert_eq!(
            log.len(),
            2,
            "exactly 2 herdr calls when opening the picker: {:?}",
            *log
        );
        // NOTE: NO `--json` — herdr prints JSON by default and `agent list` REJECTS the flag
        // (verified live, herdr 0.7.x). Pinning the exact argv here guards against re-introducing
        // the flag, which silently broke the agent-active overlay.
        assert_eq!(log[0], &["worktree", "list"], "first call: worktree list");
        assert_eq!(log[1], &["agent", "list"], "second call: agent list");
    }

    // --- Step 2: navigate to the linked (non-current) worktree row ---
    let linked_canon = common::canon(linked.path());
    let linked_idx = ctrl
        .picker()
        .unwrap()
        .rows
        .iter()
        .position(|w| common::canon(&w.path) == linked_canon)
        .expect("linked worktree is a row in the picker");
    let current_cursor = ctrl.picker().unwrap().cursor;
    // Drive the cursor to the linked row.
    if current_cursor < linked_idx {
        for _ in 0..(linked_idx - current_cursor) {
            ctrl.handle(Intent::NavDown);
        }
    } else {
        for _ in 0..(current_cursor - linked_idx) {
            ctrl.handle(Intent::NavUp);
        }
    }
    assert_eq!(
        ctrl.picker().unwrap().cursor,
        linked_idx,
        "cursor must be on the linked row before Activate"
    );

    // --- Step 3: confirm (Activate → re_root) ---
    ctrl.handle(Intent::Activate);

    // --- Assertions ---

    // The picker must be closed.
    assert!(ctrl.picker().is_none(), "picker closes after Activate");

    // The root must have changed to the linked worktree (re_root ran).
    assert_eq!(
        common::canon(ctrl.root()),
        linked_canon,
        "root is now the linked worktree after confirm"
    );
    assert_ne!(
        common::canon(ctrl.root()),
        common::canon(&root_before),
        "root changed away from the main worktree"
    );

    // THE CORE AC-10 ASSERTION: no pane call was ever issued.
    // A pane call would have first arg "pane" (e.g. "pane split", "pane run").
    // All calls must be read-only list queries; the total count must not grow beyond
    // the two queries the picker already made — re_root must not touch HerdrCli.
    let final_log = calls.lock().unwrap().clone();
    assert_eq!(
        final_log.len(),
        2,
        "re_root must not issue any further herdr calls (still exactly 2 total): {:?}",
        final_log
    );
    for call in &final_log {
        assert_ne!(
            call.first().map(String::as_str),
            Some("pane"),
            "no call may be a pane operation (AC-10): {:?}",
            call
        );
    }
    // More strongly: every recorded call is one of the permitted read-only queries.
    let allowed: &[&[&str]] = &[&["worktree", "list"], &["agent", "list"]];
    for call in &final_log {
        let call_refs: Vec<&str> = call.iter().map(String::as_str).collect();
        assert!(
            allowed.contains(&call_refs.as_slice()),
            "unexpected herdr call (must be a read-only list query): {:?}",
            call
        );
    }
}

#[test]
fn picker_other_intents_are_inert() {
    // Modal: intents other than Nav/Activate/Close are inert while the picker is open.
    let (mut ctrl, main_path, _) = setup_picker_with_two_worktrees();

    let root_before = ctrl.root().to_path_buf();
    let cursor_before = ctrl.picker().unwrap().cursor;

    // These should all be no-ops (picker stays open, root unchanged, cursor unchanged).
    for intent in [
        Intent::ToggleIgnore,
        Intent::ToggleChangedOnly,
        Intent::CycleView,
        Intent::ToggleFocus,
    ] {
        ctrl.handle(intent);
    }

    assert!(
        ctrl.picker().is_some(),
        "picker stays open for inert intents"
    );
    assert_eq!(
        ctrl.picker().unwrap().cursor,
        cursor_before,
        "cursor unchanged for inert intents"
    );
    assert_eq!(
        common::canon(ctrl.root()),
        common::canon(&main_path),
        "root unchanged for inert intents"
    );
    assert_eq!(
        common::canon(ctrl.root()),
        common::canon(&root_before),
        "root unchanged for inert intents (double-check)"
    );
}

#[test]
fn mouse_is_inert_while_the_picker_is_open() {
    // Review-gate R1 (E): the picker is a keyboard modal — while it is open the mouse must be
    // inert, just as the keyboard `handle` gate routes only Nav/Activate/Close to the picker. A
    // click or wheel behind the overlay must not drive the tree/content underneath.
    let (mut ctrl, _, _) = setup_picker_with_two_worktrees();
    ctrl.set_pane_geometry(wide_geometry());

    // Capture the underlying tree state and that the picker is open.
    assert!(
        ctrl.picker().is_some(),
        "picker open before the mouse events"
    );
    let cursor_before = ctrl.tree().cursor();
    let root_before = ctrl.root().to_path_buf();
    let focus_before = ctrl.focus();
    let picker_cursor_before = ctrl.picker().unwrap().cursor;

    // A left-click on a tree row would (without the guard) move the cursor and focus the tree.
    let click = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 6, 3));
    assert!(
        !click.redraw && !click.quit,
        "a click is inert while picking"
    );

    // A scroll over the tree would (without the guard) move the selection.
    let scroll = ctrl.handle_mouse(mouse(MouseEventKind::ScrollDown, 6, 3));
    assert!(
        !scroll.redraw && !scroll.quit,
        "a scroll is inert while picking"
    );

    // A scroll over the content pane would (without the guard) scroll the content.
    let scroll_c = ctrl.handle_mouse(mouse(MouseEventKind::ScrollDown, 50, 5));
    assert!(
        !scroll_c.redraw && !scroll_c.quit,
        "a content scroll is inert while picking"
    );

    // Nothing underneath changed, and the picker is still open and at the same row.
    assert_eq!(
        ctrl.tree().cursor(),
        cursor_before,
        "the tree cursor must be unchanged behind the open picker"
    );
    assert_eq!(ctrl.focus(), focus_before, "focus must be unchanged");
    assert_eq!(
        common::canon(ctrl.root()),
        common::canon(&root_before),
        "the root must be unchanged"
    );
    assert!(
        ctrl.picker().is_some(),
        "the picker must still be open after the mouse events"
    );
    assert_eq!(
        ctrl.picker().unwrap().cursor,
        picker_cursor_before,
        "the picker cursor must be unchanged (mouse does not drive it)"
    );
}

#[test]
fn picker_opens_in_a_single_worktree_repo() {
    // AC-1: SwitchWorktree opens the picker even in a repo with no LINKED worktree — the list has
    // exactly one (current) row. Guards against a future `rows.len() < 2 → no picker` regression:
    // the picker is the place the user learns there is only one worktree, so it must still open.
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());

    let (mut ctrl, _, _) = controller(repo.path(), true, StubGit::default(), false);
    assert!(ctrl.picker().is_none(), "no picker before the switch");

    let fx = ctrl.handle(Intent::SwitchWorktree);
    assert!(fx.redraw, "opening the picker redraws");
    let picker = ctrl
        .picker()
        .expect("SwitchWorktree opens the picker even with a single worktree (AC-1)");
    assert_eq!(
        picker.rows.len(),
        1,
        "a single-worktree repo lists exactly one row: {:?}",
        picker.rows
    );
    assert!(
        picker.rows[0].is_current,
        "the sole row is the current worktree"
    );
    assert_eq!(
        picker.cursor, 0,
        "the cursor pre-selects the sole (current) worktree"
    );
}

// ---------------------------------------------------------------------------
// T-17 — No-events conformance: re_root only via SwitchWorktree (AC-N5)
// ---------------------------------------------------------------------------

/// AC-N5 (automatable analog): the viewer re-roots only in response to the explicit
/// `SwitchWorktree → picker → Activate` sequence — no other intent ever changes the root,
/// and no event/timer path exists (the manifest side of AC-N5 is covered in tests/manifest.rs
/// by `declares_no_event_hooks`).
///
/// Assertions:
/// 1. Every intent in `Intent::ALL` except `SwitchWorktree`, applied with the picker CLOSED,
///    leaves the root UNCHANGED and the picker CLOSED.
/// 2. `SwitchWorktree` opens the picker but does NOT itself change the root.
/// 3. The only re-root path is `SwitchWorktree` → `NavDown` → `Activate`.
#[test]
fn re_root_only_reachable_via_switch_worktree_intent() {
    // Set up a real git repo with a linked worktree, so a re-root is *possible* — it just
    // must only happen via the explicit picker-confirm path.
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    std::fs::write(repo.path().join("main.txt"), "main\n").unwrap();

    let linked = TempDir::new();
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            linked.path().to_str().unwrap(),
            "-b",
            "acn5-branch",
        ],
    );
    // Leak the TempDirs so the directories survive the duration of the test.
    std::mem::forget(linked);

    let (mut ctrl, _, _) = controller(repo.path(), true, StubGit::default(), false);

    // -------------------------------------------------------------------------
    // Part 1: every intent except SwitchWorktree, with the picker CLOSED,
    // must leave the root UNCHANGED and the picker CLOSED.
    // -------------------------------------------------------------------------
    let root_before = ctrl.root().to_path_buf();
    assert!(
        ctrl.picker().is_none(),
        "precondition: picker starts closed"
    );

    for &intent in Intent::ALL.iter().filter(|&&i| i != Intent::SwitchWorktree) {
        ctrl.handle(intent);
        // Drain any async poll to be safe — none should produce a root change.
        ctrl.poll();

        assert_eq!(
            common::canon(ctrl.root()),
            common::canon(&root_before),
            "AC-N5: intent {intent:?} must not change the root (picker closed path)"
        );
        assert!(
            ctrl.picker().is_none(),
            "AC-N5: intent {intent:?} must not open the picker (and leave it auto-confirmed)"
        );
    }

    // -------------------------------------------------------------------------
    // Part 2: SwitchWorktree opens the picker but does NOT change the root.
    // -------------------------------------------------------------------------
    ctrl.handle(Intent::SwitchWorktree);

    assert!(
        ctrl.picker().is_some(),
        "AC-N5: SwitchWorktree must open the picker"
    );
    assert_eq!(
        common::canon(ctrl.root()),
        common::canon(&root_before),
        "AC-N5: SwitchWorktree itself must not re-root — it only opens the picker"
    );

    // Close the picker to reset state for Part 3.
    ctrl.handle(Intent::Close);
    assert!(ctrl.picker().is_none(), "picker closed after Close intent");

    // -------------------------------------------------------------------------
    // Part 3: SwitchWorktree → NavDown → Activate is the ONLY path that changes the root.
    // -------------------------------------------------------------------------
    ctrl.handle(Intent::SwitchWorktree);
    assert!(
        ctrl.picker().is_some(),
        "picker opens for the full-switch path"
    );
    // Move the cursor away from the current worktree (index 0 / pre-selected current).
    ctrl.handle(Intent::NavDown);
    // Confirm: this is the one and only re-root path.
    ctrl.handle(Intent::Activate);

    assert!(ctrl.picker().is_none(), "picker closes after Activate");
    assert_ne!(
        common::canon(ctrl.root()),
        common::canon(&root_before),
        "AC-N5: root MUST change after SwitchWorktree → NavDown → Activate"
    );
}
