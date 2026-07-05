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

// ── T-7: Enter copies the reference, sanitizes, notifies, and closes the mode ─────

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
fn enter_copies_single_reference() {
    // AC-9/AC-10: Enter on a single-line selection copies `path:line` and confirms it in a notice;
    // AC-4-style close: the mode is a completed action so Enter also closes it.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_at_line_five(&mut ctrl);

    let fx = ctrl.handle_line_select_key(key(KeyCode::Enter));
    assert!(fx.redraw, "copying redraws to show the confirmation notice");
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("a.rs:5"),
        "AC-9: Enter copies the single-line reference"
    );
    assert!(
        ctrl.notices().iter().any(|n| n == "Copied a.rs:5"),
        "AC-10: the copy is confirmed: {:?}",
        ctrl.notices()
    );
    assert!(
        !ctrl.line_select_active(),
        "Enter closes the mode after copying"
    );
}

#[test]
fn range_copies_start_end() {
    // AC-9: Enter on an extended selection copies `path:start-end`.
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
        "AC-9: Enter copies the range reference"
    );
}

#[test]
fn control_bytes_in_path_are_sanitized() {
    // AC-16: the path segment is untrusted — a crafted file name can carry an ESC byte. The whole
    // reference is sanitized before it reaches the clipboard OR the notice, so no raw control byte
    // is copied.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a\x1bb.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_at_line_five(&mut ctrl);

    ctrl.handle_line_select_key(key(KeyCode::Enter));
    let log = copied.lock().unwrap();
    let got = log.last().map(String::as_str).unwrap();
    assert_eq!(
        got, "ab.rs:5",
        "AC-16: the ESC byte in the file name is neutralized before copy"
    );
    assert!(
        !got.chars().any(char::is_control),
        "AC-16: no control byte reaches the clipboard: {got:?}"
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
fn click_sets_marker_to_clicked_source_line() {
    // AC-8: a plain left-click (release) in the content pane moves the marker to the clicked
    // source line, collapsed. Screen row 3, content top row 1, scroll 0 → source line 3.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_line_select_top(&mut ctrl);

    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50, 3));
    assert!(fx.redraw, "a click that moves the marker redraws");
    assert_eq!(
        ctrl.line_selection(),
        Some((3, 3)),
        "AC-8: a click places the marker on the clicked source line (3)"
    );
}

#[test]
fn shift_click_places_marker_like_a_plain_click() {
    // Amended T-8: herdr and most terminals reserve Shift+mouse for their own native text
    // selection, so a shift-click can never reliably reach the plugin — mouse shift-click
    // extend is removed. A Shift+left-click now behaves exactly like a plain click: it places
    // the marker on the clicked source line, collapsed (anchor collapses to the clicked line,
    // NOT extended from a prior anchor). Keyboard `Shift`+`j`/`k` (and Shift+arrows) remains the
    // supported way to extend a multi-line selection — that path is unchanged.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_line_select_top(&mut ctrl);

    // Plain click at row 4 → marker at line 4 (this would be the "anchor" under the old
    // extend behavior, but a subsequent shift-click no longer honors it).
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50, 4));
    assert_eq!(
        ctrl.line_selection(),
        Some((4, 4)),
        "precondition: marker placed at line 4"
    );

    // Shift-click at row 7 → marker moves to 7, collapsed — NOT (4, 7).
    let fx = ctrl.handle_mouse(shift_mouse(MouseEventKind::Up(MouseButton::Left), 55, 7));
    assert!(fx.redraw, "a shift-click that moves the marker redraws");
    assert_eq!(
        ctrl.line_selection(),
        Some((7, 7)),
        "shift-click places the marker on the clicked line like a plain click, not extending"
    );
}

#[test]
fn double_click_copies_reference() {
    // AC-9: two left-clicks on the same content row within the double-click window copy the
    // `path:line` reference for that row and close the mode — the same confirm as Enter.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_line_select_top(&mut ctrl);

    // First click moves the marker to line 3; the second (same row, within the window) is the
    // double-click that copies and closes.
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50, 3));
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50, 3));
    assert!(
        fx.redraw,
        "the copy redraws to show the confirmation notice"
    );
    assert_eq!(
        copied.lock().unwrap().last().map(String::as_str),
        Some("code.rs:3"),
        "AC-9: double-click copies the reference for the clicked line"
    );
    assert!(
        !ctrl.line_select_active(),
        "AC-9: the double-click closes the mode (like Enter)"
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
fn press_and_drag_do_not_move_the_marker() {
    // Only a completed left-click (Up) acts; a press (Down) or drag must be inert so the mode keeps
    // the mouse and no divider/scrollbar drag starts underneath.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("code.rs"), "placeholder\n").unwrap();
    let (mut ctrl, _copied) = controller_with_clipboard(dir.path(), MultiLine);
    enter_line_select_top(&mut ctrl);

    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 50, 6));
    assert!(!fx.redraw, "a press is inert in line-select mode");
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 50, 6));
    assert!(!fx.redraw, "a drag is inert in line-select mode");
    assert_eq!(
        ctrl.line_selection(),
        Some((1, 1)),
        "neither a press nor a drag moves the marker"
    );
}

#[test]
fn stale_tree_click_does_not_misfire_first_content_click_as_double() {
    // BUG regression: entering line-select mode did not clear `self.last_click`, so a tree-row
    // click made just before entry could pair with the FIRST content click at the same screen row
    // as a double-click — `is_double_click` only compares the row (not the column/pane) — firing
    // `copy_line_reference` (copy + close) before the marker was ever placed by a real content
    // click. Every other modal already guards this (`open_finder`/`open_help` clear `last_click`
    // on open); line-select's entry point must too.
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

    // A tree-row click at screen row 3 selects "code.rs" (idx 2) and sets
    // `last_click = (_, 3, now)` — the stale prior-context click.
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

    // The FIRST content click, at the SAME screen row (3) the stale tree click used, must place
    // the marker on line 3 — not pair with the stale tree click as a double-click.
    let fx = ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50, 3));
    assert!(
        fx.redraw,
        "the first content click places the marker and redraws"
    );
    assert_eq!(
        ctrl.line_selection(),
        Some((3, 3)),
        "the first content click must place the marker — not fire a stale cross-context double-click"
    );
    assert!(
        ctrl.line_select_active(),
        "the mode must stay open — a stale pairing must not copy+close it before any real content click"
    );
    assert!(
        copied.lock().unwrap().is_empty(),
        "nothing should have been copied by a stale-click misfire"
    );
}
