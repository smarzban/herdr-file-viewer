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
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use herdr_file_viewer::git::{Baseline, Status};
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
    std::fs::write(dir.path().join("notes.md"), "# Title\n").unwrap();
    // Non-git → unchanged markdown: applicable modes are [RenderedMarkdown, SyntaxContent].
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    assert_eq!(ctrl.selected_view_mode(), Some(ViewMode::RenderedMarkdown), "markdown default");
    let fx = ctrl.handle(Intent::CycleView);
    assert_eq!(ctrl.selected_view_mode(), Some(ViewMode::SyntaxContent), "advances to the override");
    assert!(fx.redraw);
    ctrl.handle(Intent::CycleView);
    assert_eq!(ctrl.selected_view_mode(), Some(ViewMode::RenderedMarkdown), "cycle wraps around");
}

#[test]
fn cycle_view_on_a_changed_file_reaches_the_full_context_diff() {
    // PR2 / AC-11: a changed file cycles Diff → FullDiff (whole file + line numbers + the diff
    // inline) → SyntaxContent → wraps. The full-context diff sits right after the compact diff.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("changed.rs"), "fn main() {}\n").unwrap();
    let mut changed = BTreeMap::new();
    changed.insert(PathBuf::from("changed.rs"), Status::Modified);
    let git = StubGit { status: changed.clone(), changed, ..StubGit::default() };
    let (mut ctrl, _, _) = controller(dir.path(), true, git, false);

    assert_eq!(ctrl.selected_view_mode(), Some(ViewMode::Diff), "a changed file defaults to diff");
    ctrl.handle(Intent::CycleView);
    assert_eq!(ctrl.selected_view_mode(), Some(ViewMode::FullDiff), "→ full-context diff");
    ctrl.handle(Intent::CycleView);
    assert_eq!(ctrl.selected_view_mode(), Some(ViewMode::SyntaxContent), "→ syntax content");
    ctrl.handle(Intent::CycleView);
    assert_eq!(ctrl.selected_view_mode(), Some(ViewMode::Diff), "cycle wraps back to the compact diff");
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
    assert_eq!(ctrl.focus(), Focus::Content, "entering zoom focuses the content pane");
    assert!(ctrl.view_state().zoomed, "the view state reflects the zoom for the Presenter");

    ctrl.handle(Intent::ToggleZoom);
    assert!(!ctrl.zoomed(), "the toggle un-zooms");
    assert_eq!(ctrl.focus(), Focus::Tree, "leaving zoom returns focus to the tree");
    assert!(!ctrl.view_state().zoomed, "the view state reflects the un-zoom");
}

#[test]
fn tab_is_inert_while_zoomed_so_focus_stays_on_content() {
    // Regression guard (review-gate R1, 4-model): zoom hides the tree and pins focus to the
    // content pane. Tab must NOT move focus to the now-hidden tree — otherwise j/k would drive
    // the invisible cursor and `dispatch_render` would silently swap the full-screen file.
    let dir = TempDir::new();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    ctrl.handle(Intent::ToggleZoom);
    assert_eq!(ctrl.focus(), Focus::Content, "entering zoom focuses the content pane");

    let fx = ctrl.handle(Intent::ToggleFocus); // Tab while zoomed
    assert_eq!(ctrl.focus(), Focus::Content, "Tab is inert while zoomed — focus stays on content");
    assert!(!fx.redraw, "an inert Tab need not redraw");

    // Un-zoom: Tab works normally again (the guard is scoped to the zoom session).
    ctrl.handle(Intent::ToggleZoom);
    assert_eq!(ctrl.focus(), Focus::Tree, "leaving zoom returns focus to the tree");
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content, "Tab switches columns again once un-zoomed");
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
    assert_eq!(ctrl.focus(), Focus::Tree, "un-zoom returns focus to the tree");

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

    assert_eq!(snapshot(dir.path()), before, "no intent mutated any file (AC-N1, AC-N3)");
}

// ---- content scrolling + wrap (focus-aware navigation) --------------------------------

/// A Content Renderer stub returning a fixed number of single-token lines (`L0`..`L{n-1}`),
/// so a test can scroll a known amount of content.
struct LinesContent {
    n: usize,
}
impl ContentProvider for LinesContent {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        let body = (0..self.n).map(|i| format!("L{i}")).collect::<Vec<_>>().join("\n");
        RenderResult { content: Text::raw(body), notices: Vec::new() }
    }
}

/// A Content Renderer stub returning five 100-column-wide lines (marker `WIDE` at the start
/// of each), for horizontal-scroll tests.
struct WideContent;
impl ContentProvider for WideContent {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        let line = format!("WIDE{}", "x".repeat(96)); // 100 columns
        let body = std::iter::repeat_n(line, 5).collect::<Vec<_>>().join("\n");
        RenderResult { content: Text::raw(body), notices: Vec::new() }
    }
}

/// Flatten the content pane to a string for assertions.
fn flatten(t: &Text) -> String {
    t.lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Spin `poll()` until the worker's render for the current selection lands (or time out).
fn await_marker(ctrl: &mut Controller, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !flatten(ctrl.content()).contains(marker) {
        ctrl.poll();
        assert!(Instant::now() < deadline, "content '{marker}' never rendered");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Build a controller over `root` whose Content Renderer returns `n` lines.
fn controller_with_lines(root: &Path, n: usize) -> Controller {
    let components = Components {
        git: Arc::new(StubGit::default()),
        content: Box::new(LinesContent { n }),
        editor: Box::new(StubEditor { fail: false, opened: Arc::new(Mutex::new(Vec::new())) }),
    };
    Controller::new(root.to_path_buf(), false, Baseline::Head, components)
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
    assert_eq!(ctrl.view_state().content_scroll, 0, "tree focus: content never scrolls");
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
    assert_eq!(ctrl.view_state().content_scroll, 2, "NavDown scrolls the content down");
    ctrl.handle(Intent::NavUp);
    assert_eq!(ctrl.view_state().content_scroll, 1, "NavUp scrolls the content up");

    for _ in 0..10 {
        ctrl.handle(Intent::NavUp);
    }
    assert_eq!(ctrl.view_state().content_scroll, 0, "cannot scroll above the first line");

    for _ in 0..200 {
        ctrl.handle(Intent::NavDown);
    }
    assert_eq!(ctrl.view_state().content_scroll, 40, "cannot scroll past the last screenful");
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
    assert_eq!(ctrl.view_state().content_scroll, 0, "a new selection resets the scroll");
}

#[test]
fn wrap_is_on_for_markdown_and_off_for_code() {
    // The content pane wraps prose (markdown / plain) but not code/diffs, whose column
    // alignment must be preserved.
    let md = TempDir::new();
    std::fs::write(md.path().join("a.md"), "# hi\n").unwrap();
    let (ctrl_md, _, _) = controller(md.path(), false, StubGit::default(), false);
    assert_eq!(ctrl_md.selected_view_mode(), Some(ViewMode::RenderedMarkdown));
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
    assert!(!ctrl.view_state().wrap, "toggling again returns to the mode default");
}

#[test]
fn left_right_scroll_the_content_horizontally_when_focused_and_unwrapped() {
    // A .rs file renders unwrapped (SyntaxContent), so its long lines can overflow the pane;
    // with the content focused, ←/→ scroll it sideways to read them. (When the tree is
    // focused those keys still collapse/expand — covered by the navigation tests.)
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "code\n").unwrap();
    let components = Components {
        git: Arc::new(StubGit::default()),
        content: Box::new(WideContent),
        editor: Box::new(StubEditor { fail: false, opened: Arc::new(Mutex::new(Vec::new())) }),
    };
    let mut ctrl = Controller::new(dir.path().to_path_buf(), false, Baseline::Head, components);
    await_marker(&mut ctrl, "WIDE");
    ctrl.set_content_viewport(20, 10); // widest line 100, viewport 20 → max hscroll = 80
    assert!(!ctrl.view_state().wrap, "a .rs file does not wrap, so horizontal scroll applies");

    ctrl.handle(Intent::ToggleFocus); // focus the content pane
    assert_eq!(ctrl.view_state().content_hscroll, 0, "starts at the left edge");

    let fx = ctrl.handle(Intent::Expand); // → scrolls right
    assert!(fx.redraw);
    let after_one = ctrl.view_state().content_hscroll;
    assert!(after_one > 0, "→ scrolls the content right when focused");
    ctrl.handle(Intent::Expand);
    assert!(ctrl.view_state().content_hscroll > after_one, "→ again scrolls further right");
    ctrl.handle(Intent::Collapse); // ← scrolls left
    assert_eq!(ctrl.view_state().content_hscroll, after_one, "← scrolls back left");

    for _ in 0..50 {
        ctrl.handle(Intent::Collapse);
    }
    assert_eq!(ctrl.view_state().content_hscroll, 0, "cannot scroll left of the start");
    for _ in 0..500 {
        ctrl.handle(Intent::Expand);
    }
    assert_eq!(ctrl.view_state().content_hscroll, 80, "clamps at the widest line minus the viewport");
}

#[test]
fn wrapped_content_scrolls_vertically_to_the_bottom_and_not_horizontally() {
    // With wrap on (a markdown file), the vertical clamp must count WRAPPED rows so the bottom
    // of long prose is reachable (regression: a ceil estimate undercounted word-wrap), and
    // horizontal scrolling is inert (nothing overflows the pane when wrapped).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.md"), "# x\n").unwrap(); // markdown → wraps by default
    let components = Components {
        git: Arc::new(StubGit::default()),
        content: Box::new(WideContent), // 5 lines × 100 columns
        editor: Box::new(StubEditor { fail: false, opened: Arc::new(Mutex::new(Vec::new())) }),
    };
    let mut ctrl = Controller::new(dir.path().to_path_buf(), false, Baseline::Head, components);
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
    assert_eq!(vmax, 10, "scrolls to exactly the last wrapped row (20 rows − 10 tall)");

    let h_before = ctrl.view_state().content_hscroll;
    ctrl.handle(Intent::Expand); // → : would scroll right, but wrap leaves nothing to scroll past
    assert_eq!(ctrl.view_state().content_hscroll, h_before, "no horizontal scroll while wrapping");
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
    assert_eq!(ctrl.view_state().content_scroll, 40, "scrolled to the bottom");

    ctrl.set_content_viewport(40, 30); // taller viewport → max 20; the offset must re-clamp
    assert_eq!(ctrl.view_state().content_scroll, 20, "offset re-clamped to the new, smaller max");
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
    assert!(ctrl.view_state().split_pct > start, "GrowTree widens the tree column");
    ctrl.handle(Intent::ShrinkTree);
    assert_eq!(ctrl.view_state().split_pct, start, "ShrinkTree narrows it back");

    // Clamp at the wide end.
    for _ in 0..50 {
        ctrl.handle(Intent::GrowTree);
    }
    let max = ctrl.view_state().split_pct;
    assert!((20..=80).contains(&max), "split stays within bounds ({max})");
    ctrl.handle(Intent::GrowTree);
    assert_eq!(ctrl.view_state().split_pct, max, "cannot grow past the maximum");

    // Clamp at the narrow end.
    for _ in 0..50 {
        ctrl.handle(Intent::ShrinkTree);
    }
    let min = ctrl.view_state().split_pct;
    assert!((20..=80).contains(&min) && min < max, "split clamps to a minimum ({min})");
    ctrl.handle(Intent::ShrinkTree);
    assert_eq!(ctrl.view_state().split_pct, min, "cannot shrink past the minimum");
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

// ---- mouse (AC-18 is keyboard-first; mouse is additive) -------------------------------

fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE }
}

/// A standard wide two-column layout: tree interior at x=1,y=1 (so visible node `i` is at row
/// `1 + i`), content interior at x=41, and the draggable divider at column 40, over a 100-wide
/// pane anchored at x=0.
fn wide_geometry() -> PaneGeometry {
    PaneGeometry {
        area_x: 0,
        area_width: 100,
        tree_inner: Some(Rect { x: 1, y: 1, width: 38, height: 20 }),
        content_inner: Some(Rect { x: 41, y: 1, width: 58, height: 20 }),
        divider_x: Some(40),
    }
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
    assert_eq!(ctrl.tree().cursor(), 2, "clicking row 3 selects visible node index 2");
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
    assert_eq!(ctrl.focus(), Focus::Content, "clicking the content column focuses it");
}

#[test]
fn double_click_a_folder_toggles_expansion() {
    let dir = TempDir::new();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/inner.txt"), "x").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.set_pane_geometry(wide_geometry());
    assert_eq!(ctrl.tree().visible_nodes().len(), 1, "only the collapsed folder is visible");

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
    assert_eq!(ctrl.tree().visible_nodes().len(), 1, "folder starts collapsed");

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
    assert_eq!(ctrl.tree().visible_nodes().len(), 1, "folder starts collapsed");

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
    assert!(opened.lock().unwrap().is_empty(), "a single click does not open the editor");

    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 6, 1)); // double → zoom
    assert!(ctrl.zoomed(), "double-clicking a file opens it in zoom mode");
    assert_eq!(ctrl.focus(), Focus::Content, "zoom focuses the content pane");
    assert!(opened.lock().unwrap().is_empty(), "double-clicking a file does NOT open the editor");
}

#[test]
fn activate_a_folder_toggles_expansion() {
    // Enter on a directory expands it (and collapses it again) — same as double-click / `l`.
    let dir = TempDir::new();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/inner.txt"), "x").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    assert_eq!(ctrl.tree().visible_nodes().len(), 1, "folder starts collapsed");

    ctrl.handle(Intent::Activate); // cursor is on the folder (only node)
    assert_eq!(ctrl.tree().visible_nodes().len(), 2, "Enter on a folder expands it");
    ctrl.handle(Intent::Activate);
    assert_eq!(ctrl.tree().visible_nodes().len(), 1, "Enter again collapses it");
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
    assert_eq!(ctrl.focus(), Focus::Content, "zoom focuses the content pane");
    assert!(opened.lock().unwrap().is_empty(), "activating a file does NOT open the editor");
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
    assert_eq!(ctrl.content_scroll(), 3, "wheel-down over content scrolls it down");
    ctrl.handle_mouse(mouse(MouseEventKind::ScrollUp, 50, 5));
    assert_eq!(ctrl.content_scroll(), 0, "wheel-up scrolls it back to the top");
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
    assert_eq!(ctrl.tree().cursor(), 1, "wheel-down over the tree moves the selection down");
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
    assert_eq!(ctrl.view_state().split_pct, 60, "the divider tracks the cursor → 60% tree");

    // Releasing ends the drag; a later move is not a resize.
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 60, 0));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 80, 0));
    assert_eq!(ctrl.view_state().split_pct, 60, "no drag in progress → no resize");
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
    assert_eq!(ctrl.tree().cursor(), before, "Shift+click is the terminal's selection, not ours");
    assert!(!fx.redraw && !fx.quit, "Shift+mouse is a no-op for the viewer");
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
    assert_eq!(ctrl.tree().cursor(), before, "clicking the empty area below the tree is inert");
}

#[test]
fn horizontal_wheel_scrolls_the_content_sideways() {
    // ScrollLeft/ScrollRight (trackpad swipe / horizontal wheel) over the content pane scroll it
    // sideways for unwrapped long lines — like the ←/→ keys. (Vertical wheel is covered above.)
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "code\n").unwrap(); // .rs → unwrapped, so hscroll applies
    let components = Components {
        git: Arc::new(StubGit::default()),
        content: Box::new(WideContent),
        editor: Box::new(StubEditor { fail: false, opened: Arc::new(Mutex::new(Vec::new())) }),
    };
    let mut ctrl = Controller::new(dir.path().to_path_buf(), false, Baseline::Head, components);
    await_marker(&mut ctrl, "WIDE");
    ctrl.set_content_viewport(20, 10); // widest line 100, viewport 20 → max hscroll = 80
    ctrl.set_pane_geometry(wide_geometry());
    assert!(!ctrl.view_state().wrap, "a .rs file is unwrapped, so horizontal scroll applies");
    assert_eq!(ctrl.view_state().content_hscroll, 0, "starts at the left edge");

    // Wheel right over the content column (no focus change needed — scroll what's under the cursor).
    ctrl.handle_mouse(mouse(MouseEventKind::ScrollRight, 50, 5));
    assert!(ctrl.view_state().content_hscroll > 0, "horizontal wheel-right scrolls the content right");
    ctrl.handle_mouse(mouse(MouseEventKind::ScrollLeft, 50, 5));
    assert_eq!(ctrl.view_state().content_hscroll, 0, "wheel-left scrolls back to the start");

    // Over the tree, horizontal wheel is inert (the tree has no horizontal scroll).
    ctrl.handle_mouse(mouse(MouseEventKind::ScrollRight, 5, 5));
    assert_eq!(ctrl.view_state().content_hscroll, 0, "horizontal wheel over the tree does nothing");
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
    let changed_calls = git.changed_calls.clone();
    let components = Components {
        git: Arc::new(git),
        content: Box::new(LinesContent { n: 50 }),
        editor: Box::new(StubEditor { fail: false, opened: Arc::new(Mutex::new(Vec::new())) }),
    };
    let mut ctrl = Controller::new(dir.path().to_path_buf(), true, Baseline::Head, components);
    await_marker(&mut ctrl, "L0");
    ctrl.set_content_viewport(40, 10);
    ctrl.handle(Intent::ToggleFocus); // focus the content pane
    ctrl.handle(Intent::NavDown);
    ctrl.handle(Intent::NavDown);
    assert_eq!(ctrl.view_state().content_scroll, 2, "scrolled down two lines");
    let before = changed_calls.lock().unwrap().len();

    let fx = ctrl.handle_focus_gained();
    assert!(fx.redraw, "focus-gain redraws (fresh tree colours)");
    assert!(changed_calls.lock().unwrap().len() > before, "focus-gain re-queries git");
    assert_eq!(ctrl.view_state().content_scroll, 2, "focus-gain does NOT reset the content scroll");
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
        if *n <= 1 { self.first.clone() } else { self.rest.clone() }
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
        RenderResult { content: Text::raw(format!("showing {}", path.display())), notices: Vec::new() }
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
    let components = Components {
        git: Arc::new(git),
        content: Box::new(PathContent),
        editor: Box::new(StubEditor { fail: false, opened: Arc::new(Mutex::new(Vec::new())) }),
    };
    let mut ctrl = Controller::new(dir.path().to_path_buf(), true, Baseline::Head, components);
    ctrl.handle(Intent::ToggleChangedOnly); // changed-only: only a.rs visible → it's selected
    await_marker(&mut ctrl, "a.rs");
    assert_eq!(ctrl.tree().selected().unwrap().path.file_name().unwrap(), "a.rs");

    // Focus-gain: the changed-set is now {b.rs}, so a.rs filters out and the cursor moves to
    // b.rs. The render is async — await it; pre-fix the content stayed on a.rs and this times out.
    ctrl.handle_focus_gained();
    await_marker(&mut ctrl, "b.rs");
    assert_eq!(ctrl.tree().selected().unwrap().path.file_name().unwrap(), "b.rs", "cursor moved to b.rs");
    assert!(flatten(ctrl.content()).contains("b.rs"), "content pane shows the selected file — in sync");
}
