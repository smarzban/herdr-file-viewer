//! Line-select modal state (copy-line-reference, T-3): entering places the marker on the top
//! visible source line (AC-1) and exiting closes the modal without touching the clipboard (AC-4).
//! Every side-effecting component is stubbed, so these tests touch no real git / renderer / editor
//! and read back a recording clipboard to prove the exit path copies nothing.

mod common;

use common::{RecordingClipboard, TempDir, resolved};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herdr_file_viewer::controller::{
    Components, ContentProvider, Controller, EditorHandoff, EditorOutcome, GitService,
    RenderResult, RootProviders,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::presenter::Focus;
use herdr_file_viewer::view_policy::ViewMode;
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

/// Build a non-git controller over `root` with the given content provider and a recording
/// clipboard, returning the controller plus a handle to the clipboard's copy log so a test can
/// assert what (if anything) was copied.
fn controller_with_clipboard(
    root: &Path,
    content: impl ContentProvider + Clone + 'static,
) -> (Controller, Arc<Mutex<Vec<String>>>) {
    let clipboard = RecordingClipboard::default();
    let copied = clipboard.copied.clone();
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(StubGit),
            content: Box::new(content.clone()),
        }),
        editor: Box::new(NoopEditor),
        clipboard: Box::new(clipboard),
        renderers: None,
    };
    let ctrl = Controller::new(
        resolved(root.to_path_buf(), false),
        Baseline::Head,
        components,
    );
    (ctrl, copied)
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
