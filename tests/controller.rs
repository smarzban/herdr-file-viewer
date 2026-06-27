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
fn toggle_hidden_hides_dotfiles_in_the_tree_and_redraws() {
    // #46: the `.` toggle drops dot-prefixed entries from the tree, independent of the gitignore
    // toggle, and signals a redraw. Off by default (dotfiles visible).
    let dir = TempDir::new();
    std::fs::write(dir.path().join(".secret"), "x").unwrap();
    std::fs::write(dir.path().join("keep.txt"), "k").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    let names = |c: &Controller| -> Vec<String> {
        c.tree()
            .visible_nodes()
            .iter()
            .map(|n| n.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    assert!(!ctrl.hide_hidden(), "dotfiles visible by default");
    assert!(
        names(&ctrl).contains(&".secret".to_string()),
        "a dotfile shows by default"
    );

    let fx = ctrl.handle(Intent::ToggleHidden);
    assert!(fx.redraw, "a filter change signals a redraw");
    assert!(ctrl.hide_hidden(), "ToggleHidden turns hiding on");
    assert!(
        !names(&ctrl).contains(&".secret".to_string()),
        "#46: the dotfile is hidden after the toggle"
    );
    assert!(
        names(&ctrl).contains(&"keep.txt".to_string()),
        "regular files remain"
    );

    ctrl.handle(Intent::ToggleHidden);
    assert!(
        names(&ctrl).contains(&".secret".to_string()),
        "ToggleHidden again reveals dotfiles"
    );
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
fn scroll_to_line_brings_the_target_line_into_view_and_clamps_out_of_range() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_lines(dir.path(), 50);
    await_marker(&mut ctrl, "L0");
    ctrl.set_content_viewport(40, 10); // 50 lines, 10 tall → max_content_scroll = 40

    // a line already near the top: line 1 lands at the top
    ctrl.scroll_to_line(1);
    assert_eq!(ctrl.content_scroll(), 0, "line 1 at the top");

    // a mid-file line lands near the top (offset = line-1), still well within the clamp
    ctrl.scroll_to_line(25);
    let off = ctrl.content_scroll();
    assert!(
        off <= 24 && 24 < off + 10,
        "line 25 is within the 10-row viewport"
    );
    assert_eq!(off, 24, "lands the target near the top");

    // below 1 clamps to line 1 (AC-4)
    ctrl.scroll_to_line(0);
    assert_eq!(ctrl.content_scroll(), 0, "0 clamps to line 1");

    // above the last clamps to the last line → last screenful (AC-4); line 50 still visible
    ctrl.scroll_to_line(1000);
    let off = ctrl.content_scroll();
    assert_eq!(off, 40, "beyond the last line shows the last screenful");
    assert!(
        off <= 49 && 49 < off + 10,
        "the last line (50) is within the viewport"
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
        tree_content_width: 0,
        tree_vbar: None,
        tree_hbar: None,
        content_inner: Some(Rect {
            x: 41,
            y: 1,
            width: 58,
            height: 20,
        }),
        content_vbar: None,
        content_hbar: None,
        divider_x: Some(40),
        finder_rows: None,
        finder_scroll: 0,
        finder_max_hscroll: 0,
        finder_vbar: None,
        picker_max_hscroll: 0,
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
fn dragging_the_tree_horizontal_scrollbar_scrolls_the_tree() {
    // The tree's horizontal scrollbar (bottom border) is draggable: press at the right end jumps
    // to max h-scroll, dragging to the left end returns to 0. Synchronous — driven purely by the
    // fed-back geometry (widest row vs tree width).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    let mut g = wide_geometry(); // tree_inner x1 y1 w38 h20
    g.tree_content_width = 138; // 100 wider than the track → max h-scroll = 100
    // The tree's horizontal scrollbar track (fed back by the presenter): the tree's bottom inner
    // row, spanning the text columns [1, 39).
    let hbar_row = 20;
    g.tree_hbar = Some(Rect {
        x: 1,
        y: hbar_row,
        width: 38,
        height: 1,
    });
    ctrl.set_pane_geometry(g);

    // Press at the far-right of the track (col 38 = track.x + width - 1) → max.
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 38, hbar_row));
    assert_eq!(
        ctrl.view_state().tree_hscroll,
        100,
        "pressing the right end of the tree hbar jumps to max h-scroll"
    );
    // Drag to the left end → back to 0.
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 1, hbar_row));
    assert_eq!(
        ctrl.view_state().tree_hscroll,
        0,
        "dragging the tree hbar to the left end scrolls back to 0"
    );
    // Release ends the drag (so the next press is a fresh interaction, not swallowed).
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 1, hbar_row));
    assert!(!fx.redraw, "the drag-release is inert (not a click)");
}

#[test]
fn dragging_the_tree_vertical_scrollbar_scrubs_the_selection() {
    // The tree's vertical scrollbar is now draggable (it lives inside the pane, off the divider):
    // pressing the bottom selects the last file, dragging to the top selects the first — the tree
    // has no independent vertical offset, so the bar scrubs the selection through the list (#45).
    let dir = TempDir::new();
    for i in 0..20 {
        std::fs::write(dir.path().join(format!("f{i:02}.txt")), "x").unwrap();
    }
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    let mut g = wide_geometry();
    // The tree's vertical scrollbar track: a 1-col rect spanning the tree's text rows [1, 21).
    g.tree_vbar = Some(Rect {
        x: 37,
        y: 1,
        width: 1,
        height: 20,
    });
    ctrl.set_pane_geometry(g);

    // Press at the bottom of the track → the last of the 20 files (index 19).
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 37, 20));
    assert_eq!(
        ctrl.tree().cursor(),
        19,
        "pressing the bottom of the tree vbar selects the last node"
    );
    // Drag to the top → the first file.
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 37, 1));
    assert_eq!(
        ctrl.tree().cursor(),
        0,
        "dragging the tree vbar to the top selects the first node"
    );
}

#[test]
fn dragging_the_content_vertical_scrollbar_scrolls_the_content() {
    // The content pane's vertical scrollbar (right border) is draggable: press at the bottom jumps
    // toward max scroll, dragging to the top returns to 0.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_lines(dir.path(), 50);
    await_marker(&mut ctrl, "L0");
    ctrl.set_content_viewport(58, 20); // 50 lines / 20 visible → max scroll 30
    let mut g = wide_geometry();
    // The content's vertical scrollbar track (fed back by the presenter): a 1-col rect in the
    // content pane spanning its 20 text rows.
    let vbar_col = 99;
    g.content_vbar = Some(Rect {
        x: vbar_col,
        y: 1,
        width: 1,
        height: 20,
    });
    ctrl.set_pane_geometry(g);

    // Press at the bottom of the track (row 20 = track.y + height - 1) → max scroll.
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), vbar_col, 20));
    assert_eq!(
        ctrl.view_state().content_scroll,
        30,
        "pressing the bottom of the content vbar jumps to max scroll"
    );
    // Drag to the top of the track → 0.
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), vbar_col, 1));
    assert_eq!(
        ctrl.view_state().content_scroll,
        0,
        "dragging the content vbar to the top scrolls back to 0"
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
fn picker_hscroll_does_not_overshoot_past_the_measured_max() {
    // SMA-229: Expand (→) is monotonic (it can't know the row widths), so over-scrolling right used
    // to park the picker's stored hscroll past the real maximum; the first few Collapse (←) presses
    // then appeared to do nothing while the overshoot burned back down. The Presenter now feeds back
    // `picker_max_hscroll` and `set_pane_geometry` clamps the stored offset to it each frame (the
    // same fix as the finder, mirroring `content_hscroll`), so one Collapse always moves the view.
    let (mut ctrl, _, _) = setup_picker_with_two_worktrees();

    // Geometry the Presenter would feed back: the widest row needs at most 8 columns of h-scroll.
    let geom = PaneGeometry {
        picker_max_hscroll: 8,
        ..wide_geometry()
    };

    // Over-scroll right well past the max (several monotonic Expand steps of HSCROLL_STEP=8).
    for _ in 0..3 {
        ctrl.handle(Intent::Expand);
    }
    assert!(
        ctrl.picker().unwrap().hscroll > 8,
        "precondition: raw Expand overshoots the max when unclamped in isolation"
    );

    // The run loop feeds the measured geometry back after the draw → the stored offset is clamped.
    ctrl.set_pane_geometry(geom);
    assert_eq!(
        ctrl.picker().unwrap().hscroll,
        8,
        "geometry feedback clamps the stored picker hscroll to the measured maximum"
    );

    // A SINGLE Collapse now visibly moves the view — no overshoot left to burn down first.
    ctrl.handle(Intent::Collapse);
    assert!(
        ctrl.picker().unwrap().hscroll < 8,
        "one Collapse moves immediately after the clamp (the bug was: it needed several)"
    );
}

#[test]
fn view_state_titles_the_tree_with_root_basename_and_branch() {
    // SMA-249: the tree's borders are driven by view_state().root_name (the root directory
    // basename) and .branch (the cached current git branch — None outside a repo / detached).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let (ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    let vs = ctrl.view_state();
    let expected = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        vs.root_name, expected,
        "root_name is the root directory basename"
    );
    assert!(vs.branch.is_none(), "branch is None outside a git repo");
}

#[test]
fn refresh_updates_the_cached_branch_after_an_external_checkout() {
    // review-gate R1 (SMA-249): the tree's bottom-border branch is cached on the controller, so it
    // must be refreshed by refresh_git_state (the `r` key / editor-return / focus-gain), not only at
    // (re-)root — otherwise an external `git checkout` while the viewer is open leaves it stale.
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    let (mut ctrl, _, _) = controller(repo.path(), true, StubGit::default(), false);
    assert!(
        ctrl.view_state().branch.is_some(),
        "precondition: on a branch in a real repo, branch is Some"
    );

    // Check out a new branch externally (as another pane / tool would), then refresh.
    git(repo.path(), &["checkout", "-b", "zz-refresh-branch"]);
    ctrl.handle(Intent::Refresh);

    assert_eq!(
        ctrl.view_state().branch.as_deref(),
        Some("zz-refresh-branch"),
        "refresh picks up the externally-changed branch (was stale before the fix)"
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
    // OpenFinder included: the finder must NOT open while the picker is the active modal
    // (modal mutual-exclusion, gate L-3) — handle() routes to handle_picker_intent first.
    for intent in [
        Intent::ToggleIgnore,
        Intent::ToggleChangedOnly,
        Intent::CycleView,
        Intent::ToggleFocus,
        Intent::OpenFinder,
    ] {
        ctrl.handle(intent);
    }

    assert!(
        ctrl.picker().is_some(),
        "picker stays open for inert intents"
    );
    assert!(
        !ctrl.finder_open(),
        "OpenFinder is inert while the picker is open — finder must not open (modal mutual-exclusion)"
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
        // OpenFinder (in Intent::ALL) opens the finder; close it so each intent is exercised from a
        // clean no-modal state — and so it is not left open for Part 2, where the finder's modal
        // guard would otherwise make SwitchWorktree inert. (review-gate R1: O2)
        if ctrl.finder_open() {
            ctrl.handle_finder_key(key(KeyCode::Esc));
        }
        // OpenSearch (in Intent::ALL since T-8) opens the search prompt; close it symmetrically
        // so the prompt modal guard cannot block SwitchWorktree in Part 2.
        if ctrl.prompt_open() {
            ctrl.handle_prompt_key(key(KeyCode::Esc));
        }
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

// ---------------------------------------------------------------------------
// T-6 — Finder keystroke matching + selection navigation (AC-6, AC-7, AC-8)
// ---------------------------------------------------------------------------

use crossterm::event::{KeyCode, KeyEvent};

/// Build a `KeyEvent` with no modifiers.
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Build a `KeyEvent` for a printable char with SHIFT (e.g. uppercase letters).
fn key_shift(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)
}

/// Build a `KeyEvent` with Ctrl held — must be rejected by handle_finder_key.
fn key_ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// Build a temp dir with files at known names and return the dir + controller.
fn finder_dir() -> (TempDir, Controller) {
    let dir = TempDir::new();
    // Three files: two in root, one in a sub-dir — deterministic candidate list.
    std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
    std::fs::write(dir.path().join("beta.rs"), "b").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("gamma.rs"), "c").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.handle(Intent::OpenFinder);
    (dir, ctrl)
}

/// Map `finder_matches()` indices through `finder_candidates()` to get paths.
fn match_paths(ctrl: &Controller) -> Vec<String> {
    let candidates = ctrl.finder_candidates().to_vec();
    ctrl.finder_matches()
        .iter()
        .map(|&i| candidates[i].clone())
        .collect()
}

#[test]
fn finder_matches_empty_before_any_keystroke() {
    // AC-6 / AC-7: when the finder is freshly opened the query is empty and the match list is
    // empty (no results until the user types — match_and_rank returns [] for an empty query).
    let (_dir, ctrl) = finder_dir();
    assert_eq!(ctrl.finder_query(), "", "query starts empty");
    assert!(
        ctrl.finder_matches().is_empty(),
        "no matches until the user types (AC-6 backing)"
    );
    assert_eq!(ctrl.finder_cursor(), 0, "cursor starts at 0");
}

#[test]
fn finder_confirm_zooms_the_file_when_only_the_tree_is_visible() {
    // Live-test fix: in the narrow, tree-only layout the Presenter draws no content column, so the
    // controller's last-observed content viewport is (0, 0). Confirming a finder jump there must
    // open the file in ZOOM mode so the user actually sees the file they jumped to — instead of
    // landing on a tree row with the file hidden off-screen. Mirrors the tree's Enter/activate on a
    // file (content full-screen).
    let (_dir, mut ctrl) = finder_dir();
    ctrl.set_content_viewport(0, 0); // the Presenter drew no content column (tree-only layout)
    assert!(!ctrl.zoomed(), "precondition: not zoomed");

    ctrl.handle_finder_key(key(KeyCode::Char('a'))); // 'a' matches alpha.txt / beta.rs / gamma.rs
    assert!(
        !ctrl.finder_matches().is_empty(),
        "query 'a' matches at least one file"
    );
    ctrl.handle_finder_key(key(KeyCode::Enter));

    assert!(!ctrl.finder_open(), "finder closed on confirm");
    assert!(
        ctrl.zoomed(),
        "content was hidden (tree-only) → the jumped-to file opens in zoom mode"
    );
}

#[test]
fn finder_confirm_does_not_force_zoom_when_content_is_visible() {
    // The complement: when a content column IS on screen (wide two-column layout, content_width > 0),
    // a finder confirm renders the file in place and must NOT force zoom — the user keeps the layout
    // they were in.
    let (_dir, mut ctrl) = finder_dir();
    ctrl.set_content_viewport(60, 20); // the Presenter drew a content column last frame
    assert!(!ctrl.zoomed(), "precondition: not zoomed");

    ctrl.handle_finder_key(key(KeyCode::Char('a')));
    assert!(!ctrl.finder_matches().is_empty());
    ctrl.handle_finder_key(key(KeyCode::Enter));

    assert!(!ctrl.finder_open(), "finder closed on confirm");
    assert!(
        !ctrl.zoomed(),
        "content already visible → finder confirm must not force zoom"
    );
}

#[test]
fn typing_a_char_updates_query_and_matches_and_resets_cursor() {
    // AC-7: a Char keystroke pushes the character, re-runs fuzzy::match_and_rank, and resets
    // the selection to 0 (so a mid-list cursor from a prior query doesn't carry over).
    let (_dir, mut ctrl) = finder_dir();

    // Manually advance the cursor first so we can check it resets.
    let fx = ctrl.handle_finder_key(key(KeyCode::Char('a')));
    assert!(fx.redraw, "a Char key signals a redraw");
    assert_eq!(ctrl.finder_query(), "a", "query appends the char");
    // "alpha.txt" and "gamma.rs" both have 'a'; "beta.rs" does not.
    let paths = match_paths(&ctrl);
    assert!(!paths.is_empty(), "at least one candidate matches 'a'");
    assert!(
        paths.iter().all(|p| p.to_lowercase().contains('a')),
        "every match contains 'a': {paths:?}"
    );
    assert_eq!(ctrl.finder_cursor(), 0, "cursor resets to 0 on a new query");
}

#[test]
fn typing_more_chars_narrows_matches() {
    // AC-7: successive chars narrow the match list (subsequence filter).
    let (_dir, mut ctrl) = finder_dir();

    ctrl.handle_finder_key(key(KeyCode::Char('b')));
    let after_b = match_paths(&ctrl);
    assert!(!after_b.is_empty(), "something matches 'b'");

    ctrl.handle_finder_key(key(KeyCode::Char('e')));
    let after_be = match_paths(&ctrl);
    // "be" as a subsequence only matches "beta.rs".
    assert!(
        after_be.len() <= after_b.len(),
        "adding a char never grows the match list"
    );
    assert_eq!(ctrl.finder_query(), "be", "query is 'be'");
}

#[test]
fn no_match_query_produces_empty_list() {
    // AC-6 (empty when nothing matches): a query with no candidates yields an empty match list,
    // not a panic or a stale result.
    let (_dir, mut ctrl) = finder_dir();

    ctrl.handle_finder_key(key(KeyCode::Char('z')));
    ctrl.handle_finder_key(key(KeyCode::Char('z')));
    ctrl.handle_finder_key(key(KeyCode::Char('z')));
    // None of our files ("alpha.txt", "beta.rs", "sub/gamma.rs") contain "zzz".
    assert_eq!(ctrl.finder_query(), "zzz");
    assert!(
        ctrl.finder_matches().is_empty(),
        "a non-matching query yields an empty match list (AC-6)"
    );
    assert_eq!(ctrl.finder_cursor(), 0, "cursor stays 0 when list is empty");
}

#[test]
fn backspace_shrinks_query_and_rematches() {
    // AC-7: Backspace removes the last character and re-runs match_and_rank.
    let (_dir, mut ctrl) = finder_dir();

    ctrl.handle_finder_key(key(KeyCode::Char('b')));
    ctrl.handle_finder_key(key(KeyCode::Char('e')));
    assert_eq!(ctrl.finder_query(), "be");
    let after_be = ctrl.finder_matches().len();

    let fx = ctrl.handle_finder_key(key(KeyCode::Backspace));
    assert!(fx.redraw, "Backspace signals a redraw");
    assert_eq!(ctrl.finder_query(), "b", "Backspace removes the last char");
    let after_b = ctrl.finder_matches().len();
    assert!(
        after_b >= after_be,
        "removing a char broadens or keeps the match list"
    );
}

#[test]
fn backspace_on_empty_query_is_a_noop() {
    // Backspace with an empty prompt must not panic or produce wrong state.
    let (_dir, mut ctrl) = finder_dir();

    let fx = ctrl.handle_finder_key(key(KeyCode::Backspace));
    assert!(fx.redraw, "Backspace redraws even on an empty query");
    assert_eq!(ctrl.finder_query(), "", "still empty after Backspace");
    assert!(ctrl.finder_matches().is_empty(), "still no matches");
}

#[test]
fn cursor_resets_to_zero_after_every_query_change() {
    // AC-8: every query change (push or Backspace) resets the selection to 0 so the old
    // position (into a now-different list) is never surfaced.
    let (_dir, mut ctrl) = finder_dir();

    // Navigate down, then type — cursor must reset.
    ctrl.handle_finder_key(key(KeyCode::Char('a'))); // match list: ≥1 entry
    ctrl.handle_finder_key(key(KeyCode::Down));
    // Only meaningful if the list had more than one entry; skip the nav assertion.
    ctrl.handle_finder_key(key(KeyCode::Char('l'))); // narrow further
    assert_eq!(
        ctrl.finder_cursor(),
        0,
        "cursor resets to 0 on every query change"
    );
}

#[test]
fn down_and_up_move_the_cursor_within_the_match_list() {
    // AC-8: Down/Up move the selection within the current match list.
    let (_dir, mut ctrl) = finder_dir();

    // 'a' matches at least two files ("alpha.txt" and "sub/gamma.rs").
    ctrl.handle_finder_key(key(KeyCode::Char('a')));
    let count = ctrl.finder_matches().len();
    assert!(
        count >= 2,
        "need ≥2 matches to test navigation; got {count}"
    );

    let fx_down = ctrl.handle_finder_key(key(KeyCode::Down));
    assert!(fx_down.redraw, "Down signals a redraw");
    assert_eq!(ctrl.finder_cursor(), 1, "Down moves the cursor to 1");

    let fx_up = ctrl.handle_finder_key(key(KeyCode::Up));
    assert!(fx_up.redraw, "Up signals a redraw");
    assert_eq!(ctrl.finder_cursor(), 0, "Up returns the cursor to 0");
}

#[test]
fn down_clamps_at_the_last_match() {
    // AC-8: Down is clamped — it never runs past the end of the match list.
    let (_dir, mut ctrl) = finder_dir();

    ctrl.handle_finder_key(key(KeyCode::Char('a')));
    let count = ctrl.finder_matches().len();
    assert!(count >= 1);

    // Press Down more times than there are matches.
    for _ in 0..(count + 5) {
        ctrl.handle_finder_key(key(KeyCode::Down));
    }
    assert_eq!(
        ctrl.finder_cursor(),
        count - 1,
        "Down clamps at the last match (index {})",
        count - 1
    );
}

#[test]
fn up_clamps_at_zero() {
    // AC-8: Up is clamped — it never goes below index 0.
    let (_dir, mut ctrl) = finder_dir();

    ctrl.handle_finder_key(key(KeyCode::Char('a')));

    for _ in 0..10 {
        ctrl.handle_finder_key(key(KeyCode::Up));
    }
    assert_eq!(ctrl.finder_cursor(), 0, "Up clamps at 0");
}

#[test]
fn nav_on_empty_match_list_stays_at_zero() {
    // AC-8 edge-case: Down/Up are inert (cursor stays 0) when the match list is empty.
    let (_dir, mut ctrl) = finder_dir();

    // Empty query → empty matches.
    ctrl.handle_finder_key(key(KeyCode::Down));
    assert_eq!(
        ctrl.finder_cursor(),
        0,
        "Down on empty list → cursor stays 0"
    );
    ctrl.handle_finder_key(key(KeyCode::Up));
    assert_eq!(ctrl.finder_cursor(), 0, "Up on empty list → cursor stays 0");
}

#[test]
fn uppercase_char_with_shift_is_accepted_and_appended() {
    // A Shift+Char (uppercase letter) is a printable keystroke — the modifier check allows SHIFT.
    let (_dir, mut ctrl) = finder_dir();

    let fx = ctrl.handle_finder_key(key_shift('A'));
    assert!(fx.redraw);
    assert_eq!(ctrl.finder_query(), "A", "Shift+A appends 'A' to the query");
}

#[test]
fn ctrl_char_is_rejected_and_does_not_change_state() {
    // A Ctrl+Char must NOT push to the query — it falls through to the noop arm.
    let (_dir, mut ctrl) = finder_dir();

    let fx = ctrl.handle_finder_key(key_ctrl('a'));
    assert!(
        !fx.redraw,
        "Ctrl+Char produces a noop (falls through to _ => Effects::noop())"
    );
    assert_eq!(ctrl.finder_query(), "", "query unchanged after Ctrl+Char");
}

#[test]
fn handle_finder_key_is_noop_when_finder_is_closed() {
    // If the caller accidentally sends a finder key when no finder is open, the controller
    // must produce a noop and not panic.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    assert!(!ctrl.finder_open(), "precondition: finder is closed");

    let fx = ctrl.handle_finder_key(key(KeyCode::Char('a')));
    assert!(
        !fx.redraw && !fx.quit,
        "a finder key with the finder closed is a noop"
    );
}

// ---------------------------------------------------------------------------
// T-5 — OpenFinder intent: open + finder_open (AC-1, AC-18)
// ---------------------------------------------------------------------------

/// AC-1 / AC-18: handle(OpenFinder) opens the finder (finder_open() → true), populates it
/// with the candidates that index::build returns for the root, and leaves the query empty.
#[test]
fn open_finder_opens_finder_with_full_candidate_list_and_empty_query() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
    std::fs::write(dir.path().join("beta.rs"), "b").unwrap();

    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    assert!(!ctrl.finder_open(), "finder starts closed");

    let fx = ctrl.handle(Intent::OpenFinder);
    assert!(fx.redraw, "OpenFinder triggers a redraw");
    assert!(ctrl.finder_open(), "finder_open() is true after OpenFinder");

    // Candidates must equal index::build(root) — same set, order may differ.
    let mut expected = herdr_file_viewer::index::build(dir.path());
    expected.sort();
    let mut got = ctrl.finder_candidates().to_vec();
    got.sort();
    assert_eq!(got, expected, "candidates must equal index::build(root)");

    assert_eq!(
        ctrl.finder_query(),
        "",
        "query is empty when the finder is first opened"
    );
}

// ---------------------------------------------------------------------------
// T-7 — Confirm (reveal + render) · cancel · no-match no-op
// ---------------------------------------------------------------------------

/// Build a temp dir with files and an open finder (git repo variant so changed_only works).
fn finder_dir_git() -> (TempDir, Controller) {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
    std::fs::write(dir.path().join("beta.rs"), "b").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("gamma.rs"), "c").unwrap();
    // Only beta.rs is in the changed-set.
    let mut changed = BTreeMap::new();
    changed.insert(PathBuf::from("beta.rs"), Status::Modified);
    let git = StubGit {
        status: changed.clone(),
        changed,
        ..StubGit::default()
    };
    let (mut ctrl, _, _) = controller(dir.path(), true, git, false);
    ctrl.handle(Intent::OpenFinder);
    (dir, ctrl)
}

#[test]
fn enter_with_match_closes_finder_and_reveals_file_and_redraws() {
    // AC-10, AC-11: Enter on a matched candidate closes the finder, moves the tree cursor to
    // that file, and triggers a redraw (content is dispatched for the new selection).
    let (_dir, mut ctrl) = finder_dir();

    // Type 'b' to match "beta.rs".
    ctrl.handle_finder_key(key(KeyCode::Char('b')));
    let matches = ctrl.finder_matches().to_vec();
    assert!(
        !matches.is_empty(),
        "precondition: at least one match for 'b'"
    );

    let candidates = ctrl.finder_candidates().to_vec();
    let selected_path = candidates[matches[0]].clone();

    let fx = ctrl.handle_finder_key(key(KeyCode::Enter));
    assert!(fx.redraw, "Enter with a match signals a redraw");
    assert!(!ctrl.finder_open(), "finder is closed after Enter (AC-10)");

    // The tree's selected node must be the confirmed file.
    let selected = ctrl
        .tree()
        .selected()
        .expect("a node is selected after reveal");
    let selected_rel = selected
        .path
        .strip_prefix(ctrl.root())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        selected_rel, selected_path,
        "tree cursor points to the confirmed file (AC-11)"
    );
}

#[test]
fn enter_with_zero_matches_keeps_finder_open_and_is_noop() {
    // AC-6: Enter with zero matches must be a no-op — the finder stays open so the user can
    // refine their query rather than being unexpectedly dismissed.
    let (_dir, mut ctrl) = finder_dir();

    // Type a non-matching query.
    ctrl.handle_finder_key(key(KeyCode::Char('z')));
    ctrl.handle_finder_key(key(KeyCode::Char('z')));
    ctrl.handle_finder_key(key(KeyCode::Char('z')));
    assert!(
        ctrl.finder_matches().is_empty(),
        "precondition: no matches for 'zzz'"
    );

    let cursor_before = ctrl.tree().cursor();
    let fx = ctrl.handle_finder_key(key(KeyCode::Enter));
    // Not a redraw: the no-op is completely inert (no state change).
    assert!(!fx.redraw, "Enter with zero matches is a noop (no redraw)");
    assert!(
        ctrl.finder_open(),
        "finder stays open when there are no matches (AC-6)"
    );
    assert_eq!(
        ctrl.tree().cursor(),
        cursor_before,
        "tree cursor is unchanged"
    );
}

#[test]
fn enter_on_missing_target_sets_notice_and_closes_finder() {
    // AC-20: if the selected candidate has been removed from disk since the finder was opened,
    // Enter must close the finder, set a non-fatal notice, and leave the tree selection unchanged.
    let (dir, mut ctrl) = finder_dir();

    // Match "beta.rs", then delete it before confirming.
    ctrl.handle_finder_key(key(KeyCode::Char('b')));
    let matches = ctrl.finder_matches().to_vec();
    assert!(!matches.is_empty(), "precondition: 'b' matches beta.rs");
    // Verify we're going to try to reveal beta.rs.
    let candidate = &ctrl.finder_candidates()[matches[0]];
    assert!(
        candidate.contains("beta"),
        "expect beta.rs to be the match: {candidate}"
    );

    let cursor_before = ctrl.tree().cursor();
    // Delete the file so reveal() returns false.
    std::fs::remove_file(dir.path().join("beta.rs")).unwrap();

    let fx = ctrl.handle_finder_key(key(KeyCode::Enter));
    assert!(
        fx.redraw,
        "Enter on a missing target still redraws (notice)"
    );
    assert!(
        !ctrl.finder_open(),
        "finder is closed even on a failed reveal (AC-20)"
    );
    assert!(
        ctrl.action_notice().is_some(),
        "a non-fatal notice is set when the target is missing (AC-20)"
    );
    assert!(
        ctrl.action_notice().unwrap().contains("beta"),
        "notice names the missing file: {:?}",
        ctrl.action_notice()
    );
    assert_eq!(
        ctrl.tree().cursor(),
        cursor_before,
        "tree selection unchanged when reveal fails"
    );
}

#[test]
fn esc_closes_finder_and_leaves_tree_unchanged() {
    // AC-9: Esc discards the finder without touching the tree selection, root, or content.
    let (_dir, mut ctrl) = finder_dir();

    // Navigate the tree to a known position.
    ctrl.handle(Intent::NavDown);
    let cursor_before = ctrl.tree().cursor();
    // Type something to prove the query is also discarded.
    ctrl.handle_finder_key(key(KeyCode::Char('a')));
    assert!(!ctrl.finder_matches().is_empty(), "precondition");

    let fx = ctrl.handle_finder_key(key(KeyCode::Esc));
    assert!(fx.redraw, "Esc signals a redraw");
    assert!(!ctrl.finder_open(), "finder is closed after Esc (AC-9)");
    assert_eq!(
        ctrl.tree().cursor(),
        cursor_before,
        "tree cursor unchanged after Esc (AC-9)"
    );
}

#[test]
fn enter_with_match_resyncs_changed_only_mirror_after_reveal() {
    // Mirror-resync guard (T-4 review note): when changed_only is ON and the finder navigates
    // to a file that is NOT in the changed-set, reveal() relaxes the tree's changed_only field
    // to false. The controller's mirror must be re-synced — otherwise the next `c` toggle
    // would read the stale mirror and re-apply the wrong filter.
    let (_dir, mut ctrl) = finder_dir_git();
    // finder_dir_git() opens the finder; close it so the changed-only toggle below isn't swallowed
    // by the finder's modal guard (handle() is inert while the finder is open). (review-gate R1: O2)
    ctrl.handle_finder_key(key(KeyCode::Esc));
    assert!(
        !ctrl.finder_open(),
        "finder closed before toggling the filter"
    );

    // Turn on changed-only filter (only beta.rs is in the changed-set).
    ctrl.handle(Intent::ToggleChangedOnly);
    assert!(ctrl.changed_only(), "precondition: changed_only is ON");

    // Open the finder and jump to alpha.txt (not in the changed-set).
    ctrl.handle(Intent::OpenFinder);
    ctrl.handle_finder_key(key(KeyCode::Char('a')));
    // alpha.txt should match.
    let matches = ctrl.finder_matches().to_vec();
    let candidates = ctrl.finder_candidates().to_vec();
    let alpha_idx = matches
        .iter()
        .position(|&i| candidates[i].contains("alpha"))
        .expect("alpha.txt must match 'a'");
    // Navigate to alpha.txt's position in the list.
    for _ in 0..alpha_idx {
        ctrl.handle_finder_key(key(KeyCode::Down));
    }
    // Confirm: reveal() must relax changed_only in the tree AND the controller re-syncs its mirror.
    let fx = ctrl.handle_finder_key(key(KeyCode::Enter));
    assert!(fx.redraw, "Enter redraws");
    assert!(!ctrl.finder_open(), "finder closed");

    // The controller mirror must be false (synced from the tree after reveal relaxed it).
    assert!(
        !ctrl.changed_only(),
        "controller changed_only() mirror is false after reveal relaxed the filter (desync guard)"
    );

    // The tree must actually show alpha.txt (visible in the now-relaxed tree).
    let nodes = ctrl.tree().visible_nodes();
    let has_alpha = nodes
        .iter()
        .any(|n| n.path.file_name().unwrap_or_default() == "alpha.txt");
    assert!(
        has_alpha,
        "alpha.txt is visible in the tree after the filter was relaxed"
    );
}

#[test]
fn enter_with_match_resyncs_hide_hidden_mirror_after_reveal() {
    // Mirror-resync guard (T-4 review note), hide_hidden variant — symmetric to the changed_only
    // case above. When hide_hidden is ON and the finder jumps to a NON-ignored dotfile, reveal()
    // relaxes the tree's hide_hidden field; the controller's mirror must re-sync so the next `.`
    // toggle does not read a stale value. Guards lines 1633-1634 (the hide_hidden re-sync).
    let dir = TempDir::new();
    std::fs::write(dir.path().join(".envrc"), "x").unwrap();
    std::fs::write(dir.path().join("main.rs"), "y").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), true, StubGit::default(), false);

    // Turn on hide-hidden (the tree would hide the dotfile; the finder index still surfaces it).
    ctrl.handle(Intent::ToggleHidden);
    assert!(ctrl.hide_hidden(), "precondition: hide_hidden is ON");

    // Open the finder and jump to the dotfile (query "env" matches ".envrc").
    ctrl.handle(Intent::OpenFinder);
    for c in "env".chars() {
        ctrl.handle_finder_key(key(KeyCode::Char(c)));
    }
    let matches = ctrl.finder_matches().to_vec();
    let candidates = ctrl.finder_candidates().to_vec();
    let envrc_idx = matches
        .iter()
        .position(|&i| candidates[i].contains(".envrc"))
        .expect(".envrc must match 'env'");
    for _ in 0..envrc_idx {
        ctrl.handle_finder_key(key(KeyCode::Down));
    }
    let fx = ctrl.handle_finder_key(key(KeyCode::Enter));
    assert!(fx.redraw, "Enter redraws");
    assert!(!ctrl.finder_open(), "finder closed");

    // The controller mirror must be false (synced from the tree after reveal relaxed it).
    assert!(
        !ctrl.hide_hidden(),
        "controller hide_hidden() mirror is false after reveal relaxed the filter (desync guard)"
    );

    // The tree must actually show .envrc (visible in the now-relaxed tree).
    let nodes = ctrl.tree().visible_nodes();
    let has_envrc = nodes
        .iter()
        .any(|n| n.path.file_name().unwrap_or_default() == ".envrc");
    assert!(
        has_envrc,
        ".envrc is visible in the tree after hide_hidden was relaxed"
    );
}

/// Build a `PaneGeometry` that reflects an open finder with three result rows starting at screen
/// row 12 (after the border + padding + query line): rows_area at x=10,y=12,w=30,h=10,
/// finder_scroll=0. Used by the finder-mouse tests below.
fn finder_geometry_with_rows() -> PaneGeometry {
    PaneGeometry {
        finder_rows: Some(Rect {
            x: 10,
            y: 12,
            width: 30,
            height: 10,
        }),
        finder_scroll: 0,
        ..wide_geometry()
    }
}

#[test]
fn mouse_is_inert_while_the_finder_is_open_outside_overlay() {
    // Rewritten T-9: the finder is mouse-interactive INSIDE its rows area, but clicks/scrolls
    // OUTSIDE (on the tree or content panes beneath the overlay) must never drive those panes.
    // This test checks the "outside/other" branch — which must stay inert — and also asserts
    // that the finder stays open (a click outside does NOT cancel it; Esc cancels).
    let (_dir, mut ctrl) = finder_dir();

    // Give the controller a geometry where `finder_rows` covers rows 12-21, cols 10-39.
    // Any click at (col=6, row=3) is in the tree pane but OUTSIDE the overlay rows.
    ctrl.set_pane_geometry(finder_geometry_with_rows());

    // Precondition: the finder is open and a query is typed so matches exist.
    ctrl.handle_finder_key(key(KeyCode::Char('a'))); // produces matches
    assert!(
        ctrl.finder_open(),
        "finder must be open before the mouse events"
    );
    let tree_cursor_before = ctrl.tree().cursor();

    // A left-click on what would be a tree row (outside the overlay) must be inert.
    let click = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 6, 3));
    assert!(!click.quit, "outside click must not quit");
    assert_eq!(
        ctrl.tree().cursor(),
        tree_cursor_before,
        "the tree cursor must be unchanged: clicks outside overlay must not drive the tree"
    );

    // The finder must still be open — clicking outside the rows area does NOT cancel.
    assert!(
        ctrl.finder_open(),
        "the finder must still be open: outside clicks do not cancel (Esc cancels)"
    );

    // Shift+mouse is also inert (terminal selection) — same guard as the normal mouse path.
    let shift_click = MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 15,
        row: 13,
        modifiers: KeyModifiers::SHIFT,
    };
    let shift_fx = ctrl.handle_mouse(shift_click);
    assert!(!shift_fx.quit, "Shift+click is inert (terminal selection)");

    // A Down event (not Up) inside the overlay is inert (no drag in the finder).
    let down = ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 15, 12));
    assert!(!down.quit, "Down event inside the finder overlay is inert");
}

#[test]
fn finder_wheel_moves_selection() {
    // ScrollDown/ScrollUp while the finder is open moves the finder selection by WHEEL_STEP (3),
    // clamped at both ends. Position-independent (the finder owns all wheel events while open).
    let (_dir, mut ctrl) = finder_dir();
    ctrl.handle_finder_key(key(KeyCode::Char('a'))); // produces ≥1 matches (alpha, beta, gamma)
    let n = ctrl.finder_matches().len();
    assert!(n >= 3, "need ≥3 matches for this test; got {n}");

    ctrl.set_pane_geometry(finder_geometry_with_rows());

    // Starting cursor is 0.
    assert_eq!(ctrl.finder_cursor(), 0, "cursor starts at 0");

    // ScrollDown → moves down by WHEEL_STEP (3) or to the last match if fewer.
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::ScrollDown, 6, 3)); // position: tree area (irrelevant)
    assert!(fx.redraw, "ScrollDown redraws");
    let expected_after_down = 3_usize.min(n - 1);
    assert_eq!(
        ctrl.finder_cursor(),
        expected_after_down,
        "ScrollDown moves the finder selection down by WHEEL_STEP"
    );

    // ScrollUp → moves back up.
    let fx2 = ctrl.handle_mouse(mouse(MouseEventKind::ScrollUp, 50, 5)); // position: content area (irrelevant)
    assert!(fx2.redraw, "ScrollUp redraws");
    assert_eq!(
        ctrl.finder_cursor(),
        0,
        "ScrollUp moves the finder selection back up (clamped at 0)"
    );

    // ScrollUp at the top is a no-op for the cursor (stays at 0) but still redraws.
    let fx3 = ctrl.handle_mouse(mouse(MouseEventKind::ScrollUp, 0, 0));
    assert!(fx3.redraw, "ScrollUp at the top still redraws");
    assert_eq!(ctrl.finder_cursor(), 0, "cursor is clamped at 0");
}

#[test]
fn finder_click_on_row_selects_it() {
    // A left-button Up event on a result row (within finder_rows) sets the finder cursor to
    // that row's index (scroll_offset + (screen_row - rows_area.y)).
    let (_dir, mut ctrl) = finder_dir();
    ctrl.handle_finder_key(key(KeyCode::Char('a'))); // produces matches
    let n = ctrl.finder_matches().len();
    assert!(n >= 2, "need ≥2 matches for this test; got {n}");

    // rows_area starts at row 12, scroll=0 → screen row 12 = index 0, row 13 = index 1.
    ctrl.set_pane_geometry(finder_geometry_with_rows());

    // Click on the SECOND result row (screen row 13, col 15 — inside rows_area).
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 15, 13));
    assert!(fx.redraw, "a click on a result row redraws");
    assert_eq!(
        ctrl.finder_cursor(),
        1,
        "clicking the second result row sets the cursor to index 1"
    );
    assert!(
        ctrl.finder_open(),
        "a single click does not confirm: the finder stays open"
    );

    // Click on the FIRST result row (screen row 12, col 15).
    let fx2 = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 15, 12));
    assert!(fx2.redraw, "clicking the first row redraws");
    assert_eq!(
        ctrl.finder_cursor(),
        0,
        "clicking the first result row sets the cursor to index 0"
    );
    assert!(ctrl.finder_open(), "finder still open after a single click");

    // Click outside the rows_area (below the last row, or to the left of the box) — inert.
    // rows_area is x=10,y=12,w=30,h=10 → row 22 is outside.
    let fx3 = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 15, 22));
    assert!(
        ctrl.finder_open(),
        "click below rows is inert: finder stays open"
    );
    assert_eq!(
        ctrl.finder_cursor(),
        0,
        "cursor unchanged after outside click"
    );
    assert!(!fx3.quit, "outside click does not quit");
}

#[test]
fn finder_double_click_confirms() {
    // A double-click (two Up(Left) events on the same row within DOUBLE_CLICK ms) confirms the
    // finder: the finder closes and the tree reveals that file. Mirrors the tree's double-click
    // behaviour (folder expand/collapse, file zoom), sharing is_double_click and last_click.
    let (_dir, mut ctrl) = finder_dir();
    ctrl.handle_finder_key(key(KeyCode::Char('a'))); // produces matches (alpha.txt, beta.rs, gamma.rs)
    let n = ctrl.finder_matches().len();
    assert!(n >= 1, "need ≥1 match for this test; got {n}");

    ctrl.set_pane_geometry(finder_geometry_with_rows());

    // First click on row 0 (screen row 12) → selects it, finder still open.
    let fx1 = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 15, 12));
    assert!(fx1.redraw, "first click redraws");
    assert!(ctrl.finder_open(), "finder still open after first click");
    assert_eq!(ctrl.finder_cursor(), 0, "cursor on row 0 after first click");

    // Second click on the SAME row within the double-click window → confirms.
    // (We rely on Instant::now() being within DOUBLE_CLICK=400ms between the two calls —
    // guaranteed in a test environment without sleep.)
    let fx2 = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 15, 12));
    assert!(fx2.redraw, "double-click redraws");
    assert!(
        !ctrl.finder_open(),
        "double-click closes the finder (confirm)"
    );

    // The tree should now have a selection pointing to the confirmed file.
    // (We can't assert the exact path without knowing ranking order, but the tree cursor
    // moved — it is no longer necessarily 0 depending on the file layout.)
    // The important assertion: the finder is gone.
    assert!(
        ctrl.finder_cursor() == 0,
        "finder_cursor() returns 0 because the finder is closed (None), not because the cursor was reset"
    );
}

/// Geometry where both `tree_inner` and `finder_rows` share row 12.
/// `tree_inner` starts at y=10 so row 12 = tree node index 2 (a valid node in a 3-node tree).
/// `finder_rows` starts at y=12 so row 12 = finder row index 0.
fn cross_contamination_geometry() -> PaneGeometry {
    PaneGeometry {
        tree_inner: Some(Rect {
            x: 1,
            y: 10,
            width: 38,
            height: 20,
        }),
        finder_rows: Some(Rect {
            x: 10,
            y: 12,
            width: 30,
            height: 10,
        }),
        finder_scroll: 0,
        ..wide_geometry()
    }
}

#[test]
fn last_click_not_shared_between_finder_and_tree_scenario_a() {
    // Scenario A: finder open → click a finder row → Esc closes finder → click tree row at
    // the SAME screen row → must NOT spuriously double-click (no zoom).
    // Without the fix, open_finder() and the Esc branch of handle_finder_key both leave
    // last_click populated, so the tree click pairs with the finder click as a double-click.
    let (_dir, mut ctrl) = finder_dir();
    // Geometry where row 12 is finder row 0 AND tree node index 2 (third node — "sub/").
    ctrl.set_pane_geometry(cross_contamination_geometry());

    // Produce at least one match so finder_rows is non-empty.
    ctrl.handle_finder_key(key(KeyCode::Char('a')));
    assert!(ctrl.finder_open(), "precondition: finder is open");
    assert!(
        !ctrl.finder_matches().is_empty(),
        "precondition: matches exist"
    );

    // Step 1: click finder row 0 at screen row 12 (sets last_click).
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 15, 12));
    assert!(ctrl.finder_open(), "finder still open after single click");

    // Step 2: Esc closes the finder. The fix clears last_click here.
    ctrl.handle_finder_key(key(KeyCode::Esc));
    assert!(!ctrl.finder_open(), "finder closed by Esc");

    // Step 3: click the tree row at the SAME screen row 12 (= tree node index 2).
    // Without the fix this would fire is_double_click → activate() → zoom.
    // With the fix last_click was cleared on Esc, so this is a single click.
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 5, 12));
    assert!(
        !ctrl.zoomed(),
        "tree click after Esc-close must NOT spuriously zoom (last_click cross-contamination)"
    );
}

#[test]
fn last_click_not_shared_between_finder_and_tree_scenario_b() {
    // Scenario B: click a tree row → open finder → click finder row at the SAME screen row →
    // must single-click select (finder stays open), NOT spuriously confirm (double-click).
    // Without the fix, open_finder() leaves last_click populated from the tree click, so the
    // finder click pairs with the tree click as a double-click and closes the finder.
    //
    // Use a fresh controller (not finder_dir which pre-opens the finder) so Step 1 goes through
    // handle_click (the tree path), not handle_finder_mouse.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
    std::fs::write(dir.path().join("beta.rs"), "b").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("gamma.rs"), "c").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.set_pane_geometry(cross_contamination_geometry());

    // Step 1: click tree row at screen row 12 (= tree node index 2 — "sub/").
    // The finder is not yet open so this routes through handle_click.
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 5, 12));
    assert!(
        !ctrl.finder_open(),
        "precondition: finder not open after tree click"
    );

    // Step 2: open the finder. The fix clears last_click here.
    ctrl.handle(Intent::OpenFinder);
    assert!(ctrl.finder_open(), "finder is now open");

    // Produce matches so finder has rows to click.
    ctrl.handle_finder_key(key(KeyCode::Char('a')));
    assert!(
        !ctrl.finder_matches().is_empty(),
        "precondition: matches exist"
    );

    // Step 3: click finder row 0 at the SAME screen row 12.
    // Without the fix is_double_click fires → confirm_finder() closes the finder.
    // With the fix last_click was cleared on open_finder(), so this is a single click.
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 15, 12));
    assert!(
        ctrl.finder_open(),
        "finder must stay open after single click (no spurious confirm from cross-contamination)"
    );
}

#[test]
fn last_click_cleared_by_a_finder_keystroke_scenario_c() {
    // review-gate R1 (O1): a finder click → KEYSTROKE → click on the SAME screen row within the
    // double-click window must NOT be misread as a double-click (confirm). Without the fix, the
    // keystroke arms of handle_finder_key leave `last_click` populated, so the second click pairs
    // with the first as a double-click and opens a file the user only single-clicked — often a
    // DIFFERENT file, since typing changed the match list. (scenario_a/b cover the open/Esc vector;
    // this covers the keystroke/nav vector.)
    let (_dir, mut ctrl) = finder_dir();
    ctrl.set_pane_geometry(finder_geometry_with_rows());

    // Query "a" matches all three files; ranked by path length the row-0 match is "beta.rs".
    ctrl.handle_finder_key(key(KeyCode::Char('a')));
    assert!(
        ctrl.finder_matches().len() >= 2,
        "precondition: 'a' matches multiple files"
    );

    // Step 1: click finder row 0 (screen row 12) → selects it, finder stays open, last_click set.
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 15, 12));
    assert!(
        ctrl.finder_open(),
        "finder open after the first single click"
    );

    // Step 2: a keystroke that narrows the match list ("al" → only "alpha.txt"), so row 0 now maps
    // to a DIFFERENT file than the first click selected. The fix clears last_click here.
    ctrl.handle_finder_key(key(KeyCode::Char('l')));
    assert!(ctrl.finder_open(), "finder still open after the keystroke");
    assert!(
        !ctrl.finder_matches().is_empty(),
        "precondition: 'al' still matches a file at row 0"
    );

    // Step 3: click the SAME screen row again within the double-click window.
    // Without the fix is_double_click fires → confirm_finder() closes the finder (opening alpha.txt
    // even though the user only single-clicked beta.rs then alpha.txt). With the fix the keystroke
    // cleared last_click, so this is a single click.
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 15, 12));
    assert!(
        ctrl.finder_open(),
        "a keystroke between two same-row clicks clears the pending double-click: no spurious confirm"
    );
}

#[test]
fn intents_are_inert_while_the_finder_is_open() {
    // review-gate R1 (O2): handle() is modal for the finder too. While the finder is open every
    // intent is a no-op — the run loop routes keys to handle_finder_key, and this structural guard
    // (symmetric with the picker guard) stops a future/test caller from leaking an intent to the
    // tree beneath the overlay or opening a SECOND modal over it.
    let (_dir, mut ctrl) = finder_dir();
    ctrl.handle_finder_key(key(KeyCode::Char('a'))); // query "a", matches present
    assert_eq!(ctrl.finder_query(), "a", "precondition: query is 'a'");

    for intent in [
        Intent::NavDown,
        Intent::Activate,
        Intent::ToggleHidden,
        Intent::SwitchWorktree, // must NOT open a second modal
        Intent::OpenFinder,     // must NOT rebuild/reset the finder
    ] {
        let fx = ctrl.handle(intent);
        assert!(
            !fx.redraw && !fx.quit,
            "intent {intent:?} is inert (noop) while the finder is open"
        );
        assert!(
            ctrl.finder_open(),
            "the finder stays open through intent {intent:?}"
        );
        assert!(
            ctrl.picker().is_none(),
            "no second modal (picker) opened by intent {intent:?}"
        );
    }
    assert_eq!(
        ctrl.finder_query(),
        "a",
        "the query is untouched — OpenFinder did not reset the finder, no intent leaked"
    );
}

#[test]
fn q_is_a_literal_query_char_in_the_finder_not_a_cancel_key() {
    // AC-9: cancel is Esc ONLY — `q` is a literal query character (the resolved Esc-only decision,
    // contrast the global `q` = Close binding). Typing `q` while the finder is open must append it
    // to the query and leave the finder OPEN; only Esc closes it.
    let (_dir, mut ctrl) = finder_dir();

    let fx = ctrl.handle_finder_key(key(KeyCode::Char('q')));
    assert!(fx.redraw, "typing 'q' redraws (it edited the query)");
    assert_eq!(
        ctrl.finder_query(),
        "q",
        "'q' is appended as a literal query char"
    );
    assert!(
        ctrl.finder_open(),
        "'q' must NOT close the finder — only Esc cancels (AC-9)"
    );

    // A second 'q' keeps building the query, still no cancel.
    ctrl.handle_finder_key(key(KeyCode::Char('q')));
    assert_eq!(ctrl.finder_query(), "qq", "'q' keeps appending");
    assert!(ctrl.finder_open(), "still open after another 'q'");

    // Esc — and only Esc — closes it.
    ctrl.handle_finder_key(key(KeyCode::Esc));
    assert!(!ctrl.finder_open(), "Esc closes the finder (AC-9)");
}

// ---------------------------------------------------------------------------
// Finder hscroll — Left/Right keys + horizontal wheel + recompute reset
// ---------------------------------------------------------------------------

#[test]
fn finder_right_key_increments_hscroll_and_left_decrements_it() {
    // Left/Right arrow keys scroll the result rows horizontally (saturating), exactly as the
    // worktree picker uses ←/→. The prompt is append-only so the arrows are free.
    let (_dir, mut ctrl) = finder_dir();

    assert_eq!(ctrl.finder_hscroll(), 0, "hscroll starts at 0");

    let fx = ctrl.handle_finder_key(key(KeyCode::Right));
    assert!(fx.redraw, "Right redraws");
    let after_right = ctrl.finder_hscroll();
    assert!(after_right > 0, "Right increments hscroll");

    let fx2 = ctrl.handle_finder_key(key(KeyCode::Right));
    assert!(fx2.redraw, "Right again redraws");
    assert!(
        ctrl.finder_hscroll() > after_right,
        "Right again increments hscroll further"
    );

    let fx3 = ctrl.handle_finder_key(key(KeyCode::Left));
    assert!(fx3.redraw, "Left redraws");
    assert_eq!(
        ctrl.finder_hscroll(),
        after_right,
        "Left decrements hscroll back by one step"
    );
}

#[test]
fn finder_left_at_zero_does_not_underflow() {
    // Left at hscroll=0 is saturating — it stays at 0, never wraps.
    let (_dir, mut ctrl) = finder_dir();

    assert_eq!(ctrl.finder_hscroll(), 0, "precondition: hscroll is 0");
    let fx = ctrl.handle_finder_key(key(KeyCode::Left));
    assert!(fx.redraw, "Left at 0 still redraws");
    assert_eq!(
        ctrl.finder_hscroll(),
        0,
        "Left at 0 stays at 0 (saturating)"
    );
}

#[test]
fn finder_horizontal_wheel_scrolls_right_and_left() {
    // ScrollRight/ScrollLeft (horizontal wheel) scroll the result rows sideways — additive to
    // the keyboard ←/→ (AC-18 keyboard-first; mouse is additive).
    let (_dir, mut ctrl) = finder_dir();
    ctrl.set_pane_geometry(finder_geometry_with_rows());

    assert_eq!(ctrl.finder_hscroll(), 0, "hscroll starts at 0");

    let fx = ctrl.handle_mouse(mouse(MouseEventKind::ScrollRight, 20, 14));
    assert!(fx.redraw, "ScrollRight redraws");
    let after_right = ctrl.finder_hscroll();
    assert!(after_right > 0, "ScrollRight increments hscroll");

    let fx2 = ctrl.handle_mouse(mouse(MouseEventKind::ScrollLeft, 20, 14));
    assert!(fx2.redraw, "ScrollLeft redraws");
    assert_eq!(
        ctrl.finder_hscroll(),
        0,
        "ScrollLeft decrements hscroll back to 0"
    );
}

#[test]
fn finder_hscroll_does_not_overshoot_past_the_measured_max() {
    // Live-test fix: `scroll_right` is monotonic (it can't know the row widths), so over-scrolling
    // right used to park `hscroll` past the real maximum; the first few left presses then appeared
    // to do nothing while the overshoot burned back down. The Presenter now feeds back
    // `finder_max_hscroll` and `set_pane_geometry` clamps the stored offset to it each frame (the
    // same pattern `content_hscroll` uses), so a single left press always moves the view.
    let (_dir, mut ctrl) = finder_dir();
    ctrl.handle_finder_key(key(KeyCode::Char('a'))); // produce match rows
    // Geometry the Presenter would feed back: the widest row needs at most 8 columns of h-scroll.
    let geom = PaneGeometry {
        finder_max_hscroll: 8,
        ..finder_geometry_with_rows()
    };

    // Over-scroll right well past the max (3 monotonic steps).
    for _ in 0..3 {
        ctrl.handle_finder_key(key(KeyCode::Right));
    }
    assert!(
        ctrl.finder_hscroll() > 8,
        "precondition: raw scroll_right overshoots the max when unclamped in isolation"
    );

    // The run loop feeds the measured geometry back after the draw → the stored offset is clamped.
    ctrl.set_pane_geometry(geom);
    assert_eq!(
        ctrl.finder_hscroll(),
        8,
        "geometry feedback clamps the stored hscroll down to the measured maximum"
    );

    // A SINGLE left press now visibly moves the view — no overshoot left to burn down first.
    ctrl.handle_finder_key(key(KeyCode::Left));
    assert!(
        ctrl.finder_hscroll() < 8,
        "one Left press moves immediately after the clamp (the bug was: it needed several)"
    );
}

#[test]
fn finder_scrollbar_is_click_draggable() {
    // Live-test fix: the finder's vertical scrollbar must be click-draggable like the tree/content
    // bars. A press on the track jumps the selection to that fractional position, a drag continues
    // it (the window follows the cursor, so the list scrolls), and the release ends the drag —
    // it must NOT be treated as a row click / confirm.
    let (_dir, mut ctrl) = finder_dir();
    ctrl.handle_finder_key(key(KeyCode::Char('a'))); // matches alpha.txt, beta.rs, sub/gamma.rs
    let total = ctrl.finder_matches().len();
    assert!(
        total >= 3,
        "need >=3 matches for a meaningful scrollbar range; got {total}"
    );

    // A geometry whose finder vbar track spans rows 12..=21 (height 10) — as the Presenter would
    // feed back when the rows overflow.
    let geom = PaneGeometry {
        finder_vbar: Some(Rect {
            x: 40,
            y: 12,
            width: 1,
            height: 10,
        }),
        ..finder_geometry_with_rows()
    };
    ctrl.set_pane_geometry(geom);

    // Press at the BOTTOM of the track → selection jumps to the last match.
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 40, 21));
    assert!(fx.redraw, "a scrollbar press redraws");
    assert_eq!(
        ctrl.finder_cursor(),
        total - 1,
        "press at the track bottom selects the last match"
    );
    assert!(
        ctrl.finder_open(),
        "the finder stays open — a scrollbar press is not a confirm"
    );

    // Drag to the TOP of the track → selection jumps to the first match.
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 40, 12));
    assert_eq!(
        ctrl.finder_cursor(),
        0,
        "dragging to the track top selects the first match"
    );

    // Release ends the drag without confirming.
    let up = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 40, 12));
    assert!(!up.redraw, "the drag-release is inert (not a row click)");
    assert!(
        ctrl.finder_open(),
        "release ends the drag; the finder stays open"
    );
}

#[test]
fn finder_hscroll_resets_to_zero_on_new_query() {
    // Typing a new character (recompute) resets hscroll to 0 so the fresh result list starts
    // unscrolled — the same pattern as cursor resetting to 0 on every query change.
    let (_dir, mut ctrl) = finder_dir();

    // Scroll right first.
    ctrl.handle_finder_key(key(KeyCode::Right));
    assert!(ctrl.finder_hscroll() > 0, "precondition: hscroll is set");

    // Typing a character calls recompute() which resets hscroll.
    ctrl.handle_finder_key(key(KeyCode::Char('a')));
    assert_eq!(
        ctrl.finder_hscroll(),
        0,
        "hscroll resets to 0 when a new query character is typed"
    );

    // Same for Backspace.
    ctrl.handle_finder_key(key(KeyCode::Right));
    assert!(ctrl.finder_hscroll() > 0, "precondition: hscroll set again");
    ctrl.handle_finder_key(key(KeyCode::Backspace));
    assert_eq!(
        ctrl.finder_hscroll(),
        0,
        "hscroll resets to 0 on Backspace (recompute)"
    );
}

// ---------------------------------------------------------------------------
// T-10 — Scope independence + non-git (AC-16, AC-17, AC-19)
// ---------------------------------------------------------------------------

#[test]
fn finder_candidates_are_independent_of_changed_only_filter() {
    // AC-16: the finder's candidate set is the full index::build walk — a separate walk from the
    // tree (ADR-0005). Turning `changed_only` ON restricts the TREE view to the changed-set, but
    // the finder candidates must remain the complete file index, unchanged.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
    std::fs::write(dir.path().join("beta.rs"), "b").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("gamma.rs"), "c").unwrap();

    // Only beta.rs is in the changed-set; changed_only would restrict the tree to beta.rs only.
    let mut changed = BTreeMap::new();
    changed.insert(PathBuf::from("beta.rs"), Status::Modified);
    let git = StubGit {
        status: changed.clone(),
        changed,
        ..StubGit::default()
    };
    let (mut ctrl, _, _) = controller(dir.path(), true, git, false);

    // Enable changed_only — the tree now shows only beta.rs.
    ctrl.handle(Intent::ToggleChangedOnly);
    assert!(ctrl.changed_only(), "precondition: changed_only is ON");

    // Open the finder — it must walk the full index regardless of the tree filter.
    ctrl.handle(Intent::OpenFinder);
    assert!(ctrl.finder_open(), "finder opened");

    let mut got = ctrl.finder_candidates().to_vec();
    got.sort();
    let mut expected = herdr_file_viewer::index::build(dir.path());
    expected.sort();

    assert_eq!(
        got, expected,
        "finder candidates must equal index::build(root), unaffected by changed_only (AC-16)"
    );
    // Sanity: there are more candidates than just the changed file.
    assert!(
        got.len() > 1,
        "the full index has more entries than the changed-set alone: {got:?}"
    );
}

#[test]
fn finder_candidates_include_dotfiles_even_with_hide_hidden_on() {
    // AC-17: the finder's candidate set comes from index::build, which always includes dotfiles
    // (hidden(false) in WalkBuilder). Toggling `hide_hidden` ON hides dotfiles in the TREE
    // view but must not affect the finder candidates. A non-ignored dotfile (e.g. `.env.example`)
    // must still appear in finder_candidates() after ToggleHidden.
    let dir = TempDir::new();
    std::fs::write(dir.path().join(".env.example"), "SECRET=x").unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    // Turn on hide_hidden — the tree would hide .env.example.
    ctrl.handle(Intent::ToggleHidden);
    assert!(ctrl.hide_hidden(), "precondition: hide_hidden is ON");

    // Open the finder.
    ctrl.handle(Intent::OpenFinder);
    assert!(ctrl.finder_open(), "finder opened");

    let candidates = ctrl.finder_candidates().to_vec();

    // The dotfile must be in the candidate list.
    assert!(
        candidates.iter().any(|p| p.contains(".env.example")),
        ".env.example must be a candidate even with hide_hidden ON (AC-17): {candidates:?}"
    );

    // Cross-check against index::build — the sets must be identical.
    let mut got = candidates.clone();
    got.sort();
    let mut expected = herdr_file_viewer::index::build(dir.path());
    expected.sort();
    assert_eq!(
        got, expected,
        "finder candidates must equal index::build(root), unaffected by hide_hidden (AC-17)"
    );
}

#[test]
fn finder_works_fully_in_a_non_git_directory() {
    // AC-19: the finder must open, list candidates, match a typed query, and jump (Enter → reveal)
    // in a directory that is NOT a git repository. The controller is built with is_git_repo=false
    // (as non-git roots are constructed throughout this test file), which means index::build uses
    // require_git(false) and all git intents are inert — but the finder must be fully operational.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("readme.txt"), "hello").unwrap();
    std::fs::write(dir.path().join("config.toml"), "[foo]").unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src").join("main.rs"), "fn main() {}").unwrap();

    // Non-git root: is_git_repo = false.
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    // 1. Open the finder — must not panic or fail.
    let fx = ctrl.handle(Intent::OpenFinder);
    assert!(fx.redraw, "OpenFinder redraws");
    assert!(
        ctrl.finder_open(),
        "finder is open in a non-git root (AC-19)"
    );

    // 2. Candidate list must be non-empty and equal to index::build(root).
    let mut got = ctrl.finder_candidates().to_vec();
    got.sort();
    let mut expected = herdr_file_viewer::index::build(dir.path());
    expected.sort();
    assert!(!got.is_empty(), "non-git root has files to list (AC-19)");
    assert_eq!(
        got, expected,
        "finder candidates match index::build in a non-git root (AC-19)"
    );

    // 3. Type a query that matches a known file — "main" matches "src/main.rs".
    for c in "main".chars() {
        ctrl.handle_finder_key(key(KeyCode::Char(c)));
    }
    let matches = ctrl.finder_matches().to_vec();
    let candidates = ctrl.finder_candidates().to_vec();
    assert!(
        !matches.is_empty(),
        "typing 'main' must produce at least one match in the non-git root: {candidates:?}"
    );
    let matched_path = &candidates[matches[0]];
    assert!(
        matched_path.contains("main"),
        "the top match must contain 'main': {matched_path}"
    );

    // 4. Press Enter — the finder must close and the tree selection must land on the matched file
    //    (reveal + render without git). AC-19: jump works without git.
    let fx = ctrl.handle_finder_key(key(KeyCode::Enter));
    assert!(fx.redraw, "Enter signals a redraw (AC-19)");
    assert!(
        !ctrl.finder_open(),
        "finder closed after Enter in a non-git root (AC-19)"
    );

    let selected = ctrl
        .tree()
        .selected()
        .expect("a node is selected after reveal in a non-git root");
    let selected_rel = selected
        .path
        .strip_prefix(ctrl.root())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        selected_rel, *matched_path,
        "the tree cursor points to the jumped-to file in a non-git root (AC-19)"
    );
}

// ---------------------------------------------------------------------------
// T-12 — Negative criteria & conformance (AC-N1, AC-N2, AC-N4, AC-N5, AC-N6)
// ---------------------------------------------------------------------------

/// Snapshot every non-.git file under `root` as (relative path, contents).
/// Excludes the .git directory so git-internal ref changes do not interfere
/// with the read-only assertion (AC-N2 uses `git status --porcelain` for that).
fn snapshot_no_git(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        entries.sort();
        for p in entries {
            // Skip the .git directory — git internals change on every query.
            if p.file_name().map(|n| n == ".git").unwrap_or(false) {
                continue;
            }
            let rel = p.strip_prefix(root).unwrap().to_path_buf();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    walk(root, root, &mut out);
    out
}

#[test]
fn ac_n1_finder_enter_journey_leaves_filesystem_unchanged() {
    // AC-N1: the viewer is read-only — a full finder exercise (open → type → Enter-jump)
    // must not create, rename, move, or delete any file.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
    std::fs::write(dir.path().join("beta.rs"), "b").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("gamma.rs"), "c").unwrap();

    let before = snapshot_no_git(dir.path());

    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    // Open the finder.
    ctrl.handle(Intent::OpenFinder);
    // Type a query ('b' matches "beta.rs").
    ctrl.handle_finder_key(key(KeyCode::Char('b')));
    // Confirm with Enter (reveal + render).
    ctrl.handle_finder_key(key(KeyCode::Enter));

    let after = snapshot_no_git(dir.path());
    assert_eq!(
        after, before,
        "AC-N1: the filesystem must be unchanged after open→type→Enter-jump"
    );
}

#[test]
fn ac_n1_finder_esc_journey_leaves_filesystem_unchanged() {
    // AC-N1: Esc-cancel must also leave the filesystem completely unchanged.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
    std::fs::write(dir.path().join("beta.rs"), "b").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("gamma.rs"), "c").unwrap();

    let before = snapshot_no_git(dir.path());

    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.handle(Intent::OpenFinder);
    ctrl.handle_finder_key(key(KeyCode::Char('a')));
    ctrl.handle_finder_key(key(KeyCode::Esc));

    let after = snapshot_no_git(dir.path());
    assert_eq!(
        after, before,
        "AC-N1: the filesystem must be unchanged after open→type→Esc-cancel"
    );
}

#[test]
fn ac_n2_finder_exercise_does_not_mutate_git_state() {
    // AC-N2: no git mutation — git status --porcelain and HEAD must be unchanged after a full
    // finder exercise (open → type → Enter-jump) in a git repository.
    let dir = TempDir::new();
    common::init_repo_with_commit(dir.path());
    std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
    std::fs::write(dir.path().join("lib.rs"), "pub fn lib() {}").unwrap();

    let status_before = common::git(dir.path(), &["status", "--porcelain"]);
    let head_before = common::git(dir.path(), &["rev-parse", "HEAD"]);

    let (mut ctrl, _, _) = controller(dir.path(), true, StubGit::default(), false);
    ctrl.handle(Intent::OpenFinder);
    ctrl.handle_finder_key(key(KeyCode::Char('m'))); // matches "main.rs"
    ctrl.handle_finder_key(key(KeyCode::Enter));

    let status_after = common::git(dir.path(), &["status", "--porcelain"]);
    let head_after = common::git(dir.path(), &["rev-parse", "HEAD"]);

    assert_eq!(
        status_after, status_before,
        "AC-N2: git status --porcelain must be unchanged after the finder exercise"
    );
    assert_eq!(
        head_after, head_before,
        "AC-N2: HEAD commit must be unchanged after the finder exercise"
    );
}

#[test]
fn ac_n4_fresh_controller_rebuilds_candidates_from_disk_with_no_persistent_state() {
    // AC-N4: the finder writes no state to disk. A second, fresh Controller over the same root
    // must produce the same candidate set as index::build(root), and the filesystem must be
    // unchanged (no cache file created by the first controller's use of the finder).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
    std::fs::write(dir.path().join("beta.rs"), "b").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("gamma.rs"), "c").unwrap();

    // First controller: open finder, type, confirm.
    {
        let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
        ctrl.handle(Intent::OpenFinder);
        ctrl.handle_finder_key(key(KeyCode::Char('b')));
        ctrl.handle_finder_key(key(KeyCode::Enter));
        assert!(!ctrl.finder_open(), "finder closed after Enter");
    }

    // No new file must have appeared under root from the first session.
    let files_after: Vec<_> = snapshot_no_git(dir.path())
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    let expected_files: Vec<PathBuf> = {
        let mut v = vec![
            PathBuf::from("alpha.txt"),
            PathBuf::from("beta.rs"),
            PathBuf::from("sub/gamma.rs"),
        ];
        v.sort();
        v
    };
    let mut actual_sorted = files_after.clone();
    actual_sorted.sort();
    assert_eq!(
        actual_sorted, expected_files,
        "AC-N4: no new file must appear under root from using the finder"
    );

    // Second, fresh controller: candidates must match index::build(root).
    let (mut ctrl2, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl2.handle(Intent::OpenFinder);
    assert!(ctrl2.finder_open(), "fresh controller opened the finder");

    let mut got = ctrl2.finder_candidates().to_vec();
    got.sort();
    let mut expected_candidates = herdr_file_viewer::index::build(dir.path());
    expected_candidates.sort();

    assert_eq!(
        got, expected_candidates,
        "AC-N4: a fresh Controller must rebuild candidates from disk (no persisted state)"
    );
}

#[test]
fn ac_18_same_controller_reopen_sees_created_and_dropped_files() {
    // AC-18: the candidate index is rebuilt each time the finder OPENS. Existing coverage
    // (index::build sees a new file; a fresh Controller rebuilds) left the same-controller
    // close→mutate→reopen flow and the REMOVED-file half untested. This drives that vector:
    // open → Esc → create one file + remove another on disk → reopen → the new file is present
    // and the removed file is absent. (review-gate R1: LS4)
    let dir = TempDir::new();
    std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
    std::fs::write(dir.path().join("beta.rs"), "b").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    ctrl.handle(Intent::OpenFinder);
    assert!(
        ctrl.finder_candidates().iter().any(|c| c == "alpha.txt"),
        "first session: alpha.txt is a candidate"
    );
    ctrl.handle_finder_key(key(KeyCode::Esc));
    assert!(
        !ctrl.finder_open(),
        "finder closed before the filesystem mutation"
    );

    // Mutate the filesystem between sessions: add one file, remove another.
    std::fs::write(dir.path().join("delta.md"), "d").unwrap();
    std::fs::remove_file(dir.path().join("alpha.txt")).unwrap();

    // Reopen the SAME controller → the index is rebuilt from disk (AC-18).
    ctrl.handle(Intent::OpenFinder);
    let candidates = ctrl.finder_candidates().to_vec();
    assert!(
        candidates.iter().any(|c| c == "delta.md"),
        "reopen sees the file created since the previous session"
    );
    assert!(
        !candidates.iter().any(|c| c == "alpha.txt"),
        "reopen no longer lists the file removed since the previous session"
    );
}

#[test]
fn ac_n3_finder_ignores_file_contents_matches_path_only() {
    // AC-N3: the finder matches by PATH/NAME only, never file CONTENTS. A token that appears
    // inside a file's bytes but is not a subsequence of any path must yield zero matches. The
    // fuzzy-level test only covered a token in neither path nor content; this drives the full
    // index→matcher pipeline to prove content is never read. (review-gate R1: LS5)
    let dir = TempDir::new();
    // The token "zqxhiddentoken" lives ONLY inside the file's CONTENTS — its leading 'z' is in no path.
    std::fs::write(
        dir.path().join("notes.txt"),
        "zqxhiddentoken appears only in here",
    )
    .unwrap();
    std::fs::write(dir.path().join("readme.md"), "nothing special").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    ctrl.handle(Intent::OpenFinder);
    for c in "zqxhiddentoken".chars() {
        ctrl.handle_finder_key(key(KeyCode::Char(c)));
    }
    assert!(
        ctrl.finder_matches().is_empty(),
        "a token found only inside file contents must yield NO finder matches (AC-N3)"
    );

    // Sanity: a token that IS in a path matches — proving the empty result above was
    // content-blindness, not a dead finder.
    for _ in 0.."zqxhiddentoken".len() {
        ctrl.handle_finder_key(key(KeyCode::Backspace));
    }
    for c in "notes".chars() {
        ctrl.handle_finder_key(key(KeyCode::Char(c)));
    }
    assert!(
        !ctrl.finder_matches().is_empty(),
        "sanity: a path token ('notes') matches, so the empty result above was content-blindness"
    );
}

#[test]
fn ac_n5_every_candidate_is_relative_and_under_root() {
    // AC-N5: every path returned by finder_candidates() is a root-relative string —
    // no leading `/`, no `..`, and every absolute resolution lands under root.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("beta.rs"), "b").unwrap();
    std::fs::create_dir(dir.path().join("sub").join("deep")).unwrap();
    std::fs::write(dir.path().join("sub").join("deep").join("gamma.rs"), "c").unwrap();

    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.handle(Intent::OpenFinder);
    assert!(ctrl.finder_open());

    let root = ctrl.root().to_path_buf();
    let candidates = ctrl.finder_candidates().to_vec();
    assert!(
        !candidates.is_empty(),
        "precondition: at least one candidate"
    );

    for candidate in &candidates {
        // Must not be absolute (no leading /).
        assert!(
            !candidate.starts_with('/'),
            "AC-N5: candidate must not be absolute: {candidate:?}"
        );
        // Must not traverse out of root with '..'.
        assert!(
            !candidate.contains(".."),
            "AC-N5: candidate must not contain '..': {candidate:?}"
        );
        // Absolute resolution must land under root.
        let abs = root.join(candidate);
        assert!(
            abs.starts_with(&root),
            "AC-N5: absolute resolution of {candidate:?} must be under root {root:?}"
        );
        assert!(
            abs.exists(),
            "AC-N5: resolved path must exist on disk: {abs:?}"
        );
    }
}

#[test]
fn ac_n5_reveal_target_resolves_under_root() {
    // AC-N5 (controller-level): after Enter-confirm, the tree's selected node path is under root.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("beta.rs"), "b").unwrap();

    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    let root = ctrl.root().to_path_buf();

    ctrl.handle(Intent::OpenFinder);
    ctrl.handle_finder_key(key(KeyCode::Char('b'))); // matches "sub/beta.rs"
    ctrl.handle_finder_key(key(KeyCode::Enter));
    assert!(!ctrl.finder_open(), "finder closed after Enter");

    let selected = ctrl
        .tree()
        .selected()
        .expect("a node is selected after reveal");
    assert!(
        selected.path.starts_with(&root),
        "AC-N5: reveal target {:?} must be under root {:?}",
        selected.path,
        root
    );
}

#[test]
fn ac_n6_only_open_finder_intent_opens_the_finder() {
    // AC-N6: the finder opens ONLY via Intent::OpenFinder. No other intent in Intent::ALL
    // opens the finder overlay when it is called on a controller where the finder is closed.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "fn main() {}").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("b.txt"), "x").unwrap();

    for intent in Intent::ALL {
        if intent == Intent::OpenFinder || intent == Intent::Close {
            continue; // OpenFinder is the one that should open it; Close ends the session
        }
        // Fresh controller for each intent to avoid accumulated state.
        let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
        assert!(!ctrl.finder_open(), "precondition: finder starts closed");

        let _ = ctrl.handle(intent);

        assert!(
            !ctrl.finder_open(),
            "AC-N6: handling {:?} must NOT open the finder (only Intent::OpenFinder may)",
            intent
        );
    }
}

#[test]
fn ac_n6_open_finder_intent_does_open_the_finder() {
    // AC-N6 (positive side): Intent::OpenFinder is the one and only intent that opens the finder.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    assert!(!ctrl.finder_open(), "finder starts closed");
    ctrl.handle(Intent::OpenFinder);
    assert!(
        ctrl.finder_open(),
        "AC-N6: Intent::OpenFinder must open the finder"
    );
}

#[test]
fn open_go_to_line_opens_the_prompt_in_a_source_mapped_view() {
    // AC-1: in a source-mapped (SyntaxContent) view, OpenGoToLine opens the prompt.
    // An unchanged, non-markdown .rs file renders as SyntaxContent (the policy default).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::SyntaxContent),
        "precondition: an unchanged .rs file is in SyntaxContent"
    );
    assert!(!ctrl.prompt_open(), "prompt starts closed");

    let fx = ctrl.handle(Intent::OpenGoToLine);
    assert!(
        ctrl.prompt_open(),
        "AC-1: prompt opens in a source-mapped view"
    );
    assert!(fx.redraw, "opening the prompt signals a redraw");
    assert_eq!(ctrl.content_scroll(), 0, "content scroll unchanged");
    assert!(
        ctrl.action_notice().is_none(),
        "no unavailable notice in a source-mapped view"
    );
}

#[test]
fn open_go_to_line_opens_the_prompt_in_transformed_views_too() {
    // AC-7 (revised): `:` opens the prompt whenever a FILE is selected — in a transformed view
    // (RenderedMarkdown, Diff, FullDiff) too. No "unavailable" notice; the view is NOT switched yet
    // (the switch happens on confirm — see the jump test below). Covers gate finding L2 (FullDiff).

    // --- RenderedMarkdown ---
    {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("notes.md"), "# Hello\n").unwrap();
        let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

        assert_eq!(
            ctrl.selected_view_mode(),
            Some(ViewMode::RenderedMarkdown),
            "precondition: a .md file is in RenderedMarkdown"
        );
        ctrl.handle(Intent::OpenGoToLine);
        assert!(
            ctrl.prompt_open(),
            "AC-7: `:` opens the prompt in RenderedMarkdown"
        );
        assert!(
            ctrl.action_notice().is_none(),
            "no unavailable notice — the prompt opens"
        );
        assert_eq!(
            ctrl.selected_view_mode(),
            Some(ViewMode::RenderedMarkdown),
            "the view is unchanged until confirm"
        );
        assert_eq!(ctrl.content_scroll(), 0, "scroll unchanged on open");
    }

    // --- Diff ---
    {
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
            "precondition: a changed .rs file is in Diff"
        );
        ctrl.handle(Intent::OpenGoToLine);
        assert!(ctrl.prompt_open(), "AC-7: `:` opens the prompt in Diff");
        assert!(
            ctrl.action_notice().is_none(),
            "no unavailable notice in Diff"
        );
        assert_eq!(ctrl.content_scroll(), 0, "scroll unchanged on open");
    }

    // --- FullDiff (gate finding L2) ---
    {
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

        ctrl.handle(Intent::CycleView); // Diff → FullDiff
        assert_eq!(
            ctrl.selected_view_mode(),
            Some(ViewMode::FullDiff),
            "precondition: one CycleView reaches FullDiff"
        );
        ctrl.handle(Intent::OpenGoToLine);
        assert!(
            ctrl.prompt_open(),
            "AC-7 / gate L2: `:` opens the prompt in FullDiff"
        );
        assert!(
            ctrl.action_notice().is_none(),
            "no unavailable notice in FullDiff"
        );
    }
}

// T-3 — Go-to-line keystroke + confirm + cancel (AC-2..AC-6, AC-7 edge)
// ---------------------------------------------------------------------------

#[test]
fn go_to_line_builds_the_number_from_digits_and_ignores_non_digits() {
    // AC-2: digit keys push to the buffer; non-digit printables are silently ignored.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_lines(dir.path(), 50);
    await_marker(&mut ctrl, "L0");
    ctrl.set_content_viewport(40, 10);
    ctrl.handle(Intent::OpenGoToLine);
    assert!(ctrl.prompt_open());

    ctrl.handle_prompt_key(key(KeyCode::Char('4')));
    ctrl.handle_prompt_key(key(KeyCode::Char('a'))); // non-digit: ignored
    ctrl.handle_prompt_key(key(KeyCode::Char('2')));
    assert_eq!(
        ctrl.prompt_query(),
        "42",
        "digits build the number, non-digits ignored"
    );
    // Backspace deletes the last digit
    ctrl.handle_prompt_key(key(KeyCode::Backspace));
    assert_eq!(ctrl.prompt_query(), "4");
}

#[test]
fn go_to_line_enter_jumps_to_the_line_and_clamps_out_of_range() {
    // AC-3: Enter with a valid line number scrolls the content to that line (near the top).
    // AC-4: a line number beyond the last line is clamped to the last screenful.
    // 50 lines, viewport 10 → max_content_scroll = 40.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_lines(dir.path(), 50);
    await_marker(&mut ctrl, "L0");
    ctrl.set_content_viewport(40, 10);

    // Jump to line 25 (1-based): content_scroll should be 24 (line 25 near top).
    ctrl.handle(Intent::OpenGoToLine);
    assert!(ctrl.prompt_open());
    ctrl.handle_prompt_key(key(KeyCode::Char('2')));
    ctrl.handle_prompt_key(key(KeyCode::Char('5')));
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert!(!ctrl.prompt_open(), "Enter closes the prompt");
    assert_eq!(
        ctrl.content_scroll(),
        24,
        "line 25 lands at offset 24 (near top, AC-3)"
    );

    // Re-open and type "1000" — beyond the last line → clamped to max_content_scroll (40, AC-4).
    ctrl.handle(Intent::OpenGoToLine);
    assert!(ctrl.prompt_open());
    for c in "1000".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert!(
        !ctrl.prompt_open(),
        "Enter closes the prompt after out-of-range"
    );
    assert_eq!(
        ctrl.content_scroll(),
        40,
        "out-of-range clamps to last screenful (AC-4)"
    );
}

#[test]
fn go_to_line_empty_enter_closes_without_jumping() {
    // AC-5: Enter with an empty buffer closes the prompt and leaves the scroll unchanged.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_lines(dir.path(), 50);
    await_marker(&mut ctrl, "L0");
    ctrl.set_content_viewport(40, 10);

    ctrl.handle(Intent::OpenGoToLine);
    assert!(ctrl.prompt_open());
    assert_eq!(ctrl.content_scroll(), 0, "scroll starts at 0");

    // Enter with no digits typed: close, no scroll.
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert!(!ctrl.prompt_open(), "empty Enter closes the prompt (AC-5)");
    assert_eq!(
        ctrl.content_scroll(),
        0,
        "scroll unchanged on empty Enter (AC-5)"
    );
}

#[test]
fn go_to_line_esc_closes_without_jumping() {
    // AC-6: Esc closes the prompt and leaves content_scroll unchanged.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_lines(dir.path(), 50);
    await_marker(&mut ctrl, "L0");
    ctrl.set_content_viewport(40, 10);

    ctrl.handle(Intent::OpenGoToLine);
    assert!(ctrl.prompt_open());
    // Type some digits to prove Esc discards them.
    ctrl.handle_prompt_key(key(KeyCode::Char('3')));
    ctrl.handle_prompt_key(key(KeyCode::Char('7')));
    assert_eq!(ctrl.prompt_query(), "37");
    ctrl.handle_prompt_key(key(KeyCode::Esc));
    assert!(!ctrl.prompt_open(), "Esc closes the prompt (AC-6)");
    assert_eq!(ctrl.content_scroll(), 0, "scroll unchanged on Esc (AC-6)");
}

#[test]
fn open_go_to_line_is_unavailable_when_no_file_is_selected() {
    // AC-7 edge: when the cursor sits on a directory, selected_view_mode() is None →
    // open_go_to_line fires the unavailable notice and leaves the prompt closed.
    // Build a tree whose first (and only non-hidden) node is a subdirectory.
    // The tree lists alphabetically: "adir/" sorts before any file we might add,
    // and since the cursor starts at index 0, the first node (the directory) is selected.
    let dir = TempDir::new();
    std::fs::create_dir(dir.path().join("adir")).unwrap();
    // Add a file so the controller_with_lines render worker has something to render,
    // but the tree cursor starts at "adir/" (index 0, a directory).
    std::fs::write(dir.path().join("z.txt"), "content\n").unwrap();

    // Use the plain `controller()` helper (not controller_with_lines) — we only need
    // the directory-selection behaviour, not a specific rendered line count.
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    // Confirm the first visible node is a directory (precondition).
    let nodes = ctrl.tree().visible_nodes();
    assert!(
        !nodes.is_empty(),
        "tree must have at least one visible node"
    );
    let first = &nodes[0];
    assert_eq!(
        first.path.file_name().unwrap().to_str().unwrap(),
        "adir",
        "precondition: first node is the subdirectory"
    );
    // Cursor starts at 0 → the directory is selected → selected_view_mode() is None.
    assert_eq!(
        ctrl.selected_view_mode(),
        None,
        "precondition: directory has no view mode"
    );

    ctrl.handle(Intent::OpenGoToLine);
    assert!(
        !ctrl.prompt_open(),
        "AC-7 edge: prompt must NOT open when a directory is selected"
    );
    assert!(
        ctrl.action_notice().is_some(),
        "AC-7 edge: an unavailable notice is set when a directory is selected"
    );
}

/// A git controller whose single file `file` is reported CHANGED (so its default view is Diff, a
/// transformed view), with `n` numbered lines of content — for exercising the go-to-line auto-switch
/// from a transformed view to the source-mapped content view.
fn changed_controller_with_lines(root: &Path, file: &str, n: usize) -> Controller {
    let mut changed = BTreeMap::new();
    changed.insert(PathBuf::from(file), Status::Modified);
    let git = StubGit {
        status: changed.clone(),
        changed,
        ..StubGit::default()
    };
    let git: Arc<dyn GitService> = Arc::new(git);
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::clone(&git),
            content: Box::new(LinesContent { n }),
        }),
        editor: Box::new(StubEditor {
            fail: false,
            opened: Arc::new(Mutex::new(Vec::new())),
        }),
        clipboard: Box::new(common::RecordingClipboard::default()),
    };
    Controller::new(
        common::resolved(root.to_path_buf(), true),
        Baseline::Head,
        components,
    )
}

#[test]
fn go_to_line_in_a_transformed_view_switches_to_content_and_jumps() {
    // AC-7 (revised): confirming `:N` in a transformed view (here Diff) switches the file to the
    // source-mapped content view and jumps to source line N once the re-render lands.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = changed_controller_with_lines(dir.path(), "a.txt", 50);
    await_marker(&mut ctrl, "L0"); // initial render (changed → Diff; LinesContent ignores mode)
    ctrl.set_content_viewport(40, 10); // 50 lines, 10 tall → max_content_scroll = 40

    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::Diff),
        "precondition: the changed file is in Diff (a transformed view)"
    );

    // Open the prompt (opens in any view), type 25, Enter.
    ctrl.handle(Intent::OpenGoToLine);
    assert!(ctrl.prompt_open());
    for c in "25".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));

    // Confirm auto-switched the view to source-mapped content and queued the jump for the re-render.
    assert!(!ctrl.prompt_open(), "Enter closes the prompt");
    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::SyntaxContent),
        "AC-7: confirm auto-switched to the source-mapped content view"
    );
    assert_eq!(
        ctrl.pending_goto_line(),
        Some(25),
        "the jump is queued against the dispatched re-render"
    );

    // Pump poll() until the queued render lands and the jump applies.
    let deadline = Instant::now() + Duration::from_secs(5);
    while ctrl.pending_goto_line().is_some() {
        ctrl.poll();
        assert!(
            Instant::now() < deadline,
            "the auto-switch jump never applied"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        ctrl.content_scroll(),
        24,
        "after the switch render, jumped to line 25 (offset 24)"
    );
}

// review-gate Round 1 — regression tests for the findings fixed in R1.
// ---------------------------------------------------------------------------

#[test]
fn mouse_is_inert_while_the_go_to_line_prompt_is_open() {
    // R1 (HIGH, 5 models): the go-to-line prompt is keyboard-only and modal — the run loop routes
    // only KEY events to it. Without a guard in handle_mouse, a click/wheel would still reach the
    // tree beneath and change the selection, so a subsequent Enter would jump/auto-switch the WRONG
    // file. The mouse must be inert while the prompt is open (mirroring the picker's modal guard).
    let dir = TempDir::new();
    for i in 0..6 {
        std::fs::write(dir.path().join(format!("f{i:02}.txt")), "x\n").unwrap();
    }
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);
    ctrl.set_pane_geometry(wide_geometry());
    assert_eq!(ctrl.tree().cursor(), 0, "cursor starts on f00.txt");

    ctrl.handle(Intent::OpenGoToLine);
    assert!(ctrl.prompt_open(), "prompt opens on the selected file");

    // A left click on another tree row (row 4 → visible node 3) must be swallowed.
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 6, 4));
    assert!(
        !fx.redraw,
        "a click under an open prompt is inert (no redraw)"
    );
    assert_eq!(
        ctrl.tree().cursor(),
        0,
        "the click must NOT move the selection while the prompt is open"
    );
    assert!(
        ctrl.prompt_open(),
        "the prompt stays open after an inert click"
    );

    // A scroll-wheel over the tree is inert too.
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::ScrollDown, 6, 3));
    assert!(!fx.redraw, "a scroll under an open prompt is inert");
    assert_eq!(
        ctrl.tree().cursor(),
        0,
        "scroll does not move the selection under the prompt"
    );
}

/// A content provider for the go-to-line wrap test: 5 long, space-free lines (`W0…`, 25 cols → 3
/// rows each at width 10) then 5 short lines (`S5`..`S9`, 1 row each). With wrap on, source line 6
/// (`S5`) sits at display row 15, not row 5 — so the wrap-aware mapping and the naive `line-1`
/// disagree, which is exactly what the test pins down.
struct WrapLines;
impl ContentProvider for WrapLines {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        let mut lines: Vec<String> = (0..5).map(|i| format!("W{i}{}", "x".repeat(23))).collect();
        lines.extend((5..10).map(|i| format!("S{i}")));
        RenderResult {
            content: Text::raw(lines.join("\n")),
            notices: Vec::new(),
        }
    }
}

fn controller_with_wrap_lines(root: &Path) -> Controller {
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(StubGit::default()),
            content: Box::new(WrapLines),
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
fn go_to_line_maps_source_line_to_wrapped_row_offset_when_wrap_is_on() {
    // R1 (MEDIUM, 4 models): with the `w` wrap override on, a source line no longer maps 1:1 to a
    // display row — earlier long lines wrap into several rows. `:N` must land on source line N (its
    // cumulative wrapped-row offset), not display row N-1, or the target falls off-screen. (AC-3
    // under wrap.) 5 W-lines × 3 rows = 15, so source line 6 (the first S-line) is at row 15.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_wrap_lines(dir.path());
    await_marker(&mut ctrl, "S9"); // 5 long (W) + 5 short (S) lines rendered
    ctrl.set_content_viewport(10, 5); // width 10 → each 25-char W line wraps to 3 rows

    // Wrap OFF: source line 6 maps 1:1 → display row 5.
    ctrl.scroll_to_line(6);
    assert_eq!(
        ctrl.content_scroll(),
        5,
        "wrap off: source line 6 = display row 5 (1:1)"
    );

    // Wrap ON (the `w` key): the 5 W-lines each occupy 3 rows, so source line 6 sits at row 15.
    ctrl.handle(Intent::ToggleWrap);
    ctrl.scroll_to_line(6);
    assert_eq!(
        ctrl.content_scroll(),
        15,
        "wrap on: source line 6 lands at its wrapped-row offset (15), not display row 5"
    );
    assert_ne!(
        ctrl.content_scroll(),
        5,
        "the wrap-aware mapping must differ from the naive line-1"
    );
}

#[test]
fn go_to_line_queues_the_jump_when_a_source_render_is_still_in_flight() {
    // R1 (MEDIUM): if a source file's render hasn't landed yet, selected_view_mode() reports
    // SyntaxContent from the path while self.content is still stale. Confirming `:N` must NOT clamp
    // against the stale content — it queues against the in-flight render (applied_seq != latest_seq)
    // and the jump applies once that render lands.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_lines(dir.path(), 50);
    // Deliberately do NOT await_marker: the initial render is still in flight.
    ctrl.set_content_viewport(40, 10);
    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::SyntaxContent),
        "source-mapped by path even before its render lands"
    );

    ctrl.handle(Intent::OpenGoToLine);
    for c in "25".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert_eq!(
        ctrl.pending_goto_line(),
        Some(25),
        "the jump is queued against the in-flight render, not clamped against stale content"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while ctrl.pending_goto_line().is_some() {
        ctrl.poll();
        assert!(Instant::now() < deadline, "the queued jump never applied");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        ctrl.content_scroll(),
        24,
        "after the render lands, jumped to line 25 (offset 24)"
    );
}

#[test]
fn go_to_line_second_confirm_supersedes_an_in_flight_auto_switch_jump() {
    // R1 (MEDIUM): confirming `:` in a transformed view auto-switches (override → Syntax) and queues
    // a jump against the switch render; selected_view_mode() then reports SyntaxContent immediately.
    // A SECOND confirm before that render lands must WIN — the older queued line must not overwrite
    // it when the render arrives.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = changed_controller_with_lines(dir.path(), "a.txt", 50);
    await_marker(&mut ctrl, "L0"); // initial Diff render landed
    ctrl.set_content_viewport(40, 10);
    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::Diff),
        "starts in a transformed view"
    );

    // First confirm — :10 → auto-switch + queue (render in flight).
    ctrl.handle(Intent::OpenGoToLine);
    for c in "10".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert_eq!(
        ctrl.pending_goto_line(),
        Some(10),
        "first confirm queued line 10"
    );
    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::SyntaxContent),
        "auto-switched the view (override)"
    );

    // Second confirm BEFORE polling — :30 must supersede the queued 10.
    ctrl.handle(Intent::OpenGoToLine);
    for c in "30".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert_eq!(
        ctrl.pending_goto_line(),
        Some(30),
        "the second confirm supersedes the queued jump (30, not 10)"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while ctrl.pending_goto_line().is_some() {
        ctrl.poll();
        assert!(Instant::now() < deadline, "the queued jump never applied");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        ctrl.content_scroll(),
        29,
        "lands on line 30 (offset 29) — the LAST confirm wins, not line 10"
    );
}

// ── T-8: OpenSearch / NextMatch / PrevMatch ─────────────────────────────────

#[test]
fn open_search_opens_a_search_prompt_in_any_view() {
    // AC-8: pressing `/` opens a one-line search prompt at the bottom, in ANY view mode —
    // including RenderedMarkdown (unlike `:` which is view-gated on a file selection).

    // --- SyntaxContent view (an unchanged .rs file) ---
    {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

        assert!(!ctrl.prompt_open(), "precondition: prompt starts closed");
        let fx = ctrl.handle(Intent::OpenSearch);
        assert!(
            ctrl.prompt_open(),
            "AC-8: OpenSearch opens the prompt in SyntaxContent"
        );
        assert!(fx.redraw, "opening the prompt signals a redraw");
        assert!(
            ctrl.action_notice().is_none(),
            "no action notice on success"
        );
        assert_eq!(ctrl.prompt_query(), "", "prompt buffer starts empty");
    }

    // --- RenderedMarkdown view (a .md file) — lock AC-8's "any view" contract ---
    {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("notes.md"), "# Hello\n").unwrap();
        let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

        assert_eq!(
            ctrl.selected_view_mode(),
            Some(ViewMode::RenderedMarkdown),
            "precondition: .md file is in RenderedMarkdown"
        );
        ctrl.handle(Intent::OpenSearch);
        assert!(
            ctrl.prompt_open(),
            "AC-8: OpenSearch opens the prompt in RenderedMarkdown (no view-gate)"
        );
    }
}

#[test]
fn open_search_opens_prompt_with_search_mode() {
    // AC-8: the opened prompt must be in Search mode, not GoToLine mode.
    use herdr_file_viewer::infile::PromptMode;

    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    ctrl.handle(Intent::OpenSearch);
    assert!(ctrl.prompt_open(), "prompt is open");
    assert_eq!(
        ctrl.prompt_mode(),
        Some(PromptMode::Search),
        "AC-8: prompt mode is Search"
    );
    assert_eq!(ctrl.prompt_query(), "", "buffer starts empty");
}

#[test]
fn open_search_is_noop_while_picker_is_open() {
    // Modal mutual-exclusion: OpenSearch is inert while the worktree picker is open.
    // Need a real git repo so SwitchWorktree can open the picker.
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    let (mut ctrl, _, _) = controller(repo.path(), true, StubGit::default(), false);

    ctrl.handle(Intent::SwitchWorktree); // open the picker
    assert!(ctrl.picker().is_some(), "precondition: picker is open");

    let fx = ctrl.handle(Intent::OpenSearch);
    assert!(!fx.redraw, "OpenSearch is a no-op while the picker is open");
    assert!(
        !ctrl.prompt_open(),
        "prompt must not open while the picker is modal"
    );
}

#[test]
fn open_search_is_noop_while_finder_is_open() {
    // Modal mutual-exclusion: OpenSearch is inert while the go-to-file finder is open.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    ctrl.handle(Intent::OpenFinder); // open the finder
    assert!(ctrl.finder_open(), "precondition: finder is open");

    // While the finder is open handle() is inert for ALL non-picker intents — the
    // structural guard returns noop(). The prompt must not open.
    let fx = ctrl.handle(Intent::OpenSearch);
    assert!(!fx.redraw, "inert while finder is modal");
    assert!(
        !ctrl.prompt_open(),
        "prompt stays closed while finder is open"
    );
}

#[test]
fn next_match_and_prev_match_are_noops_with_no_committed_search() {
    // AC-19: n/N have no effect when there is no committed search with ≥1 match.
    // At this task stage no committed search ever exists, so both are always no-ops.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    let scroll_before = ctrl.content_scroll();

    let fx_next = ctrl.handle(Intent::NextMatch);
    assert!(!fx_next.redraw, "NextMatch is a no-op: no redraw");
    assert!(!ctrl.prompt_open(), "NextMatch must not open a prompt");
    assert_eq!(
        ctrl.content_scroll(),
        scroll_before,
        "NextMatch must not scroll the content pane"
    );

    let fx_prev = ctrl.handle(Intent::PrevMatch);
    assert!(!fx_prev.redraw, "PrevMatch is a no-op: no redraw");
    assert!(!ctrl.prompt_open(), "PrevMatch must not open a prompt");
    assert_eq!(
        ctrl.content_scroll(),
        scroll_before,
        "PrevMatch must not scroll the content pane"
    );
}

// ── T-9: Search open + incremental matching ──────────────────────────────────

/// Content renderer that returns a predictable multi-line body with known searchable tokens.
struct SearchContent;
impl ContentProvider for SearchContent {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        // 20 lines; "needle" appears at lines 2, 5, 10, 15 (0-based: 1, 4, 9, 14).
        let lines: Vec<String> = (0..20)
            .map(|i| match i {
                1 | 4 | 9 | 14 => format!("line{i} needle here"),
                _ => format!("line{i} other content"),
            })
            .collect();
        RenderResult {
            content: Text::raw(lines.join("\n")),
            notices: Vec::new(),
        }
    }
}

fn controller_with_search_content(root: &Path) -> Controller {
    let components = Components {
        providers: Box::new(|_resolved| RootProviders {
            git: Arc::new(StubGit::default()),
            content: Box::new(SearchContent),
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
fn search_typing_populates_matches_and_scrolls_to_first_match() {
    // AC-9: typing into the search prompt populates matches from the displayed content.
    // AC-10: when matches exist the content scrolls so a match is within the viewport.
    use herdr_file_viewer::infile::PromptMode;

    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_search_content(dir.path());
    await_marker(&mut ctrl, "needle"); // wait for the SearchContent render to land
    ctrl.set_content_viewport(40, 5); // 20 lines, 5 visible → max scroll = 15

    // Open the search prompt.
    ctrl.handle(Intent::OpenSearch);
    assert!(ctrl.prompt_open(), "precondition: prompt is open");
    assert_eq!(ctrl.prompt_mode(), Some(PromptMode::Search));

    // Before typing, search() should be None (no search state yet).
    assert!(ctrl.search().is_none(), "no search state before typing");

    // Snapshot scroll before typing — first match is at line 1 (0-based), so scroll should move.
    let scroll_before = ctrl.content_scroll();

    // Type "needle" character by character.
    for c in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }

    // AC-9: matches populated from displayed content.
    let s = ctrl
        .search()
        .expect("SearchState must be Some after typing");
    assert_eq!(
        s.matches.len(),
        4,
        "AC-9: 4 'needle' matches in the content (lines 1,4,9,14)"
    );

    // AC-9: current is 0 (first match in document order).
    assert_eq!(s.current, 0, "AC-9: current match is index 0 (first)");

    // AC-10: scroll must have moved to bring the first match into view.
    // First match is at content line 1 (0-based); scroll_to_line(2) → offset 1.
    assert_ne!(
        ctrl.content_scroll(),
        scroll_before.max(1) - 1, // the top was already 0; first match line is 1
        "AC-10: content scrolled toward first match"
    );
    // Concretely: scroll_to_line(2) sets offset = line-1 = 1.
    assert_eq!(
        ctrl.content_scroll(),
        1,
        "AC-10: scrolled to display row 1 (first match's line offset)"
    );
}

#[test]
fn search_no_match_leaves_matches_empty_and_scroll_unchanged() {
    // AC-18: a query that matches nothing → matches empty AND content scroll is unchanged.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_search_content(dir.path());
    await_marker(&mut ctrl, "needle");
    ctrl.set_content_viewport(40, 5);

    // Scroll down a bit first so we can confirm it doesn't move.
    ctrl.handle(Intent::ToggleFocus);
    ctrl.handle(Intent::NavDown);
    ctrl.handle(Intent::NavDown);
    ctrl.handle(Intent::NavDown);
    let scroll_before = ctrl.content_scroll();
    assert!(scroll_before > 0, "precondition: we've scrolled down");
    ctrl.handle(Intent::ToggleFocus); // back to tree focus

    ctrl.handle(Intent::OpenSearch);

    // Type a query that definitely doesn't appear in the content.
    for c in "xyzzy_not_found".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }

    let s = ctrl.search().expect("SearchState exists even on no-match");
    assert!(s.matches.is_empty(), "AC-18: no matches for absent query");
    assert_eq!(
        ctrl.content_scroll(),
        scroll_before,
        "AC-18: scroll must not move when there are no matches"
    );
}

#[test]
fn search_backspace_rematches() {
    // Backspace reduces the query and re-runs find_matches; dropping the last char of a
    // no-match query can restore matches (incremental re-match, AC-9).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_search_content(dir.path());
    await_marker(&mut ctrl, "needle");
    ctrl.set_content_viewport(40, 5);

    ctrl.handle(Intent::OpenSearch);

    // Type "needle" (matches exist).
    for c in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    assert_eq!(
        ctrl.search().unwrap().matches.len(),
        4,
        "precondition: 4 matches for 'needle'"
    );

    // Extend with 'X' → "needleX" which matches nothing.
    ctrl.handle_prompt_key(key(KeyCode::Char('X')));
    assert!(
        ctrl.search().unwrap().matches.is_empty(),
        "no matches for 'needleX'"
    );

    // Backspace: back to "needle" → matches restore.
    ctrl.handle_prompt_key(key(KeyCode::Backspace));
    assert_eq!(
        ctrl.search().unwrap().matches.len(),
        4,
        "AC-9: Backspace rematches; 4 'needle' matches restored"
    );
    assert_eq!(
        ctrl.search().unwrap().current,
        0,
        "current is 0 after re-match"
    );
}

#[test]
fn search_accepts_all_printable_chars_not_just_digits() {
    // The search prompt accepts any printable char (letters, digits, symbols, spaces).
    // Contrast with go-to-line which rejects non-digit printables.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_search_content(dir.path());
    await_marker(&mut ctrl, "needle");

    ctrl.handle(Intent::OpenSearch);

    // Type a query with mixed chars including uppercase (via SHIFT), digits, and symbols.
    // "Line" with uppercase L — key_shift is already defined in this file.
    ctrl.handle_prompt_key(key_shift('L'));
    ctrl.handle_prompt_key(key(KeyCode::Char('i')));
    ctrl.handle_prompt_key(key(KeyCode::Char('n')));
    ctrl.handle_prompt_key(key(KeyCode::Char('e')));

    // "Line" is case-sensitive (uppercase L) → won't match "line…" (lowercase l).
    let s = ctrl.search().expect("SearchState must be Some");
    assert_eq!(
        ctrl.prompt_query(),
        "Line",
        "prompt buffer contains all typed chars including shift-char"
    );
    // Smartcase: "Line" has uppercase → case-sensitive → no match on "line…"
    assert!(
        s.matches.is_empty(),
        "case-sensitive 'Line' doesn't match lowercase 'line…'"
    );

    // Now type a space + digit (symbol-ish chars).
    ctrl.handle_prompt_key(key(KeyCode::Char(' ')));
    ctrl.handle_prompt_key(key(KeyCode::Char('1')));
    // Query is "Line 1" — still no match (uppercase L, case-sensitive).
    assert_eq!(ctrl.prompt_query(), "Line 1", "space and digit accepted");
}

#[test]
fn search_esc_closes_prompt() {
    // Esc closes the search prompt (minimal T-9 behavior; Esc-restore is T-11).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    ctrl.handle(Intent::OpenSearch);
    assert!(ctrl.prompt_open(), "precondition: prompt is open");

    ctrl.handle_prompt_key(key(KeyCode::Esc));
    assert!(!ctrl.prompt_open(), "Esc closes the search prompt");
}

#[test]
fn search_enter_closes_prompt() {
    // Enter closes the search prompt (minimal T-9 behavior; commit semantics are T-10).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let (mut ctrl, _, _) = controller(dir.path(), false, StubGit::default(), false);

    ctrl.handle(Intent::OpenSearch);
    assert!(ctrl.prompt_open(), "precondition: prompt is open");

    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert!(!ctrl.prompt_open(), "Enter closes the search prompt");
}

// ── T-10: Search commit + n/N + wrap ─────────────────────────────────────────

#[test]
fn search_enter_commits_retaining_search_state() {
    // AC-14: Enter closes the prompt but retains the SearchState (query + matches + current).
    // The committed SearchState must be non-None with the same matches as before Enter.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_search_content(dir.path());
    await_marker(&mut ctrl, "needle");
    ctrl.set_content_viewport(40, 5);

    // Open search and type "needle" → 4 matches.
    ctrl.handle(Intent::OpenSearch);
    for c in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    let matches_before = ctrl
        .search()
        .expect("SearchState after typing")
        .matches
        .len();
    assert_eq!(matches_before, 4, "precondition: 4 'needle' matches");

    // Press Enter to commit.
    ctrl.handle_prompt_key(key(KeyCode::Enter));

    // Prompt must be closed.
    assert!(!ctrl.prompt_open(), "AC-14: Enter closes the prompt");
    // SearchState must be retained (not cleared).
    let s = ctrl
        .search()
        .expect("AC-14: SearchState retained after Enter");
    assert_eq!(
        s.matches.len(),
        4,
        "AC-14: all 4 matches are retained after commit"
    );
    assert_eq!(s.current, 0, "AC-14: current stays at 0 after commit");
}

#[test]
fn next_match_advances_current_and_scrolls() {
    // AC-15: after a committed search, n advances current (document order) and scrolls.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_search_content(dir.path());
    await_marker(&mut ctrl, "needle");
    ctrl.set_content_viewport(40, 5); // 20 lines; needle at 0-based 1,4,9,14

    // Commit a search for "needle".
    ctrl.handle(Intent::OpenSearch);
    for c in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert!(!ctrl.prompt_open(), "precondition: prompt closed");
    assert_eq!(ctrl.search().unwrap().current, 0, "precondition: current=0");

    // First NextMatch: 0 → 1 (match at line 4, 0-based).
    let fx = ctrl.handle(Intent::NextMatch);
    assert!(fx.redraw, "AC-15: NextMatch returns redraw");
    assert_eq!(
        ctrl.search().unwrap().current,
        1,
        "AC-15: n advances current 0→1"
    );
    // scroll_to_line(5) → offset 4 (line 4, 0-based, → 1-based = 5, offset = 5-1 = 4).
    assert_eq!(
        ctrl.content_scroll(),
        4,
        "AC-15: scrolled to match at line 4"
    );

    // Second NextMatch: 1 → 2 (match at line 9, 0-based).
    ctrl.handle(Intent::NextMatch);
    assert_eq!(
        ctrl.search().unwrap().current,
        2,
        "AC-15: n advances current 1→2"
    );
    assert_eq!(
        ctrl.content_scroll(),
        9,
        "AC-15: scrolled to match at line 9"
    );
}

#[test]
fn prev_match_retreats_current_and_scrolls() {
    // AC-15: PrevMatch retreats current (document order) and scrolls.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_search_content(dir.path());
    await_marker(&mut ctrl, "needle");
    ctrl.set_content_viewport(40, 5);

    // Commit search, advance to match 2.
    ctrl.handle(Intent::OpenSearch);
    for c in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    ctrl.handle(Intent::NextMatch); // → 1
    ctrl.handle(Intent::NextMatch); // → 2
    assert_eq!(ctrl.search().unwrap().current, 2, "precondition: current=2");

    // PrevMatch: 2 → 1.
    let fx = ctrl.handle(Intent::PrevMatch);
    assert!(fx.redraw, "AC-15: PrevMatch returns redraw");
    assert_eq!(
        ctrl.search().unwrap().current,
        1,
        "AC-15: N retreats current 2→1"
    );
    assert_eq!(ctrl.content_scroll(), 4, "AC-15: scrolled to line 4");

    // PrevMatch again: 1 → 0.
    ctrl.handle(Intent::PrevMatch);
    assert_eq!(
        ctrl.search().unwrap().current,
        0,
        "AC-15: N retreats current 1→0"
    );
    assert_eq!(ctrl.content_scroll(), 1, "AC-15: scrolled to line 1");
}

#[test]
fn next_match_wraps_past_last_with_notice() {
    // AC-16: advancing past the last match wraps to the first and sets a wrap notice.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_search_content(dir.path());
    await_marker(&mut ctrl, "needle");
    ctrl.set_content_viewport(40, 5);

    // Commit search for "needle" (4 matches; current=0).
    ctrl.handle(Intent::OpenSearch);
    for c in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));

    // Advance to the last match (index 3).
    ctrl.handle(Intent::NextMatch); // → 1
    ctrl.handle(Intent::NextMatch); // → 2
    ctrl.handle(Intent::NextMatch); // → 3
    assert_eq!(
        ctrl.search().unwrap().current,
        3,
        "precondition: at last match"
    );
    // Clear notice (NextMatch at non-wrapping positions may have set none, but advance_search
    // sets action_notice only on wrap — ensure we don't carry a stale one).

    // NextMatch past the last → wraps to 0, notice set.
    let fx = ctrl.handle(Intent::NextMatch);
    assert!(fx.redraw, "AC-16: wrap still returns redraw");
    assert_eq!(
        ctrl.search().unwrap().current,
        0,
        "AC-16: wrapped to first match (index 0)"
    );
    let notice = ctrl.action_notice().expect("AC-16: wrap notice is set");
    assert!(
        notice.contains("wrap") || notice.contains("first"),
        "AC-16: wrap notice mentions wrap/first: {notice}"
    );
}

#[test]
fn prev_match_wraps_before_first_with_notice() {
    // AC-16: going before the first match wraps to the last and sets a wrap notice.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_search_content(dir.path());
    await_marker(&mut ctrl, "needle");
    ctrl.set_content_viewport(40, 5);

    // Commit search for "needle"; current=0.
    ctrl.handle(Intent::OpenSearch);
    for c in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert_eq!(
        ctrl.search().unwrap().current,
        0,
        "precondition: at first match"
    );

    // PrevMatch from first → wraps to last (index 3), notice set.
    let fx = ctrl.handle(Intent::PrevMatch);
    assert!(fx.redraw, "AC-16: wrap still returns redraw");
    assert_eq!(
        ctrl.search().unwrap().current,
        3,
        "AC-16: wrapped to last match (index 3)"
    );
    let notice = ctrl.action_notice().expect("AC-16: wrap notice is set");
    assert!(
        notice.contains("wrap") || notice.contains("last"),
        "AC-16: wrap notice mentions wrap/last: {notice}"
    );
}

#[test]
fn next_match_prev_match_inert_with_zero_match_committed_search() {
    // AC-19: n/N have no effect when a search is committed but has zero matches.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_search_content(dir.path());
    await_marker(&mut ctrl, "needle");
    ctrl.set_content_viewport(40, 5);

    // Open search and type a query that matches nothing, then commit (Enter).
    ctrl.handle(Intent::OpenSearch);
    for c in "xyzzy_absent".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    // Verify zero matches before committing.
    assert!(
        ctrl.search().is_some_and(|s| s.matches.is_empty()),
        "precondition: zero matches for absent query"
    );
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert!(
        !ctrl.prompt_open(),
        "precondition: prompt closed after Enter"
    );
    // SearchState is Some but has zero matches (committed zero-match search).
    let s = ctrl.search().expect("SearchState retained after Enter");
    assert!(
        s.matches.is_empty(),
        "precondition: committed search has zero matches"
    );

    let scroll_before = ctrl.content_scroll();

    // n → no-op.
    let fx_next = ctrl.handle(Intent::NextMatch);
    assert!(
        !fx_next.redraw,
        "AC-19: NextMatch is a no-op with zero matches: no redraw"
    );
    assert_eq!(
        ctrl.content_scroll(),
        scroll_before,
        "AC-19: NextMatch must not scroll with zero matches"
    );
    assert_eq!(
        ctrl.search().unwrap().current,
        0,
        "AC-19: current unchanged after no-op NextMatch"
    );

    // N → no-op.
    let fx_prev = ctrl.handle(Intent::PrevMatch);
    assert!(
        !fx_prev.redraw,
        "AC-19: PrevMatch is a no-op with zero matches: no redraw"
    );
    assert_eq!(
        ctrl.content_scroll(),
        scroll_before,
        "AC-19: PrevMatch must not scroll with zero matches"
    );
}

// ── T-11: Search cancel + clear lifecycle ─────────────────────────────────────

#[test]
fn search_esc_restores_scroll_and_clears_search_state() {
    // AC-17: Esc in search mode cancels — restores the pre-open scroll AND clears self.search.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_search_content(dir.path());
    await_marker(&mut ctrl, "needle"); // wait for initial render
    ctrl.set_content_viewport(40, 5); // 20 lines, 5 visible → max scroll = 15

    // Scroll the content pane down to a known position before opening the search prompt.
    ctrl.handle(Intent::ToggleFocus); // switch to content focus
    for _ in 0..6 {
        ctrl.handle(Intent::NavDown); // scroll down 6 lines
    }
    let pre_open_scroll = ctrl.content_scroll();
    assert!(
        pre_open_scroll > 0,
        "precondition: scroll is non-zero before opening search"
    );

    // Switch back to tree focus so we can open the search prompt.
    ctrl.handle(Intent::ToggleFocus);

    // Open the search prompt (snapshots scroll into saved_scroll).
    ctrl.handle(Intent::OpenSearch);
    assert!(ctrl.prompt_open(), "precondition: prompt is open");

    // Type "needle" — this should scroll to the first match (line 1) and populate self.search.
    for c in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    assert!(
        ctrl.search().is_some(),
        "precondition: search state populated after typing"
    );
    // The scroll has moved away from pre_open_scroll (to first match at line 1).
    // Because we scrolled to 6 before and first match is at line 1, scroll should now be 1.
    let scroll_after_typing = ctrl.content_scroll();
    assert_ne!(
        scroll_after_typing, pre_open_scroll,
        "precondition: scroll moved while typing (first match scrolled into view)"
    );

    // Press Esc → cancel: should restore saved_scroll AND clear search.
    ctrl.handle_prompt_key(key(KeyCode::Esc));

    // AC-17: prompt is closed.
    assert!(!ctrl.prompt_open(), "AC-17: Esc closes the prompt");
    // AC-17: content_scroll is restored to the pre-open position.
    assert_eq!(
        ctrl.content_scroll(),
        pre_open_scroll,
        "AC-17: Esc restores content_scroll to the pre-open value"
    );
    // AC-17: self.search is cleared (None), not retained.
    assert!(
        ctrl.search().is_none(),
        "AC-17: Esc clears search state (self.search = None)"
    );
}

#[test]
fn open_search_clears_prior_committed_search() {
    // AC-20 (new search clears prior): committing a search then opening a new one clears
    // the previously committed SearchState.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_search_content(dir.path());
    await_marker(&mut ctrl, "needle");
    ctrl.set_content_viewport(40, 5);

    // Commit a first search for "needle".
    ctrl.handle(Intent::OpenSearch);
    for c in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert!(
        !ctrl.prompt_open(),
        "precondition: prompt closed after Enter"
    );
    assert!(
        ctrl.search().is_some(),
        "precondition: committed search is Some after Enter"
    );

    // Open a new search — this should clear the prior committed SearchState.
    let fx = ctrl.handle(Intent::OpenSearch);
    assert!(fx.redraw, "OpenSearch returns redraw");
    assert!(ctrl.prompt_open(), "AC-20: new search prompt is open");

    // The prior committed search must be cleared (None) immediately on opening.
    assert!(
        ctrl.search().is_none(),
        "AC-20: opening a new search clears the prior committed SearchState"
    );
}

#[test]
fn content_change_clears_committed_search_file_select() {
    // AC-20 (content change clears committed search): navigating to a different file clears
    // the committed SearchState because dispatch_render is called.
    let dir = TempDir::new();
    // Need two files so NavDown can select the second one.
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "y\n").unwrap();
    let mut ctrl = controller_with_search_content(dir.path());
    await_marker(&mut ctrl, "needle");
    ctrl.set_content_viewport(40, 5);

    // Commit a search for "needle".
    ctrl.handle(Intent::OpenSearch);
    for c in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert!(
        ctrl.search().is_some(),
        "precondition: search is Some after commit"
    );

    // Navigate to the next file — this calls dispatch_render which must clear search.
    ctrl.handle(Intent::NavDown);

    // AC-20: the committed search must be cleared synchronously by dispatch_render.
    assert!(
        ctrl.search().is_none(),
        "AC-20: navigating to a different file clears the committed search"
    );
}

#[test]
fn content_change_clears_committed_search_cycle_view() {
    // AC-20 (content change clears committed search): cycling the view mode clears the
    // committed SearchState because dispatch_render is called.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_search_content(dir.path());
    await_marker(&mut ctrl, "needle");
    ctrl.set_content_viewport(40, 5);

    // Commit a search for "needle".
    ctrl.handle(Intent::OpenSearch);
    for c in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert!(
        ctrl.search().is_some(),
        "precondition: search is Some after commit"
    );

    // Cycle the view mode — calls dispatch_render which must clear search.
    ctrl.handle(Intent::CycleView);

    // AC-20: the committed search must be cleared synchronously.
    assert!(
        ctrl.search().is_none(),
        "AC-20: cycling the view mode clears the committed search"
    );
}

#[test]
fn incremental_search_typing_does_not_clear_search_via_dispatch_render() {
    // Regression guard: live incremental typing (refresh_search) must NOT call dispatch_render,
    // which would wipe self.search while the user is still typing (AC-17 / AC-20 invariant).
    // This test verifies that typing into the search prompt never wipes the SearchState mid-type.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let mut ctrl = controller_with_search_content(dir.path());
    await_marker(&mut ctrl, "needle");
    ctrl.set_content_viewport(40, 5);

    ctrl.handle(Intent::OpenSearch);

    // Type one character at a time and verify search is always Some (never wiped).
    for c in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
        assert!(
            ctrl.search().is_some(),
            "AC-17: search state must remain Some while typing (not cleared by dispatch_render); failed after typing '{c}'"
        );
    }

    // Also Backspace must not wipe.
    ctrl.handle_prompt_key(key(KeyCode::Backspace));
    assert!(
        ctrl.search().is_some(),
        "AC-17: search state remains Some after Backspace"
    );
}
