//! Line-select modal state (copy-line-reference, T-3): entering places the marker on the top
//! visible source line (AC-1) and exiting closes the modal without touching the clipboard (AC-4).
//! Every side-effecting component is stubbed, so these tests touch no real git / renderer / editor
//! and read back a recording clipboard to prove the exit path copies nothing.

mod common;

use common::{RecordingClipboard, TempDir, resolved};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use herdr_file_viewer::controller::{
    Clipboard, Components, ContentProvider, Controller, EditorHandoff, EditorOutcome, GitService,
    RenderResult, RootProviders,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::presenter::{Focus, PaneGeometry};
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::layout::Rect;
use ratatui::text::Text;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A key event with no modifier (the common case for `j`/`k`/arrows).
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

// ── stubs ──────────────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct StubGit;
impl GitService for StubGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }
    fn changed_set(&self, _baseline: Baseline) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }
    fn diff(&self, _p: &Path, _b: Baseline, _full: bool) -> String {
        String::new()
    }
    fn diff_directory(&self, _rel_dir: &Path, _baseline: Baseline) -> String {
        String::new()
    }
}

struct NoopEditor;
impl EditorHandoff for NoopEditor {
    fn open(&mut self, _file: &Path) -> EditorOutcome {
        EditorOutcome::NoTakeover
    }
}

/// A content provider that returns a fixed 20-line body (`line0`..`line19`) regardless of view
/// mode, so a test can scroll to a known offset and read the top visible source line.
#[derive(Clone, Copy)]
struct MultiLine;
impl ContentProvider for MultiLine {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        let lines: Vec<String> = (0..20).map(|i| format!("line{i}")).collect();
        RenderResult {
            content: Text::raw(lines.join("\n")),
            notices: Vec::new(),
            source: None,
        }
    }
}

/// A content provider whose first two source lines are wider than the 80-column viewport, so with
/// the `w` wrap override on they each occupy several DISPLAY rows — exercising the wrap fix where
/// line-select must map between wrapped display rows and 1-based source lines (not assume 1:1).
/// At width 80: source line 1 = 170 `a`s → 3 rows (display rows 0-2); line 2 = 90 `b`s → 2 rows
/// (rows 3-4); lines 3..10 (`line2`..`line9`) are short → 1 row each (rows 5,6,…). So the wrapped
/// offset of source line 2 is 3, of line 3 is 5 — both differ from the buggy `scroll + 1` mapping.
#[derive(Clone, Copy)]
struct WrapBody;
impl ContentProvider for WrapBody {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        let mut lines = vec!["a".repeat(170), "b".repeat(90)];
        lines.extend((2..10).map(|i| format!("line{i}")));
        RenderResult {
            content: Text::raw(lines.join("\n")),
            notices: Vec::new(),
            source: None,
        }
    }
}

/// A Clipboard stub whose `copy` always fails, so a test can prove the copy adapter surfaces a
/// failure notice (AC-11) without panicking when the clipboard is unavailable.
#[derive(Default, Clone)]
struct FailingClipboard;
impl Clipboard for FailingClipboard {
    fn copy(&mut self, _text: &str) -> std::io::Result<()> {
        Err(std::io::Error::other("clipboard unavailable"))
    }
}

/// Build a non-git controller over `root` with the given content provider and a recording
/// clipboard, returning the controller plus a handle to the clipboard's copy log so a test can
/// assert what (if anything) was copied.
fn controller_with_clipboard(
    root: &Path,
    content: impl ContentProvider + Clone + 'static,
) -> (Controller, Arc<Mutex<Vec<String>>>) {
    let clipboard = RecordingClipboard::default();
    let copied = clipboard.copied.clone();
    let ctrl = controller_with(root, content, Box::new(clipboard));
    (ctrl, copied)
}

/// Build a non-git controller over `root` with the given content provider and an explicit
/// clipboard double (recording or failing).
fn controller_with(
    root: &Path,
    content: impl ContentProvider + Clone + 'static,
    clipboard: Box<dyn Clipboard>,
) -> Controller {
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(StubGit),
            content: Box::new(content.clone()),
        }),
        editor: Box::new(NoopEditor),
        clipboard,
        renderers: None,
    };
    Controller::new(
        resolved(root.to_path_buf(), false),
        Baseline::Head,
        components,
    )
}

/// Flatten the content pane to a plain string and spin `poll()` until it contains `marker`.
fn await_marker(ctrl: &mut Controller, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text: String = ctrl
            .content()
            .lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        if text.contains(marker) {
            return;
        }
        ctrl.poll();
        assert!(
            Instant::now() < deadline,
            "content '{marker}' never rendered"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A content provider that always renders a genuinely empty body (zero lines, `Text::default()`)
/// — used to reach the empty `content.lines` case (T-4/AC-3). Note `Text::raw("")` is NOT
/// empty here: ratatui gives it exactly one (empty) `Line`, same as the non-empty
/// "Rendering…" loading placeholder `dispatch_render` shows while a render is in flight — only
/// a bare `Text::default()` has a truly empty `lines` vec.
#[derive(Clone, Copy)]
struct EmptyContent;
impl ContentProvider for EmptyContent {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        RenderResult {
            content: Text::default(),
            notices: Vec::new(),
            source: None,
        }
    }
}

/// Content whose single source line carries a raw ESC control byte and a leading tab, so a copy
/// test can prove `copy_line_content` strips the residual control byte while PRESERVING the tab
/// (indentation). Real renders never leak an ESC into a span (the renderer cleans it first), but
/// this stub bypasses that path, exercising the copy adapter's own defense-in-depth filter.
#[derive(Clone, Copy)]
struct ControlContent;
impl ContentProvider for ControlContent {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        RenderResult {
            content: Text::raw("\tcode\x1bhere"),
            notices: Vec::new(),
            source: None,
        }
    }
}

/// Content that mimics `bat --style=numbers`: a right-aligned line-number gutter + a one-space
/// separator, then the source line — with real indentation on line 2 — so a copy test proves the
/// gutter is stripped while the code's own indentation survives. Carries the retained source, as
/// the live `SyntaxContent` renderer always does (a line-number gutter exists ONLY behind that
/// mode, which is exactly when source is retained — see `RenderResult::source`).
#[derive(Clone, Copy)]
struct GutterContent;
impl ContentProvider for GutterContent {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        let lines = ["  1 fn main() {", "  2     let x = 5;", "  3 }"];
        RenderResult {
            content: Text::raw(lines.join("\n")),
            notices: Vec::new(),
            source: Some(vec![
                "fn main() {".to_string(),
                "    let x = 5;".to_string(),
                "}".to_string(),
            ]),
        }
    }
}

/// A genuinely source-less view (rendered markdown / diff — no per-display-line source, no gutter)
/// whose line 2 happens to start with "2 " — the input that fools the bare digit heuristic. With no
/// source to anchor, `gutter_len_of` must return 0 (there is no gutter in such a view), so the copy
/// keeps the leading "2 ". Guards the source-less half of the gutter-strip fix.
#[derive(Clone, Copy)]
struct PlainNoSource;
impl ContentProvider for PlainNoSource {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        let lines = ["intro line", "2 spaces of intro", "tail line"];
        RenderResult {
            content: Text::raw(lines.join("\n")),
            notices: Vec::new(),
            source: None,
        }
    }
}

/// Spin `poll()` until the worker's empty render has landed — i.e. until `content.lines` is
/// actually empty, past the non-empty "Rendering…" placeholder `dispatch_render` shows while
/// the job is in flight.
fn await_empty(ctrl: &mut Controller) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ctrl.content().lines.is_empty() {
        ctrl.poll();
        assert!(Instant::now() < deadline, "content never settled to empty");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ── tests ──────────────────────────────────────────────────────────────────────

#[test]
fn enter_places_marker_on_top_line() {
    // AC-1: entering line-select drops the marker on the top *visible* source line
    // (`content_scroll + 1`), collapsed onto a single line.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    await_marker(&mut ctrl, "line0"); // wait for the render worker to fill content.lines

    // Scroll so source line 5 sits at the top of the viewport (content_scroll == 4).
    ctrl.set_content_viewport(80, 10);
    ctrl.scroll_to_line(5);
    assert_eq!(
        ctrl.content_scroll(),
        4,
        "precondition: line 5 is at the top"
    );

    ctrl.enter_line_select_at_top();
    assert!(
        ctrl.line_select_active(),
        "line-select is active after entering"
    );
    assert_eq!(
        ctrl.line_selection(),
        Some((5, 5)),
        "AC-1: marker collapsed onto the top visible source line (5)"
    );
}

#[test]
fn esc_exits_without_copy() {
    // AC-4: leaving line-select closes the modal and copies nothing.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    await_marker(&mut ctrl, "line0");

    ctrl.enter_line_select_at_top();
    assert!(
        ctrl.line_select_active(),
        "precondition: line-select is active"
    );

    ctrl.exit_line_select();
    assert!(
        !ctrl.line_select_active(),
        "AC-4: exit closes the line-select modal"
    );
    assert!(
        copied.lock().unwrap().is_empty(),
        "AC-4: exiting line-select copied nothing to the clipboard"
    );
}

#[test]
fn l_on_content_focus_enters_mode() {
    // T-4/AC-1: with the content pane focused and a source-view file with ≥1 rendered line,
    // `L` (`Intent::TreeScrollRight`) enters line-select (ADR-0010) instead of h-scrolling the
    // tree (which is inert on content focus regardless).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    await_marker(&mut ctrl, "line0");

    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content, "content is now focused");
    assert!(
        !ctrl.line_select_active(),
        "precondition: line-select is not active yet"
    );

    let fx = ctrl.handle(Intent::TreeScrollRight);
    assert!(fx.redraw, "entering line-select redraws");
    assert!(
        ctrl.line_select_active(),
        "AC-1: L on content focus with ≥1 rendered line enters line-select"
    );
}

#[test]
fn l_on_empty_content_is_inert() {
    // T-4/AC-3: with the content pane focused but zero rendered lines (no file / an empty
    // render), `L` (`Intent::TreeScrollRight`) is a plain no-op — it must not enter line-select
    // on an empty pane.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("empty.rs"), "").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), EmptyContent);
    await_empty(&mut ctrl);
    assert!(
        ctrl.content().lines.is_empty(),
        "precondition: the content pane has no rendered lines"
    );

    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content, "content is now focused");

    let fx = ctrl.handle(Intent::TreeScrollRight);
    assert!(!fx.redraw, "AC-3: L is inert when content has no lines");
    assert!(
        !ctrl.line_select_active(),
        "AC-3: line-select does not activate on empty content"
    );
}

#[test]
fn j_k_move_marker_one_line() {
    // AC-5: `j`/`Down` moves the marker down one source line, `k`/`Up` moves it up one — each
    // a plain (collapsing) move, so the selection stays a single line.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    await_marker(&mut ctrl, "line0");

    ctrl.set_content_viewport(80, 10);
    ctrl.scroll_to_line(5); // line 5 at the top of the viewport
    ctrl.enter_line_select_at_top();
    assert_eq!(
        ctrl.line_selection(),
        Some((5, 5)),
        "precondition: marker at 5"
    );

    let fx = ctrl.handle_line_select_key(key(KeyCode::Char('j')));
    assert!(fx.redraw, "a move redraws");
    assert_eq!(
        ctrl.line_selection(),
        Some((6, 6)),
        "AC-5: j moves the marker down one line, collapsed"
    );

    ctrl.handle_line_select_key(key(KeyCode::Char('k')));
    ctrl.handle_line_select_key(key(KeyCode::Char('k')));
    assert_eq!(
        ctrl.line_selection(),
        Some((4, 4)),
        "AC-5: k moves the marker up one line, collapsed"
    );
}

#[test]
fn shift_j_extends_selection() {
    // AC-12: Shift+`j` (reported as `Char('J')`) extends the selection from the held anchor
    // instead of collapsing it — the anchor stays put while the marker moves.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    await_marker(&mut ctrl, "line0");

    ctrl.set_content_viewport(80, 10);
    ctrl.scroll_to_line(5);
    ctrl.enter_line_select_at_top();
    assert_eq!(
        ctrl.line_selection(),
        Some((5, 5)),
        "precondition: marker at 5"
    );

    // Char('J') (Shift+j, uppercase) extends: anchor 5 held, marker → 6.
    ctrl.handle_line_select_key(key(KeyCode::Char('J')));
    assert_eq!(
        ctrl.line_selection(),
        Some((5, 6)),
        "AC-12: Shift+j extends the selection (anchor 5 held, marker 6)"
    );

    // Shift+Down (arrow + SHIFT modifier) extends the same way.
    ctrl.handle_line_select_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    assert_eq!(
        ctrl.line_selection(),
        Some((5, 7)),
        "AC-12: Shift+Down also extends (arrow + SHIFT)"
    );
}

#[test]
fn marker_scrolls_into_view_when_moved_past_viewport() {
    // AC-7: after each move the content pane scrolls so the marker row stays within the
    // viewport `[content_scroll+1, content_scroll+content_height]` (1-based marker vs 0-based
    // scroll offset).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    await_marker(&mut ctrl, "line0");

    let height: usize = 5;
    ctrl.set_content_viewport(80, height as u16); // small viewport, 20-line body
    ctrl.scroll_to_line(1); // top
    ctrl.enter_line_select_at_top();
    assert_eq!(
        ctrl.line_selection(),
        Some((1, 1)),
        "precondition: marker at 1"
    );

    // Drive the marker to the bottom of the file one line at a time; the marker must remain
    // visible after every step (it starts pushing the scroll once it passes the viewport bottom).
    for _ in 0..19 {
        ctrl.handle_line_select_key(key(KeyCode::Char('j')));
        let (_, marker) = ctrl.line_selection().unwrap();
        let top = ctrl.content_scroll() as usize; // 0-based first visible row
        assert!(
            marker > top && marker <= top + height,
            "AC-7: marker {marker} stayed within view [{}, {}]",
            top + 1,
            top + height
        );
    }
    assert_eq!(
        ctrl.line_selection(),
        Some((20, 20)),
        "marker clamps at the last line"
    );
}

// ── T-6: auto-switch-to-source on enter (AC-15) ──────────────────────────────────

#[test]
fn enter_from_markdown_switches_to_source_then_activates() {
    // AC-15: entering line-select on a file in a *transformed* view (here RenderedMarkdown, a
    // markdown file's default) must NOT open the modal immediately — it switches the file to the
    // source-mapped content view and defers the entry until that render lands, then drops the
    // marker on the top visible source line (line 1 after the scroll reset).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("notes.md"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    await_marker(&mut ctrl, "line0");
    ctrl.set_content_viewport(80, 10);

    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::RenderedMarkdown),
        "precondition: a markdown file defaults to a transformed view"
    );

    ctrl.enter_line_select_at_top();
    // Deferred: not active yet, but a SyntaxContent override was set and the entry is queued.
    assert!(
        !ctrl.line_select_active(),
        "AC-15: deferred — line-select is not active until the source render lands"
    );
    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::SyntaxContent),
        "AC-15: the override switched the file to the source-mapped content view"
    );
    assert!(
        ctrl.line_select_pending(),
        "AC-15: the entry is queued against the switch render"
    );

    // Pump poll() until the switch render lands and line-select opens.
    let deadline = Instant::now() + Duration::from_secs(5);
    while ctrl.line_select_pending() {
        ctrl.poll();
        assert!(
            Instant::now() < deadline,
            "the deferred line-select never activated"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::SyntaxContent),
        "still in the source-mapped view after the render landed"
    );
    assert!(
        ctrl.line_select_active(),
        "AC-15: line-select is active once the source render lands"
    );
    assert_eq!(
        ctrl.line_selection(),
        Some((1, 1)),
        "AC-15: marker on the top visible source line (line 1 after the scroll reset)"
    );
}

#[test]
fn superseding_render_clears_pending() {
    // AC-15: after a deferred entry, a NEWER render dispatch (here view-cycle) supersedes it — the
    // queued entry is cleared and the stale render must NOT spuriously open line-select.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("notes.md"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    await_marker(&mut ctrl, "line0");
    ctrl.set_content_viewport(80, 10);

    ctrl.enter_line_select_at_top();
    assert!(
        ctrl.line_select_pending(),
        "precondition: the entry is queued"
    );

    // A newer render dispatch supersedes it (mirrors pending_goto's supersede via dispatch_render).
    ctrl.handle(Intent::CycleView);
    assert!(
        !ctrl.line_select_pending(),
        "AC-15: a newer render dispatch clears the pending line-select entry"
    );

    // Let the cycle render (and any stale one) drain; line-select must stay closed.
    await_marker(&mut ctrl, "line0");
    for _ in 0..5 {
        ctrl.poll();
    }
    assert!(
        !ctrl.line_select_active(),
        "AC-15: the superseded entry did not open line-select on the stale render"
    );
    assert!(!ctrl.line_select_pending(), "AC-15: pending stays cleared");
}

#[test]
fn enter_from_source_is_synchronous() {
    // AC-15 fast path: entering when the file is already source-mapped (SyntaxContent) AND its render
    // is up to date opens the modal immediately, with nothing queued.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    await_marker(&mut ctrl, "line0"); // a .rs file defaults to SyntaxContent; render landed
    ctrl.set_content_viewport(80, 10);
    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::SyntaxContent),
        "precondition: already in the source-mapped view"
    );
    ctrl.scroll_to_line(5); // line 5 at the top of the viewport

    ctrl.enter_line_select_at_top();
    assert!(
        !ctrl.line_select_pending(),
        "AC-15: the synchronous fast path queues nothing"
    );
    assert!(
        ctrl.line_select_active(),
        "AC-15: line-select opens immediately on an up-to-date source view"
    );
    assert_eq!(
        ctrl.line_selection(),
        Some((5, 5)),
        "marker on the top visible source line (5)"
    );
}

// ── T-7: the two confirms — Enter copies the `path:line` REFERENCE (the v1.9.0 contract),
// y/Y copy the selected lines' CONTENT; both sanitize, notify, and close the mode ────────────────

/// Enter line-select at line 5 on a source-view `.rs` file (synchronous fast path).
fn enter_at_line_five(ctrl: &mut Controller) {
    await_marker(ctrl, "line0");
    ctrl.set_content_viewport(80, 10);
    ctrl.scroll_to_line(5); // line 5 at the top of the viewport
    ctrl.enter_line_select_at_top();
    assert_eq!(
        ctrl.line_selection(),
        Some((5, 5)),
        "precondition: marker collapsed at line 5"
    );
}

#[test]
fn enter_copies_single_line_reference() {
    // AC-9/AC-10: Enter on a single-line selection copies the repo-relative `path:line`
    // REFERENCE — the shipped v1.9.0 contract, unchanged by the copy-content addition — and
    // confirms it in a notice; AC-4-style close: the mode is a completed action so Enter closes it.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_at_line_five(&mut ctrl);

    let fx = ctrl.handle_line_select_key(key(KeyCode::Enter));
    assert!(fx.redraw, "copying redraws to show the confirmation notice");
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("a.rs:5"),
        "AC-9: Enter copies the `path:line` reference (v1.9.0 contract)"
    );
    assert!(
        ctrl.notices().iter().any(|n| n == "Copied a.rs:5"),
        "AC-10: the copy is confirmed with the reference itself: {:?}",
        ctrl.notices()
    );
    assert!(
        !ctrl.line_select_active(),
        "Enter closes the mode after copying"
    );
}

#[test]
fn enter_copies_range_reference() {
    // AC-9: Enter on an extended selection copies the `path:start-end` range reference.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_at_line_five(&mut ctrl);

    // Extend the selection from 5 down to 8 (Shift+j, reported as `Char('J')`).
    ctrl.handle_line_select_key(key(KeyCode::Char('J')));
    ctrl.handle_line_select_key(key(KeyCode::Char('J')));
    ctrl.handle_line_select_key(key(KeyCode::Char('J')));
    assert_eq!(
        ctrl.line_selection(),
        Some((5, 8)),
        "precondition: selection 5-8"
    );

    ctrl.handle_line_select_key(key(KeyCode::Enter));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("a.rs:5-8"),
        "AC-9: Enter copies the `path:start-end` range reference"
    );
}

#[test]
fn range_copies_lines_joined_by_newline() {
    // `y` on an extended selection copies every selected line's content, joined with '\n'.
    // Selection 5-8 over MultiLine → "line4".."line7".
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_at_line_five(&mut ctrl);

    // Extend the selection from 5 down to 8 (Shift+j, reported as `Char('J')`).
    ctrl.handle_line_select_key(key(KeyCode::Char('J')));
    ctrl.handle_line_select_key(key(KeyCode::Char('J')));
    ctrl.handle_line_select_key(key(KeyCode::Char('J')));
    assert_eq!(
        ctrl.line_selection(),
        Some((5, 8)),
        "precondition: selection 5-8"
    );

    ctrl.handle_line_select_key(key(KeyCode::Char('y')));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("line4\nline5\nline6\nline7"),
        "y copies the range's content joined by newlines"
    );
    assert!(
        ctrl.notices().iter().any(|n| n == "Copied lines 5-8"),
        "the notice names the copied line range: {:?}",
        ctrl.notices()
    );
}

#[test]
fn copy_strips_line_number_gutter_keeps_indentation() {
    // The syntax view (`bat --style=numbers`) bakes a line-number gutter into the content. Copying
    // must yield the source text alone — no `  1 ` prefix — while preserving code indentation.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), GutterContent);
    await_marker(&mut ctrl, "fn main");
    ctrl.set_content_viewport(80, 10);
    ctrl.scroll_to_line(1);
    ctrl.enter_line_select_at_top();
    assert_eq!(
        ctrl.line_selection(),
        Some((1, 1)),
        "precondition: marker on line 1"
    );

    // Extend over all three lines (1 → 3) and copy the content with `y`.
    ctrl.handle_line_select_key(key(KeyCode::Char('J')));
    ctrl.handle_line_select_key(key(KeyCode::Char('J')));
    assert_eq!(ctrl.line_selection(), Some((1, 3)), "precondition: 1-3");

    ctrl.handle_line_select_key(key(KeyCode::Char('y')));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("fn main() {\n    let x = 5;\n}"),
        "the line-number gutter is stripped; the code indentation is preserved"
    );
}

#[test]
fn y_copies_single_line_content() {
    // The familiar copy keys `y`/`Y` copy the selection's CONTENT — the counterpart to Enter's
    // reference (line-select routes every key through `handle_line_select_key`, so these are
    // wired to the content-copy path explicitly). One selection, two products.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_at_line_five(&mut ctrl);

    let fx = ctrl.handle_line_select_key(key(KeyCode::Char('y')));
    assert!(fx.redraw, "y copies and redraws");
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("line4"),
        "y copies the selected line's content (source line 5 == \"line4\")"
    );
    assert!(
        ctrl.notices().iter().any(|n| n == "Copied line 5"),
        "the content copy is confirmed by line number: {:?}",
        ctrl.notices()
    );
    assert!(
        !ctrl.line_select_active(),
        "y closes the mode after copying"
    );
}

#[test]
fn control_bytes_in_content_are_sanitized_tabs_kept() {
    // AC-16: content is untrusted too — a crafted line can carry an ESC byte. The copied text is
    // filtered so no raw control byte reaches the clipboard, while a tab (indentation) survives.
    // ControlContent renders the single line "\tcode\x1bhere" → copy must yield "\tcodehere".
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), ControlContent);
    await_marker(&mut ctrl, "code");
    ctrl.set_content_viewport(80, 10);
    ctrl.enter_line_select_at_top();
    assert_eq!(
        ctrl.line_selection(),
        Some((1, 1)),
        "precondition: marker on the single line"
    );

    ctrl.handle_line_select_key(key(KeyCode::Char('y')));
    let log = copied.lock().unwrap();
    let got = log.last().map(String::as_str).unwrap();
    assert_eq!(
        got, "\tcodehere",
        "AC-16: the ESC byte is dropped from the content while the leading tab is preserved"
    );
    assert!(
        !got.chars().any(|c| c.is_control() && c != '\t'),
        "AC-16: no control byte (other than tab) reaches the clipboard: {got:?}"
    );
    assert!(
        ctrl.notices()
            .iter()
            .all(|n| !n.chars().any(char::is_control)),
        "AC-16: no control byte reaches the notice either: {:?}",
        ctrl.notices()
    );
}

#[test]
fn clipboard_error_shows_failure_notice_no_panic() {
    // AC-11: a clipboard write that fails surfaces a failure notice instead of panicking/exiting.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "placeholder\n").unwrap();
    let mut ctrl = controller_with(dir.path(), MultiLine, Box::new(FailingClipboard));
    enter_at_line_five(&mut ctrl);

    let fx = ctrl.handle_line_select_key(key(KeyCode::Enter));
    assert!(fx.redraw, "the failure notice redraws");
    assert!(
        ctrl.notices().iter().any(|n| n.contains("Could not copy")),
        "AC-11: a clipboard failure is surfaced as a notice: {:?}",
        ctrl.notices()
    );
    assert!(
        !ctrl.line_select_active(),
        "the mode closes even on a copy failure (the notice conveys the outcome)"
    );
}

// ── T-8: mouse — click sets marker (shift-click is treated as a plain click), double-click
// copies ──────────────────────────────────────────────────────────────────────────────────

/// A left mouse event (button `Up` unless overridden by the kind) with no modifier at `(col, row)`.
fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// The same event with the Shift modifier set (a real mouse modifier, unlike the uppercase-char
/// keyboard convention).
fn shift_mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::SHIFT,
    }
}

/// Geometry whose content interior starts at screen row 1 (`content_inner.y == 1`), so a click on
/// screen row `r` maps to content row `r - 1` — and, with `content_scroll == 0`, source line `r`.
fn content_geometry() -> PaneGeometry {
    PaneGeometry {
        content_inner: Some(Rect {
            x: 41,
            y: 1,
            width: 58,
            height: 20,
        }),
        divider_x: Some(40),
        ..PaneGeometry::default()
    }
}

/// Enter line-select at the top of the file (line 1) with the content geometry wired up, so a
/// click on the content pane hit-tests as `Content` and maps 1:1 to a source line.
fn enter_line_select_top(ctrl: &mut Controller) {
    await_marker(ctrl, "line0");
    ctrl.set_content_viewport(80, 20); // full 20-line body fits → content_scroll stays 0
    ctrl.set_pane_geometry(content_geometry());
    ctrl.enter_line_select_at_top();
    assert_eq!(
        ctrl.line_selection(),
        Some((1, 1)),
        "precondition: marker collapsed on the top line"
    );
}

#[test]
fn mouse_down_places_caret_on_clicked_line() {
    // A left press in the content pane drops the selection caret on the clicked source line,
    // collapsed. Screen row 3, content top row 1, scroll 0 → source line 3.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_line_select_top(&mut ctrl);

    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 50, 3));
    assert!(fx.redraw, "a press that places the caret redraws");
    assert_eq!(
        ctrl.line_selection(),
        Some((3, 3)),
        "a press places the caret on the clicked source line (3), collapsed"
    );
}

#[test]
fn shift_mouse_is_left_for_the_terminal() {
    // We must NOT swallow Shift+mouse — herdr and most terminals reserve it for their own native
    // text selection/copy. A Shift+press is therefore inert in the plugin: it neither starts a
    // selection nor redraws, so the event passes through to the terminal untouched.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_line_select_top(&mut ctrl);

    let fx = ctrl.handle_mouse(shift_mouse(MouseEventKind::Down(MouseButton::Left), 55, 7));
    assert!(!fx.redraw, "a Shift+press is inert (left for the terminal)");
    assert_eq!(
        ctrl.line_selection(),
        Some((1, 1)),
        "Shift+mouse must not move or start a selection in the plugin"
    );
}

#[test]
fn drag_selects_characters_across_lines_and_copies() {
    // Press → drag → release selects a character-granular span; `y` copies exactly that span
    // (the tail of the first line + the head of the last, joined by '\n'). MultiLine renders
    // "line0".."line19" with no gutter, so char carets map straight to columns.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_line_select_top(&mut ctrl);

    // Press at line 1, char 0 (col 42 = pane x 41 + 1 glyph col + 0).
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 42, 1));
    // Drag to line 2, char 3 (col 45 = 41 + 1 glyph + 3).
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 45, 2));
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 45, 2));
    assert!(fx.redraw, "finalizing the drag redraws");
    assert!(
        ctrl.line_select_active(),
        "release finalizes the selection but keeps the mode open for Enter/y"
    );

    ctrl.handle_line_select_key(key(KeyCode::Char('y')));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("line0\nlin"),
        "y copies the character span: all of line 1 + the first 3 chars of line 2"
    );
    assert!(
        ctrl.notices().iter().any(|n| n == "Copied selection"),
        "the notice confirms a character selection: {:?}",
        ctrl.notices()
    );
    assert!(
        !ctrl.line_select_active(),
        "y closes the mode after copying"
    );
}

#[test]
fn enter_after_drag_copies_reference_of_spanned_lines() {
    // Enter after a mouse drag copies the REFERENCE for the lines the drag spans — the same
    // selection can produce either product: `y` the dragged characters, Enter the `path:lines`.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_line_select_top(&mut ctrl);

    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 42, 1));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 45, 2));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 45, 2));

    ctrl.handle_line_select_key(key(KeyCode::Enter));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("code.rs:1-2"),
        "Enter copies the range reference for the dragged lines"
    );
}

#[test]
fn drag_populates_char_selection_in_the_view() {
    // A mouse drag must feed the presenter a character selection (so the highlight is char-granular,
    // not whole-line). After dragging line 1 char 0 → line 2 char 3, the view carries those carets.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_line_select_top(&mut ctrl);

    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 42, 1));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 45, 2));

    let vs = ctrl.view_state();
    let ls = vs.line_select.expect("line-select overlay is present");
    let cs = ls
        .char_sel
        .expect("a mouse drag populates the character selection for the overlay");
    assert_eq!(
        (cs.start_line, cs.start_col, cs.end_line, cs.end_col),
        (1, 0, 2, 3),
        "the overlay carries the exact drag carets (ordered)"
    );
}

#[test]
fn click_without_drag_collapses_and_y_copies_the_line() {
    // A press with no drag collapses the selection onto one character; `y` then falls back to
    // copying the whole clicked line, so a plain click-then-y still yields a line — and Enter
    // yields that line's reference. Row 3 → source line 3 == "line2".
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_line_select_top(&mut ctrl);

    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 50, 3));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50, 3));
    ctrl.handle_line_select_key(key(KeyCode::Char('y')));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("line2"),
        "a collapsed click copies the whole clicked line (source line 3 == \"line2\")"
    );

    // The same collapsed click, confirmed with Enter, yields the line's reference.
    ctrl.enter_line_select_at_top();
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 50, 3));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50, 3));
    ctrl.handle_line_select_key(key(KeyCode::Enter));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("code.rs:3"),
        "Enter on the collapsed click copies the clicked line's reference"
    );
}

#[test]
fn click_outside_content_is_inert() {
    // A click outside the content region is inert for the mode (it does not move the marker) and
    // does not leak to the columns. Row 3, column 5 is the tree column, not the content pane.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_line_select_top(&mut ctrl);

    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 5, 3));
    assert!(!fx.redraw, "a click outside the content pane is inert");
    assert_eq!(
        ctrl.line_selection(),
        Some((1, 1)),
        "the marker is unchanged by an out-of-content click"
    );
    assert!(
        ctrl.line_select_active(),
        "the mode stays open on an out-of-content click"
    );
}

#[test]
fn drag_within_one_line_selects_chars() {
    // A press + drag on the same row selects a character range within that single line; Enter
    // copies just those chars. Line 1 == "line0"; select chars [1, 4) → "ine".
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_line_select_top(&mut ctrl);

    // Press at line 1 char 1 (col 43 = pane x 41 + glyph + 1), drag to char 4 (col 46).
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 43, 1));
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 46, 1));
    assert!(fx.redraw, "a selection drag redraws");
    assert_eq!(
        ctrl.line_selection(),
        Some((1, 1)),
        "the selection stays on line 1 (character-granular within the line)"
    );

    ctrl.handle_line_select_key(key(KeyCode::Char('y')));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("ine"),
        "y copies the selected characters [1, 4) of \"line0\""
    );
}

#[test]
fn content_press_after_tree_click_places_caret_without_copying() {
    // Entering line-select right after a tree-row click, then a first content press at the same
    // screen row, must place the caret on that source line and copy nothing — a press is a caret
    // placement, never a copy (copying is Enter/y). Guards that a prior-context click can't cause a
    // spurious copy on the first content interaction.
    let dir = TempDir::new();
    for name in ["aa.txt", "bb.txt", "code.rs"] {
        std::fs::write(dir.path().join(name), "placeholder\n").unwrap();
    }
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    await_marker(&mut ctrl, "line0");
    ctrl.set_content_viewport(80, 20); // full 20-line body fits → content_scroll stays 0

    // Geometry with a tree interior AND a content interior that share screen row 3: tree row 3
    // (tree_inner.y == 1) maps to node index 2 — the 3rd of 3 files, "code.rs" — and content row 3
    // (content_inner.y == 1, content_scroll 0) maps to source line 3.
    let mut geometry = content_geometry();
    geometry.tree_inner = Some(Rect {
        x: 1,
        y: 1,
        width: 38,
        height: 20,
    });
    ctrl.set_pane_geometry(geometry);

    // A tree-row click at screen row 3 selects "code.rs" (idx 2) — the prior-context click.
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 6, 3));
    assert_eq!(
        ctrl.tree().cursor(),
        2,
        "precondition: the tree click selected the 3rd node (code.rs)"
    );
    await_marker(&mut ctrl, "line0"); // let the re-render triggered by the tree click land

    // Line-select is entered next (as `L` does on content focus), synchronously — the selection
    // is a source-mapped file and its render is fully applied.
    ctrl.enter_line_select_at_top();
    assert!(
        ctrl.line_select_active(),
        "precondition: the mode opened synchronously"
    );

    // The first content press, at the SAME screen row (3) the tree click used, places the caret on
    // source line 3 — and copies nothing.
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 50, 3));
    assert!(
        fx.redraw,
        "the first content press places the caret and redraws"
    );
    assert_eq!(
        ctrl.line_selection(),
        Some((3, 3)),
        "the first content press places the caret on the clicked source line"
    );
    assert!(
        ctrl.line_select_active(),
        "the mode stays open — a press never copies or closes it"
    );
    assert!(
        copied.lock().unwrap().is_empty(),
        "nothing is copied by a press"
    );
}

// ── wrap fix (copy-line-reference): line-select must map wrapped display rows ↔ source lines ──
// Regression guard for the round-1 gate finding: line-select assumed a 1:1 source-line→display-row
// mapping, but the `w` (ToggleWrap / wrap_override) toggle wraps EVERY mode including SyntaxContent,
// so entry, mouse click, and keep-marker-visible math each conflated a wrapped display-row offset
// with a source-line index — placing/copying the WRONG line (the feature's core output).

#[test]
fn entry_maps_wrapped_scroll_offset_to_source_line() {
    // With wrap ON, scrolling so source line 2 sits at the top puts `content_scroll` at display
    // row 3 (source line 1 wraps to 3 rows). Entry must land the marker on source line 2 — NOT on
    // `content_scroll + 1` == 4, the pre-fix behavior.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), WrapBody);
    await_marker(&mut ctrl, "aaa");
    ctrl.set_content_viewport(80, 10);
    ctrl.handle(Intent::ToggleWrap);
    assert!(ctrl.wrap_override(), "precondition: wrap override is on");
    ctrl.scroll_to_line(2); // wrap-aware → content_scroll == 3 (line 1 occupies rows 0-2)
    assert_eq!(
        ctrl.content_scroll(),
        3,
        "precondition: top display row is 3"
    );

    ctrl.enter_line_select_at_top();
    assert_eq!(
        ctrl.line_selection(),
        Some((2, 2)),
        "entry maps wrapped scroll offset 3 back to source line 2, not the buggy scroll+1 (=4)"
    );
}

#[test]
fn mouse_press_maps_wrapped_row_to_source_line() {
    // With wrap ON and scroll at the top, a press on screen row 4 (content top row 1 → display row
    // 3) must select source line 2 — display row 3 is the first wrapped row of line 2 — NOT the
    // pre-fix `scroll + (row - top) + 1` == 4.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), WrapBody);
    await_marker(&mut ctrl, "aaa");
    ctrl.set_content_viewport(80, 20);
    ctrl.set_pane_geometry(content_geometry());
    ctrl.handle(Intent::ToggleWrap);
    assert!(ctrl.wrap_override(), "precondition: wrap override is on");
    ctrl.enter_line_select_at_top();
    assert_eq!(
        ctrl.line_selection(),
        Some((1, 1)),
        "precondition: marker on line 1"
    );

    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 50, 4));
    assert!(fx.redraw, "a press that places the caret redraws");
    assert_eq!(
        ctrl.line_selection(),
        Some((2, 2)),
        "press on wrapped display row 3 maps to source line 2, not the buggy row+1 (=4)"
    );
}

#[test]
fn marker_stays_visible_under_wrap_when_moved_past_viewport() {
    // AC-7 under wrap: as the marker moves down, the content must scroll so the marker's WRAPPED
    // display row stays within the viewport — computed with the same wrapped mapping, not `marker-1`.
    // WrapBody at width 80 lays out source lines 1..=10 at these 0-based display-row offsets (line 1
    // spans rows 0-2, line 2 rows 3-4, then one row each):
    let row_of = [0usize, 3, 5, 6, 7, 8, 9, 10, 11, 12]; // index = source line - 1
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), WrapBody);
    await_marker(&mut ctrl, "aaa");
    let height = 4usize;
    ctrl.set_content_viewport(80, height as u16);
    ctrl.handle(Intent::ToggleWrap);
    assert!(ctrl.wrap_override(), "precondition: wrap override is on");
    ctrl.enter_line_select_at_top();

    // Walk the marker to the last source line; after each step its wrapped display row must stay
    // within the visible window [content_scroll, content_scroll + height). Pre-fix the scroll math
    // used `marker - 1` as the row and would leave the marker off-screen under wrap.
    for _ in 0..9 {
        ctrl.handle_line_select_key(key(KeyCode::Char('j')));
        let (_, marker) = ctrl.line_selection().unwrap();
        let row = row_of[marker - 1];
        let scroll = ctrl.content_scroll() as usize;
        assert!(
            row >= scroll && row < scroll + height,
            "marker source line {marker} (display row {row}) must stay within [{scroll}, {})",
            scroll + height
        );
    }
    assert_eq!(
        ctrl.content_scroll(),
        9,
        "at the last line (display row 12) the bottom pins to the marker: 12 + 1 - 4 = 9"
    );
}

// ── ambient content-pane selection (mouse drag with NO modal open) ──────────────────────────
// Click-drag selects text during normal navigation WITHOUT entering L mode; the release
// auto-copies the span. Because no modal is open the content overlay prepends NO gutter glyph, so
// a char caret at column C maps to screen column `pane.x + C` (41 + C) — one less than the L-mode
// drag tests above, which add the glyph column (41 + 1 + C).

/// Await the content render and wire up the content geometry WITHOUT entering line-select, so a
/// mouse drag in the content pane exercises the ambient (`Modal::None`) path.
fn setup_ambient(ctrl: &mut Controller) {
    await_marker(ctrl, "line0");
    ctrl.set_content_viewport(80, 20); // the full 20-line body fits → content_scroll stays 0
    ctrl.set_pane_geometry(content_geometry());
    assert!(
        !ctrl.line_select_active(),
        "precondition: no modal is open (the ambient path)"
    );
}

/// Drag line 1 char 0 → line 2 char 3 in the ambient path (cols 41/44 = pane x + charcol, no
/// glyph), leaving a non-collapsed selection standing after the release.
fn ambient_drag_line1_to_line2(ctrl: &mut Controller) {
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 41, 1));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 44, 2));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 44, 2));
}

#[test]
fn ambient_drag_auto_copies_on_release() {
    // A press → drag → release in the content pane with NO modal open selects a character span and
    // copies it on release (auto-copy), leaving the highlight standing and never entering L mode.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    setup_ambient(&mut ctrl);

    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 41, 1));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 44, 2));
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 44, 2));

    assert!(fx.redraw, "the release redraws");
    assert!(
        !ctrl.line_select_active(),
        "an ambient selection never enters L mode"
    );
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("line0\nlin"),
        "release auto-copies the span: all of line 1 + the first 3 chars of line 2"
    );
    assert!(
        ctrl.notices().iter().any(|n| n == "Copied selection"),
        "the notice confirms the copy: {:?}",
        ctrl.notices()
    );
    assert!(
        ctrl.view_state().content_selection.is_some(),
        "the selection stays highlighted after the auto-copy (feedback)"
    );
}

#[test]
fn ambient_drag_populates_content_selection_overlay() {
    // A mouse drag feeds the presenter a gutter-less character selection via view_state, distinct
    // from the L-mode `line_select` overlay (which stays None — no modal is open).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    setup_ambient(&mut ctrl);

    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 41, 1));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 44, 2));

    let vs = ctrl.view_state();
    assert!(
        vs.line_select.is_none(),
        "the L-mode overlay is not used for an ambient drag"
    );
    let cs = vs
        .content_selection
        .expect("the ambient selection overlay is populated");
    assert_eq!(
        (cs.start_line, cs.start_col, cs.end_line, cs.end_col),
        (1, 0, 2, 3),
        "the overlay carries the exact drag carets (ordered), gutter-less"
    );
    assert_eq!(cs.gutter, 0, "MultiLine has no bat gutter → gutter width 0");
}

#[test]
fn ambient_plain_click_focuses_and_copies_nothing() {
    // A press with no drag (Down then Up at the same cell) is a plain click: it focuses the content
    // pane, leaves no standing selection, and copies nothing.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    setup_ambient(&mut ctrl);

    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 50, 3));
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50, 3));

    assert!(fx.redraw, "the click focuses the content pane (redraw)");
    assert_eq!(
        ctrl.focus(),
        Focus::Content,
        "a content click focuses the content pane"
    );
    assert!(
        ctrl.view_state().content_selection.is_none(),
        "a plain click leaves no standing selection"
    );
    assert!(
        copied.lock().unwrap().is_empty(),
        "a plain click copies nothing"
    );
}

#[test]
fn ambient_press_outside_content_clears_selection() {
    // Once a selection stands, a left press anywhere outside the content region drops it
    // (click-away deselect). Column 5 is the tree/outside region (content starts at x 41).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    setup_ambient(&mut ctrl);
    ambient_drag_line1_to_line2(&mut ctrl);
    assert!(
        ctrl.view_state().content_selection.is_some(),
        "precondition: a selection stands"
    );

    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 5, 3));
    assert!(fx.redraw, "clearing the selection redraws");
    assert!(
        ctrl.view_state().content_selection.is_none(),
        "a press outside the content region clears the standing selection"
    );
}

#[test]
fn ambient_esc_clears_selection_before_quitting() {
    // Esc (Intent::Close) dismisses a standing ambient selection first, before it would unzoom or
    // quit — the outermost layer of the Esc stack. It must redraw and NOT quit.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    setup_ambient(&mut ctrl);
    ambient_drag_line1_to_line2(&mut ctrl);
    assert!(
        ctrl.view_state().content_selection.is_some(),
        "precondition: a selection stands"
    );

    let fx = ctrl.handle(Intent::Close);
    assert!(fx.redraw, "dismissing the selection redraws");
    assert!(
        !fx.quit,
        "the first Esc dismisses the selection, it does not quit"
    );
    assert!(
        ctrl.view_state().content_selection.is_none(),
        "Esc clears the ambient selection"
    );
}

#[test]
fn ambient_selection_cleared_on_content_change() {
    // Any displayed-content change drops a standing selection so a stale highlight never maps onto
    // different text. Cycling the view re-renders through dispatch_render, which clears it.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    setup_ambient(&mut ctrl);
    ambient_drag_line1_to_line2(&mut ctrl);
    assert!(
        ctrl.view_state().content_selection.is_some(),
        "precondition: a selection stands"
    );

    ctrl.handle(Intent::CycleView);
    assert!(
        ctrl.view_state().content_selection.is_none(),
        "a content change (view cycle → dispatch_render) clears the selection"
    );
}

#[test]
fn ambient_shift_drag_is_left_for_the_terminal() {
    // Shift+mouse is reserved for the terminal's own native selection — it must not start an
    // ambient selection nor redraw.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    setup_ambient(&mut ctrl);

    let fx = ctrl.handle_mouse(shift_mouse(MouseEventKind::Down(MouseButton::Left), 45, 3));
    assert!(!fx.redraw, "a Shift+press is inert (left for the terminal)");
    assert!(
        ctrl.view_state().content_selection.is_none(),
        "Shift+mouse must not start an ambient selection"
    );
}

#[test]
fn entering_line_select_clears_an_ambient_selection() {
    // The two selections never coexist: entering L mode drops any standing ambient selection so L
    // starts with a fresh line marker, not the inherited mouse span.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    setup_ambient(&mut ctrl);
    ambient_drag_line1_to_line2(&mut ctrl);
    assert!(
        ctrl.view_state().content_selection.is_some(),
        "precondition: a selection stands"
    );

    ctrl.enter_line_select_at_top();
    assert!(ctrl.line_select_active(), "L mode is now active");
    let vs = ctrl.view_state();
    assert!(
        vs.content_selection.is_none(),
        "the ambient selection is cleared on entering L mode"
    );
    assert_eq!(
        ctrl.line_selection(),
        Some((1, 1)),
        "L mode starts with a fresh line-1 marker, not the ambient span"
    );
}

#[test]
fn ambient_drag_over_directory_guidance_copies_nothing() {
    // The pane can show first-party guidance ("Directory: select a file to view") when a directory
    // is selected. Dragging over it must copy nothing — the is_file guard, shared with the L-mode
    // copy path. A temp dir whose only entry is a subdirectory selects that directory by default.
    let dir = TempDir::new();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("inner.rs"), "x\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    await_marker(&mut ctrl, "Directory:"); // the directory-selected guidance ("Directory: select a file to view")
    ctrl.set_content_viewport(80, 20);
    ctrl.set_pane_geometry(content_geometry());

    // Drag across the guidance line (line 1) and release.
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 41, 1));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 50, 1));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50, 1));

    assert!(
        copied.lock().unwrap().is_empty(),
        "dragging over directory guidance copies nothing (is_file guard)"
    );
}

#[test]
fn held_ambient_press_does_not_leak_into_line_select() {
    // A still-held ambient press must not bleed into L mode: entering L mode (the `L` key) mid-press
    // resets the in-flight drag, so the next mouse-move is inert for L mode. Without the reset the
    // held `Drag::ContentSelect` would extend the fresh L-mode selection from (top, col 0) to the
    // cursor — a character selection the user never initiated with an L-mode press.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    setup_ambient(&mut ctrl);

    // Press and HOLD in the content pane (ambient selection begins; drag == ContentSelect).
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 41, 1));
    // Enter L mode without releasing the button.
    ctrl.enter_line_select_at_top();
    assert!(
        ctrl.line_select_active(),
        "precondition: L mode is now active"
    );

    // A held-mouse move must be inert for L mode — no L-mode press ever happened.
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 55, 4));
    assert_eq!(
        ctrl.line_selection(),
        Some((1, 1)),
        "the L-mode selection stays collapsed on the top line — the held ambient drag did not leak in"
    );
    let ls = ctrl
        .view_state()
        .line_select
        .expect("L-mode overlay present");
    assert!(
        ls.char_sel.is_none(),
        "a leaked drag would have produced a character selection; there must be none"
    );

    // `y` copies the whole top line (line granularity), not an unintended dragged span.
    ctrl.handle_line_select_key(key(KeyCode::Char('y')));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("line0"),
        "y copies the whole top line (line 1 == \"line0\"), not a dragged char span"
    );
}

#[test]
fn ambient_drag_over_rendering_placeholder_copies_nothing() {
    // While a render is in flight the pane shows the non-empty "Rendering…" placeholder with a FILE
    // still selected — so the is_file guard does NOT block it. A press over the placeholder must not
    // seed a selection (the `content_rendering` press-time guard), so a drag copies nothing.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    // Do NOT poll: the pane is still showing the "Rendering…" placeholder (content_rendering == true).
    ctrl.set_content_viewport(80, 20);
    ctrl.set_pane_geometry(content_geometry());
    assert!(
        ctrl.view_state().content_rendering,
        "precondition: a render is in flight (the placeholder is shown)"
    );

    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 41, 1));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 45, 1));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 45, 1));

    assert!(
        ctrl.view_state().content_selection.is_none(),
        "no selection is seeded over the render placeholder"
    );
    assert!(
        copied.lock().unwrap().is_empty(),
        "dragging over the \"Rendering…\" placeholder copies nothing"
    );
}

// ── wrapped views: mouse selection is char-granular through the break-position map ──────────
// Under wrap (prose by default, or the `w` override) a source line spans several display rows.
// `char_at_content_col` maps a click through `text_layout::wrap_row_starts` — the SAME greedy
// word-packing simulation the wrapped scroll math counts rows with (`wrapped_rows` is defined
// from it, so they can never drift) — so the caret lands on the character actually under the
// cursor, and a drag copies exactly the swept span. WrapBody at width 80: source line 1
// ("a"×170, one word) char-breaks at [0, 80, 160] (display rows 0-2); line 2 ("b"×90) at
// [0, 80] (rows 3-4). Screen row = display row + 1 (content top y=1), ambient column = 41 + col.

/// Await WrapBody, wire the geometry, and turn the `w` wrap override on WITHOUT entering L mode.
fn setup_ambient_wrapped(ctrl: &mut Controller) {
    await_marker(ctrl, "aaa");
    ctrl.set_content_viewport(80, 20);
    ctrl.set_pane_geometry(content_geometry());
    ctrl.handle(Intent::ToggleWrap);
    assert!(ctrl.wrap_override(), "precondition: wrap override is on");
    assert!(!ctrl.line_select_active(), "precondition: no modal open");
}

#[test]
fn wrapped_ambient_drag_copies_the_exact_char_span() {
    // Press on screen row 1 col 50 → display row 0 → line 1, row-start 0, caret 9. Drag to screen
    // row 4 col 50 → display row 3 → line 2's FIRST wrapped row, caret 9. The copy is exactly the
    // swept span: the tail of line 1 from char 9 + the head of line 2 up to char 9.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), WrapBody);
    setup_ambient_wrapped(&mut ctrl);

    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 50, 1));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 50, 4));
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50, 4));

    assert!(fx.redraw, "the release redraws");
    let expected = format!("{}\n{}", "a".repeat(161), "b".repeat(9));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some(expected.as_str()),
        "the wrapped drag copies the exact char span (line 1 from char 9 + line 2 up to char 9)"
    );
    assert!(
        ctrl.notices().iter().any(|n| n == "Copied selection"),
        "the notice confirms the selection copy: {:?}",
        ctrl.notices()
    );
}

#[test]
fn wrapped_drag_across_rows_of_one_line_selects_its_chars() {
    // Screen rows 1 and 2 are both display rows OF SOURCE LINE 1 (an "a"×170 word char-broken at
    // 80). Press row 1 col 45 → caret 4; drag row 2 col 60 → row-start 80 + col 19 = caret 99.
    // The copy is chars [4, 99) of that single line — a word-or-two selection inside one wrapped
    // paragraph, the exact gesture line-granularity couldn't do.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), WrapBody);
    setup_ambient_wrapped(&mut ctrl);

    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 45, 1));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 60, 2));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 60, 2));

    let expected = "a".repeat(95);
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some(expected.as_str()),
        "chars [4, 99) of the wrapped line — mapped through its row starts [0, 80, 160]"
    );
}

#[test]
fn wrapped_word_break_maps_columns_to_the_right_word() {
    // Word wrap (not just char wrap): a 50-char word + a 40-char word at width 80 breaks BEFORE
    // the second word — row 1 starts at char 51 (index 50 is the separator space, which stays on
    // row 0). A drag on row 1 cols 3..8 must therefore select chars [54, 59) == "bbbbb", proving
    // the caret map uses real break positions, not a naive row*width offset.
    #[derive(Clone, Copy)]
    struct WordWrapBody;
    impl ContentProvider for WordWrapBody {
        fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
            RenderResult {
                content: Text::raw(format!("{} {}", "a".repeat(50), "b".repeat(40))),
                notices: Vec::new(),
                source: None,
            }
        }
    }
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), WordWrapBody);
    await_marker(&mut ctrl, "aaa");
    ctrl.set_content_viewport(80, 20);
    ctrl.set_pane_geometry(content_geometry());
    ctrl.handle(Intent::ToggleWrap);
    assert!(ctrl.wrap_override(), "precondition: wrap override is on");

    // Screen row 2 = display row 1 = the second word's row (starts at char 51).
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 44, 2)); // col 3 → caret 54
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 49, 2)); // col 8 → caret 59
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 49, 2));

    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("bbbbb"),
        "row 1 begins at the second WORD (char 51) — the break-position map, not row×width"
    );
}

#[test]
fn wrapped_ambient_plain_click_copies_nothing() {
    // A press + release with no drag under wrap is still a plain click: focus only, no selection,
    // no copy.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), WrapBody);
    setup_ambient_wrapped(&mut ctrl);

    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 50, 2));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50, 2));

    assert!(
        copied.lock().unwrap().is_empty(),
        "a plain click under wrap copies nothing"
    );
    assert!(
        ctrl.view_state().content_selection.is_none(),
        "no selection highlight is left standing by a plain click"
    );
}

#[test]
fn wrapped_l_mode_drag_selects_chars_and_both_confirms_work() {
    // In L mode under wrap the same mapping applies (minus the 1-col ▶ glyph): a press places the
    // char caret, a drag extends it; `y` copies the swept chars and Enter the range reference.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), WrapBody);
    await_marker(&mut ctrl, "aaa");
    ctrl.set_content_viewport(80, 20);
    ctrl.set_pane_geometry(content_geometry());
    ctrl.handle(Intent::ToggleWrap);
    assert!(ctrl.wrap_override(), "precondition: wrap override is on");
    ctrl.enter_line_select_at_top();

    // Press screen row 1 col 50 → disp col 8 (pane x 41 + 1 glyph) → line 1 caret 8; drag to
    // screen row 4 col 50 → line 2 (its first wrapped row) caret 8.
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 50, 1));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 50, 4));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50, 4));
    assert_eq!(
        ctrl.line_selection(),
        Some((1, 2)),
        "the drag spans source lines 1-2"
    );

    // y copies the swept char span …
    ctrl.handle_line_select_key(key(KeyCode::Char('y')));
    let expected = format!("{}\n{}", "a".repeat(162), "b".repeat(8));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some(expected.as_str()),
        "y copies the char span under wrap (line 1 from char 8 + line 2 up to char 8)"
    );

    // … and Enter (on a fresh identical selection) the range reference.
    ctrl.enter_line_select_at_top();
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 50, 1));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 50, 4));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50, 4));
    ctrl.handle_line_select_key(key(KeyCode::Enter));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("code.rs:1-2"),
        "Enter copies the range reference for the wrapped drag"
    );
}

// ── retained source: copies are byte-faithful and the gutter is source-anchored ─────────────
// A source-mapped render retains the file's raw lines (`RenderResult::source`, 1:1 with the
// displayed lines). Whole-line copies then return the SOURCE text verbatim (real tabs — the
// renderer expands them to spaces on screen — and no gutter parsing at all), and the gutter
// width is anchored by matching the displayed line against its source instead of guessed from
// leading digits — killing the heuristic's one false positive (a gutter-less line that happens
// to start with its own line number).

/// Bat-style displayed lines (gutter + tab expanded to 4 spaces) with the REAL source retained,
/// as the live renderer supplies for `SyntaxContent`.
#[derive(Clone, Copy)]
struct GutterWithSource;
impl ContentProvider for GutterWithSource {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        let displayed = ["   1 fn main() {", "   2     let x = 5;", "   3 }"];
        RenderResult {
            content: Text::raw(displayed.join("\n")),
            notices: Vec::new(),
            source: Some(vec![
                "fn main() {".to_string(),
                "\tlet x = 5;".to_string(), // REAL tab in the file; bat shows 4 spaces
                "}".to_string(),
            ]),
        }
    }
}

/// A gutter-less view (plain-text fallback) whose line 3 happens to START with "3 " — the exact
/// input that fools the digit heuristic — with the source retained to anchor the truth.
#[derive(Clone, Copy)]
struct PlainWithSource;
impl ContentProvider for PlainWithSource {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        let lines = ["fn main() {", "    let x = 5;", "3 loops below"];
        RenderResult {
            content: Text::raw(lines.join("\n")),
            notices: Vec::new(),
            source: Some(vec![
                "fn main() {".to_string(),
                "\tlet x = 5;".to_string(),
                "3 loops below".to_string(),
            ]),
        }
    }
}

#[test]
fn line_copy_returns_source_verbatim_real_tabs_no_gutter() {
    // Whole-line copies come from the retained source: the bat gutter never needs stripping and
    // the file's REAL tab indentation survives (the display shows expanded spaces).
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), GutterWithSource);
    await_marker(&mut ctrl, "fn main");
    ctrl.set_content_viewport(80, 10);
    ctrl.enter_line_select_at_top();

    ctrl.handle_line_select_key(key(KeyCode::Char('J'))); // extend 1 → 2
    ctrl.handle_line_select_key(key(KeyCode::Char('y')));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("fn main() {\n\tlet x = 5;"),
        "the copy is the SOURCE text: no gutter, and the real tab survives (not 4 spaces)"
    );
}

#[test]
fn gutterless_line_starting_with_its_own_number_is_not_mis_stripped() {
    // Line 3 of a gutter-less view reads "3 loops below" — the digit heuristic alone would strip
    // the leading "3 " as a gutter. With the source retained the copy is anchored to the truth.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), PlainWithSource);
    await_marker(&mut ctrl, "fn main");
    ctrl.set_content_viewport(80, 10);
    ctrl.enter_line_select_at_top();

    ctrl.handle_line_select_key(key(KeyCode::Char('j'))); // 1 → 2
    ctrl.handle_line_select_key(key(KeyCode::Char('j'))); // 2 → 3
    ctrl.handle_line_select_key(key(KeyCode::Char('y')));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("3 loops below"),
        "a line that merely starts with its own number keeps its leading digits"
    );
}

#[test]
fn char_drag_gutter_is_source_anchored_not_guessed() {
    // The same false-positive line, selected char-granularly with the mouse: the source-anchored
    // gutter is 0 (the displayed line IS the source), so carets [0, 3) map to "3 l" — the digit
    // heuristic would have mis-based every caret by the phantom 2-char "gutter".
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), PlainWithSource);
    await_marker(&mut ctrl, "fn main");
    ctrl.set_content_viewport(80, 10);
    ctrl.set_pane_geometry(content_geometry());
    ctrl.enter_line_select_at_top();

    // L-mode geometry: content x=41 + 1 overlay glyph col. Row 3 → source line 3.
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 42, 3));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 45, 3));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 45, 3));
    ctrl.handle_line_select_key(key(KeyCode::Char('y')));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("3 l"),
        "carets are based off the source-anchored gutter (0), not the heuristic's guess"
    );
}

#[test]
fn sourceless_view_ambient_copy_keeps_a_leading_number() {
    // A source-less view (rendered markdown / diff) is reachable for a copy only via the AMBIENT
    // drag — entering L mode auto-switches to source. Line 2 reads "2 spaces of intro"; there is no
    // gutter, so `gutter_len_of` returns 0 with no source to anchor and the leading "2 " survives.
    // The bare digit heuristic would have wrongly eaten it. Guards the source-less half of the fix.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("doc.md"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), PlainNoSource);
    await_marker(&mut ctrl, "intro");
    ctrl.set_content_viewport(80, 20);
    ctrl.set_pane_geometry(content_geometry());
    assert!(
        !ctrl.line_select_active(),
        "precondition: the ambient path, no modal open"
    );

    // Drag across all of line 2 (screen row 2 with content_inner.y == 1, scroll 0): cols 41..58
    // (pane x 41 + the 17 chars of "2 spaces of intro"), no L-mode glyph.
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 41, 2));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 58, 2));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 58, 2));
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("2 spaces of intro"),
        "no source, no gutter: the leading \"2 \" is real text and survives the copy"
    );
}

#[test]
fn drag_past_the_viewport_edge_autoscrolls_the_content() {
    // Dragging an ambient selection below the bottom (or above the top) of the content viewport
    // nudges the scroll one line so the selection can extend past what is on screen; a drag inside
    // the viewport leaves the scroll alone.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    await_marker(&mut ctrl, "line0");
    // A short viewport (height 5) over the 20-line body, so there is room to scroll (max 15).
    let geom = PaneGeometry {
        content_inner: Some(Rect {
            x: 41,
            y: 1,
            width: 58,
            height: 5,
        }),
        divider_x: Some(40),
        ..PaneGeometry::default()
    };
    ctrl.set_content_viewport(80, 5);
    ctrl.set_pane_geometry(geom);
    assert_eq!(ctrl.content_scroll(), 0, "precondition: not scrolled");

    // Press inside the pane to start the ambient drag (content_inner bottom edge is row y+h = 6).
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 41, 1));
    // Drag below the bottom edge → scroll down one line, twice.
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 44, 6));
    assert_eq!(
        ctrl.content_scroll(),
        1,
        "a drag below the bottom scrolls down one"
    );
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 44, 6));
    assert_eq!(
        ctrl.content_scroll(),
        2,
        "another edge drag scrolls one more"
    );
    // Drag back inside the viewport → no scroll change.
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 44, 3));
    assert_eq!(
        ctrl.content_scroll(),
        2,
        "a drag inside the viewport leaves the scroll alone"
    );
    // Drag above the top edge (row < y = 1) → scroll up one line.
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 44, 0));
    assert_eq!(
        ctrl.content_scroll(),
        1,
        "a drag above the top scrolls up one"
    );
}

#[test]
fn source_control_bytes_are_still_scrubbed() {
    // The retained source is untrusted file bytes — the AC-16 control scrub applies to it exactly
    // as to display extraction (tabs kept, ESC dropped).
    #[derive(Clone, Copy)]
    struct HostileSource;
    impl ContentProvider for HostileSource {
        fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
            RenderResult {
                content: Text::raw("clean view"),
                notices: Vec::new(),
                source: Some(vec!["\tcode\x1b[2Jhere".to_string()]),
            }
        }
    }
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), HostileSource);
    await_marker(&mut ctrl, "clean");
    ctrl.set_content_viewport(80, 10);
    ctrl.enter_line_select_at_top();

    ctrl.handle_line_select_key(key(KeyCode::Char('y')));
    let log = copied.lock().unwrap();
    let got = log.last().map(String::as_str).unwrap();
    assert_eq!(
        got, "\tcode[2Jhere",
        "the ESC byte is dropped from the source-backed copy; the tab survives"
    );
    assert!(
        !got.chars().any(|c| c.is_control() && c != '\t'),
        "no control byte (other than tab) reaches the clipboard: {got:?}"
    );
}

#[test]
fn wrapped_break_dropped_space_does_not_shift_selection_to_the_line_above() {
    // Regression (owner-caught live): ratatui DROPS the whitespace at a row break, so a line of
    // two exact-width words renders in FEWER rows than ceil(chars/width) — the old char-wrap
    // floor overcounted its rows, shifting the row→line mapping so a click on the next line
    // selected the line ABOVE. Line 1 ("x"×40 + " " + "y"×40) renders as exactly 2 rows at
    // width 40; the click on display row 2 (screen row 3) must therefore select line 2.
    #[derive(Clone, Copy)]
    struct DropBreakBody;
    impl ContentProvider for DropBreakBody {
        fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
            let line1 = format!("{} {}", "x".repeat(40), "y".repeat(40));
            RenderResult {
                content: Text::raw(format!("{line1}\nsecond\nthird")),
                notices: Vec::new(),
                source: None,
            }
        }
    }
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), DropBreakBody);
    await_marker(&mut ctrl, "second");
    ctrl.set_content_viewport(40, 20);
    ctrl.set_pane_geometry(content_geometry());
    ctrl.handle(Intent::ToggleWrap);
    assert!(ctrl.wrap_override(), "precondition: wrap override is on");

    // Screen row 3 = display row 2 = the FIRST row after line 1's two wrapped rows → line 2.
    // Drag chars [2, 6) of "second" and release: the copy must come from line 2, not line 1.
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 43, 3));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 47, 3));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 47, 3));

    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("cond"),
        "the click on display row 2 maps to line 2 (\"second\") — not the line above"
    );
}
