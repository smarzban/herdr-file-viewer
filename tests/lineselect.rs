//! Line-select modal state (copy-line-reference, T-3): entering places the marker on the top
//! visible source line (AC-1) and exiting closes the modal without touching the clipboard (AC-4).
//! Every side-effecting component is stubbed, so these tests touch no real git / renderer / editor
//! and read back a recording clipboard to prove the exit path copies nothing.

mod common;

use common::{RecordingClipboard, TempDir, resolved};
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
