//! Phase 2 controller integration for session-only file annotations.

mod common;

use common::{TempDir, git, init_repo_with_commit, resolved};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use herdr_file_viewer::annotation::LineRange;
use herdr_file_viewer::controller::{
    Clipboard, Components, ContentProvider, Controller, EditorHandoff, EditorOutcome, GitService,
    RenderResult, RootProviders,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::presenter::PaneGeometry;
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::layout::Rect;
use ratatui::text::Text;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct StubGit;

impl GitService for StubGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }

    fn changed_set(&self, _baseline: Baseline) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }

    fn diff(&self, _path: &Path, _baseline: Baseline, _full: bool) -> String {
        String::new()
    }
}

struct NoopEditor;

impl EditorHandoff for NoopEditor {
    fn open(&mut self, _file: &Path) -> EditorOutcome {
        EditorOutcome::NoTakeover
    }
}

#[derive(Clone, Copy)]
struct Lines {
    source_mapped: bool,
}

impl ContentProvider for Lines {
    fn render(&self, _path: &Path, _mode: ViewMode, _diff: Option<&str>) -> RenderResult {
        let lines = (1..=10)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>();
        RenderResult {
            content: Text::raw(lines.join("\n")),
            notices: Vec::new(),
            source: self.source_mapped.then_some(lines),
        }
    }
}

#[derive(Default)]
struct ClipboardState {
    calls: Vec<String>,
    fail: bool,
}

#[derive(Clone)]
struct TestClipboard(Arc<Mutex<ClipboardState>>);

impl Clipboard for TestClipboard {
    fn copy(&mut self, text: &str) -> io::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(text.to_string());
        if state.fail {
            Err(io::Error::other("clipboard unavailable"))
        } else {
            Ok(())
        }
    }
}

fn controller_with_mapping(
    root: &Path,
    fail_clipboard: bool,
    source_mapped: bool,
) -> (Controller, Arc<Mutex<ClipboardState>>) {
    let state = Arc::new(Mutex::new(ClipboardState {
        fail: fail_clipboard,
        ..ClipboardState::default()
    }));
    let clipboard = TestClipboard(Arc::clone(&state));
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(StubGit),
            content: Box::new(Lines { source_mapped }),
        }),
        editor: Box::new(NoopEditor),
        clipboard: Box::new(clipboard),
        renderers: None,
    };
    (
        Controller::new(
            resolved(root.to_path_buf(), false),
            Baseline::Head,
            components,
        ),
        state,
    )
}

fn controller(root: &Path, fail_clipboard: bool) -> (Controller, Arc<Mutex<ClipboardState>>) {
    controller_with_mapping(root, fail_clipboard, true)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn shifted(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)
}

fn type_editor(ctrl: &mut Controller, text: &str) {
    for c in text.chars() {
        ctrl.handle_annotation_editor_key(key(KeyCode::Char(c)));
    }
}

fn save_editor(ctrl: &mut Controller) {
    ctrl.handle_annotation_editor_key(key(KeyCode::Enter));
}

fn add_file_annotation(ctrl: &mut Controller, text: &str) {
    ctrl.handle(Intent::AddAnnotation);
    assert!(ctrl.annotation_editor().is_some());
    type_editor(ctrl, text);
    save_editor(ctrl);
    assert!(!ctrl.annotation_modal_open());
}

fn await_lines(ctrl: &mut Controller) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text = ctrl
            .content()
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        if text.contains("line 10") {
            return;
        }
        ctrl.poll();
        assert!(Instant::now() < deadline, "content never rendered");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn add_range_annotation(ctrl: &mut Controller, start: usize, end: usize, text: &str) {
    await_lines(ctrl);
    ctrl.set_content_viewport(80, 10);
    ctrl.enter_line_select_at_top();
    for _ in 1..start {
        ctrl.handle_line_select_key(key(KeyCode::Char('j')));
    }
    assert_eq!(ctrl.line_selection(), Some((start, start)));
    let move_key = if end >= start { 'J' } else { 'K' };
    for _ in 0..start.abs_diff(end) {
        ctrl.handle_line_select_key(key(KeyCode::Char(move_key)));
    }
    ctrl.handle_line_select_key(key(KeyCode::Char('a')));
    type_editor(ctrl, text);
    save_editor(ctrl);
}

fn annotation_lines(ctrl: &Controller, index: usize) -> Option<LineRange> {
    ctrl.annotations().ordered()[index].target().lines()
}

fn content_geometry() -> PaneGeometry {
    PaneGeometry {
        content_inner: Some(Rect {
            x: 41,
            y: 1,
            width: 58,
            height: 10,
        }),
        divider_x: Some(40),
        ..PaneGeometry::default()
    }
}

fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn indicator_projection_is_root_joined_deduplicated_merged_and_immediate() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "a\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "b\n").unwrap();
    let (mut ctrl, _) = controller(dir.path(), false);
    await_lines(&mut ctrl);

    add_file_annotation(&mut ctrl, "file a");
    add_range_annotation(&mut ctrl, 4, 2, "first range");
    add_range_annotation(&mut ctrl, 4, 6, "adjacent range");
    add_range_annotation(&mut ctrl, 9, 9, "separate range");
    let a = ctrl.root().join("a.rs");
    let view = ctrl.view_state().annotation_indicators;
    assert_eq!(
        view.annotated_files.into_iter().collect::<Vec<_>>(),
        vec![a.clone()]
    );
    assert!(view.displayed_file_annotated);
    assert_eq!(
        view.displayed_line_ranges,
        vec![LineRange::new(2, 6).unwrap(), LineRange::new(9, 9).unwrap()]
    );

    ctrl.handle(Intent::ShowAnnotations);
    ctrl.handle_annotations_key(key(KeyCode::Enter));
    ctrl.handle_annotation_editor_key(key(KeyCode::End));
    type_editor(&mut ctrl, " revised");
    save_editor(&mut ctrl);
    assert_eq!(
        ctrl.view_state()
            .annotation_indicators
            .displayed_line_ranges,
        vec![LineRange::new(2, 6).unwrap(), LineRange::new(9, 9).unwrap()],
        "editing a note preserves its immutable target projection"
    );
    ctrl.handle_annotations_key(key(KeyCode::Esc));

    ctrl.handle(Intent::NavDown);
    let loading = ctrl.view_state().annotation_indicators;
    assert!(
        loading.displayed_file_annotated,
        "the projection follows the old applied content path, not the new live cursor"
    );
    assert!(
        loading.displayed_line_ranges.is_empty(),
        "a render placeholder has no source mapping, even while the old applied path is retained"
    );
    await_lines(&mut ctrl);
    assert!(
        !ctrl
            .view_state()
            .annotation_indicators
            .displayed_file_annotated,
        "the unannotated file becomes displayed only when its render lands"
    );
    add_file_annotation(&mut ctrl, "file b");
    let b = ctrl.root().join("b.rs");
    let view = ctrl.view_state().annotation_indicators;
    assert_eq!(
        view.annotated_files.into_iter().collect::<Vec<_>>(),
        vec![a, b]
    );
    assert!(view.displayed_file_annotated);
    assert!(view.displayed_line_ranges.is_empty());
}

#[test]
fn indicator_projection_requires_source_mapping_for_numeric_ranges() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "a\n").unwrap();
    let (mut ctrl, _) = controller_with_mapping(dir.path(), false, false);
    await_lines(&mut ctrl);
    add_range_annotation(&mut ctrl, 2, 4, "transformed");

    let view = ctrl.view_state().annotation_indicators;
    assert!(
        view.displayed_file_annotated,
        "the applied file/title remains marked"
    );
    assert!(
        view.displayed_line_ranges.is_empty(),
        "transformed or otherwise unmapped content never guesses source lines"
    );
}

#[test]
fn indicator_projection_tracks_delete_clear_and_successful_reroot_lifecycle() {
    let first = TempDir::new();
    std::fs::write(first.path().join("a.rs"), "a\n").unwrap();
    let second = TempDir::new();
    std::fs::write(second.path().join("b.rs"), "b\n").unwrap();
    let (mut ctrl, _) = controller(first.path(), false);
    await_lines(&mut ctrl);
    add_file_annotation(&mut ctrl, "one");
    add_range_annotation(&mut ctrl, 2, 3, "two");

    ctrl.handle(Intent::ShowAnnotations);
    ctrl.handle_annotations_key(key(KeyCode::Char('d')));
    assert!(
        ctrl.view_state()
            .annotation_indicators
            .displayed_file_annotated,
        "the remaining range keeps the file marked after one delete"
    );
    ctrl.handle_annotations_key(key(KeyCode::Char('D')));
    assert_eq!(
        ctrl.view_state().annotation_indicators,
        Default::default(),
        "clear-all removes every indicator immediately"
    );
    ctrl.handle_annotations_key(key(KeyCode::Esc));

    add_file_annotation(&mut ctrl, "reroot");
    let before = ctrl.view_state().annotation_indicators;
    ctrl.re_root(first.path());
    assert_eq!(
        ctrl.view_state().annotation_indicators,
        before,
        "same-root retains"
    );
    ctrl.re_root(&first.path().join("missing"));
    assert_eq!(
        ctrl.view_state().annotation_indicators,
        before,
        "failed root retains"
    );
    // A real switch confirms first now; Enter proceeds and clears the indicators with the store.
    ctrl.re_root(second.path());
    ctrl.handle_discard_confirm_key(key(KeyCode::Enter));
    assert_eq!(ctrl.view_state().annotation_indicators, Default::default());
}

#[test]
fn file_creation_and_normalized_empty_validation() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, _) = controller(dir.path(), false);

    assert!(ctrl.annotations().is_empty(), "a fresh controller is empty");
    ctrl.handle(Intent::AddAnnotation);
    type_editor(&mut ctrl, " \t\n");
    save_editor(&mut ctrl);
    assert!(
        ctrl.annotation_editor().is_some(),
        "invalid save stays open"
    );
    assert_eq!(
        ctrl.annotation_editor().unwrap().error(),
        Some("Annotation text cannot be empty")
    );
    assert!(ctrl.annotations().is_empty());

    type_editor(&mut ctrl, " Keep\tnote ");
    save_editor(&mut ctrl);
    let annotation = ctrl.annotations().ordered()[0];
    assert_eq!(annotation.target().path(), Path::new("a.rs"));
    assert_eq!(annotation.target().lines(), None);
    assert_eq!(annotation.text(), "Keep note");
    assert!(
        !ctrl.annotation_modal_open(),
        "add save returns to normal mode"
    );
}

#[test]
fn directory_and_empty_selection_refuse_add_nonfatally() {
    let dir = TempDir::new();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let (mut directory_ctrl, _) = controller(dir.path(), false);
    let fx = directory_ctrl.handle(Intent::AddAnnotation);
    assert!(fx.redraw && !fx.quit);
    assert!(directory_ctrl.annotation_editor().is_none());
    assert!(
        directory_ctrl
            .action_notice()
            .unwrap()
            .contains("Directories")
    );

    let empty = TempDir::new();
    let (mut empty_ctrl, _) = controller(empty.path(), false);
    empty_ctrl.handle(Intent::AddAnnotation);
    assert!(empty_ctrl.annotation_editor().is_none());
    assert!(
        empty_ctrl
            .action_notice()
            .unwrap()
            .contains("Select a file")
    );
}

#[test]
fn normal_add_cancel_returns_to_normal_without_saving() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, _) = controller(dir.path(), false);
    ctrl.handle(Intent::AddAnnotation);
    type_editor(&mut ctrl, "discard me");
    ctrl.handle_annotation_editor_key(key(KeyCode::Esc));
    assert!(!ctrl.annotation_modal_open());
    assert!(ctrl.annotations().is_empty());
}

#[test]
fn reversed_line_selection_saves_a_normalized_range_and_returns_to_normal() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, _) = controller(dir.path(), false);

    add_range_annotation(&mut ctrl, 5, 2, "reverse");
    assert_eq!(
        annotation_lines(&ctrl, 0),
        Some(LineRange::new(2, 5).unwrap())
    );
    assert!(!ctrl.line_select_active());
    assert!(ctrl.annotation_editor().is_none());
}

#[test]
fn line_select_cancel_restores_the_exact_mouse_selection_snapshot() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, _) = controller(dir.path(), false);
    await_lines(&mut ctrl);
    ctrl.set_content_viewport(80, 10);
    ctrl.set_pane_geometry(content_geometry());
    ctrl.enter_line_select_at_top();

    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 45, 2));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 49, 5));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 49, 5));
    let before_view = ctrl.view_state().line_select.unwrap();
    let before_marker = before_view.marker;
    let before = before_view.char_sel.unwrap();
    let exact_before = (
        before.start_line,
        before.start_col,
        before.end_line,
        before.end_col,
        before.gutter,
    );

    ctrl.handle_line_select_key(key(KeyCode::Char('a')));
    assert_eq!(
        ctrl.annotation_editor().unwrap().target().lines(),
        Some(LineRange::new(2, 5).unwrap()),
        "a character span becomes its covering line range"
    );
    ctrl.handle_annotation_editor_key(key(KeyCode::Esc));

    let after_view = ctrl.view_state().line_select.unwrap();
    assert_eq!(
        after_view.marker, before_marker,
        "cancel restores the directed marker, not only the normalized span"
    );
    let after = after_view.char_sel.unwrap();
    assert_eq!(
        (
            after.start_line,
            after.start_col,
            after.end_line,
            after.end_col,
            after.gutter,
        ),
        exact_before,
        "cancel restores line and character carets exactly"
    );
    ctrl.handle_line_select_key(key(KeyCode::Char('K')));
    assert_eq!(
        ctrl.line_selection(),
        Some((2, 4)),
        "extending after cancel proves the original line-2 anchor remained fixed"
    );
    assert!(ctrl.annotations().is_empty());
}

#[test]
fn mouse_span_save_uses_covering_lines_and_does_not_restore_line_select() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, _) = controller(dir.path(), false);
    await_lines(&mut ctrl);
    ctrl.set_content_viewport(80, 10);
    ctrl.set_pane_geometry(content_geometry());
    ctrl.enter_line_select_at_top();
    ctrl.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 45, 7));
    ctrl.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 49, 3));
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 49, 3));

    ctrl.handle_line_select_key(key(KeyCode::Char('a')));
    type_editor(&mut ctrl, "mouse range");
    save_editor(&mut ctrl);
    assert_eq!(
        annotation_lines(&ctrl, 0),
        Some(LineRange::new(3, 7).unwrap())
    );
    assert!(!ctrl.line_select_active(), "save returns to normal mode");
}

#[test]
fn overview_edit_cancel_save_and_delete_preserve_and_clamp_cursor() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, _) = controller(dir.path(), false);
    add_file_annotation(&mut ctrl, "one");
    add_file_annotation(&mut ctrl, "two");

    ctrl.handle(Intent::ShowAnnotations);
    ctrl.handle_annotations_key(key(KeyCode::Char('j')));
    assert_eq!(ctrl.annotation_list().unwrap().cursor(), 1);
    ctrl.handle_annotations_key(key(KeyCode::Char('e')));
    assert_eq!(ctrl.annotation_editor().unwrap().text(), "two");
    ctrl.handle_annotation_editor_key(key(KeyCode::Esc));
    assert_eq!(
        ctrl.annotation_list().unwrap().cursor(),
        1,
        "edit cancel returns to the same row"
    );

    ctrl.handle_annotations_key(key(KeyCode::Enter));
    ctrl.handle_annotation_editor_key(key(KeyCode::Home));
    type_editor(&mut ctrl, "revised ");
    save_editor(&mut ctrl);
    assert_eq!(
        ctrl.annotation_list().unwrap().cursor(),
        1,
        "edit save returns to the same row"
    );
    assert_eq!(ctrl.annotations().ordered()[1].text(), "revised two");

    ctrl.handle_annotations_key(key(KeyCode::Char('d')));
    assert_eq!(ctrl.annotations().len(), 1);
    assert_eq!(
        ctrl.annotation_list().unwrap().cursor(),
        0,
        "delete clamps the cursor"
    );
    ctrl.handle_annotations_key(key(KeyCode::Char('d')));
    assert!(ctrl.annotations().is_empty());
    assert_eq!(ctrl.annotation_list().unwrap().cursor(), 0);
    assert!(
        ctrl.annotation_list().is_some(),
        "empty overview remains visible"
    );
}

#[test]
fn overview_fixed_navigation_keys_and_q_close_are_modal_local() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, _) = controller(dir.path(), false);
    add_file_annotation(&mut ctrl, "one");
    add_file_annotation(&mut ctrl, "two");
    add_file_annotation(&mut ctrl, "three");
    ctrl.handle(Intent::ShowAnnotations);

    ctrl.handle_annotations_key(key(KeyCode::Down));
    assert_eq!(ctrl.annotation_list().unwrap().cursor(), 1);
    ctrl.handle_annotations_key(key(KeyCode::Char('j')));
    assert_eq!(ctrl.annotation_list().unwrap().cursor(), 2);
    ctrl.handle_annotations_key(key(KeyCode::Up));
    assert_eq!(ctrl.annotation_list().unwrap().cursor(), 1);
    ctrl.handle_annotations_key(key(KeyCode::Char('k')));
    assert_eq!(ctrl.annotation_list().unwrap().cursor(), 0);
    ctrl.handle_annotations_key(key(KeyCode::Char('q')));
    assert!(!ctrl.annotation_modal_open());
    assert_eq!(ctrl.annotations().len(), 3);
}

#[test]
fn clear_all_is_one_shift_d_and_never_calls_clipboard() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, clipboard) = controller(dir.path(), false);
    add_file_annotation(&mut ctrl, "file one");
    add_file_annotation(&mut ctrl, "file two");
    add_range_annotation(&mut ctrl, 2, 4, "range one");
    add_range_annotation(&mut ctrl, 7, 6, "range two");
    assert_eq!(ctrl.annotations().len(), 4);

    ctrl.handle(Intent::ShowAnnotations);
    ctrl.handle_annotations_key(key(KeyCode::Char('j')));
    ctrl.handle_annotations_key(shifted('D'));

    assert_eq!(ctrl.annotations().len(), 0);
    assert_eq!(ctrl.annotation_list().unwrap().cursor(), 0);
    assert!(
        ctrl.annotation_list().is_some(),
        "empty overview stays open"
    );
    assert!(
        clipboard.lock().unwrap().calls.is_empty(),
        "clear-all never copies"
    );
}

#[test]
fn uppercase_d_without_a_separate_shift_bit_also_clears_all() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, clipboard) = controller(dir.path(), false);
    add_file_annotation(&mut ctrl, "one");
    add_file_annotation(&mut ctrl, "two");
    ctrl.handle(Intent::ShowAnnotations);

    ctrl.handle_annotations_key(key(KeyCode::Char('D')));

    assert!(ctrl.annotations().is_empty());
    assert_eq!(ctrl.annotation_list().unwrap().cursor(), 0);
    assert!(ctrl.annotation_list().is_some());
    assert!(clipboard.lock().unwrap().calls.is_empty());
}

#[test]
fn editor_printables_including_d_are_text_and_cursor_keys_edit_unicode_safely() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, clipboard) = controller(dir.path(), false);
    add_file_annotation(&mut ctrl, "existing");

    ctrl.handle(Intent::AddAnnotation);
    type_editor(&mut ctrl, "qed");
    ctrl.handle_annotation_editor_key(shifted('D'));
    type_editor(&mut ctrl, "yé");
    assert_eq!(ctrl.annotation_editor().unwrap().text(), "qedDyé");
    assert_eq!(
        ctrl.annotations().len(),
        1,
        "editor D did not clear existing annotations"
    );
    assert!(
        clipboard.lock().unwrap().calls.is_empty(),
        "editor y did not copy"
    );

    ctrl.handle_annotation_editor_key(key(KeyCode::Home));
    ctrl.handle_annotation_editor_key(key(KeyCode::Right));
    type_editor(&mut ctrl, "X");
    ctrl.handle_annotation_editor_key(key(KeyCode::Delete));
    ctrl.handle_annotation_editor_key(key(KeyCode::End));
    ctrl.handle_annotation_editor_key(key(KeyCode::Backspace));
    ctrl.handle_annotation_editor_key(key(KeyCode::Left));
    assert_eq!(ctrl.annotation_editor().unwrap().text(), "qXdDy");
    assert_eq!(ctrl.annotation_editor().unwrap().cursor(), 4);
    save_editor(&mut ctrl);
    assert_eq!(ctrl.annotations().len(), 2);
}

#[test]
fn copy_all_is_byte_exact_closes_on_success_and_preserves_store() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, clipboard) = controller(dir.path(), false);
    add_file_annotation(&mut ctrl, " First\tnote ");
    add_range_annotation(&mut ctrl, 4, 2, "Use <safe> & exact.");
    let expected = "<file-annotations>\n- a.rs -> First note\n- a.rs:2-4 -> Use &lt;safe&gt; &amp; exact.\n</file-annotations>";

    ctrl.handle(Intent::ShowAnnotations);
    ctrl.handle_annotations_key(key(KeyCode::Char('y')));
    assert!(
        !ctrl.annotation_modal_open(),
        "successful copy closes overview"
    );
    assert_eq!(
        ctrl.annotations().len(),
        2,
        "copy does not mutate annotations"
    );
    let state = clipboard.lock().unwrap();
    assert_eq!(state.calls.len(), 1);
    assert_eq!(state.calls[0].as_bytes(), expected.as_bytes());
}

#[test]
fn copy_failure_still_closes_and_empty_copy_stays_open_without_calling() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut failing, clipboard) = controller(dir.path(), true);
    add_file_annotation(&mut failing, "kept");
    failing.handle(Intent::ShowAnnotations);
    failing.handle_annotations_key(key(KeyCode::Char('y')));
    assert!(
        !failing.annotation_modal_open(),
        "failed copy closes overview"
    );
    assert_eq!(failing.annotations().len(), 1);
    assert!(failing.action_notice().unwrap().contains("Could not copy"));
    let state = clipboard.lock().unwrap();
    assert_eq!(state.calls.len(), 1);
    assert_eq!(
        state.calls[0].as_bytes(),
        b"<file-annotations>\n- a.rs -> kept\n</file-annotations>"
    );
    drop(state);

    let empty_dir = TempDir::new();
    std::fs::write(empty_dir.path().join("a.rs"), "source\n").unwrap();
    let (mut empty, empty_clipboard) = controller(empty_dir.path(), false);
    empty.handle(Intent::ShowAnnotations);
    let fx = empty.handle_annotations_key(key(KeyCode::Char('y')));
    assert!(!fx.redraw);
    assert!(empty.annotation_list().is_some());
    assert!(empty_clipboard.lock().unwrap().calls.is_empty());
}

#[test]
fn successful_root_change_clears_and_reports_but_failed_and_same_root_retain() {
    let first = TempDir::new();
    std::fs::write(first.path().join("a.rs"), "source\n").unwrap();
    let second = TempDir::new();
    std::fs::write(second.path().join("b.rs"), "source\n").unwrap();
    let (mut ctrl, _) = controller(first.path(), false);
    add_file_annotation(&mut ctrl, "one");
    add_file_annotation(&mut ctrl, "two");

    ctrl.re_root(first.path());
    assert_eq!(
        ctrl.annotations().len(),
        2,
        "same-root no-op retains annotations"
    );
    assert!(ctrl.action_notice().is_none());

    ctrl.re_root(&first.path().join("missing"));
    assert_eq!(
        ctrl.annotations().len(),
        2,
        "failed root retains annotations"
    );
    assert!(
        ctrl.action_notice()
            .unwrap()
            .contains("cannot switch worktree")
    );

    // A REAL switch would discard them, so it now confirms first (the same guard `q` gets) rather
    // than clearing and reporting it after the fact. The no-op/failure paths above are untouched:
    // the guard sits after both early-returns, so neither raises a confirm.
    ctrl.re_root(second.path());
    assert!(
        ctrl.discard_confirm_open(),
        "a real switch confirms before discarding"
    );
    assert_eq!(ctrl.annotations().len(), 2, "nothing is cleared yet");

    // Enter proceeds with the switch, discarding them and reporting it exactly as before.
    ctrl.handle_discard_confirm_key(key(KeyCode::Enter));
    assert!(ctrl.annotations().is_empty());
    assert!(
        ctrl.action_notice()
            .unwrap()
            .contains("Cleared 2 annotations")
    );
}

#[test]
fn annotation_modals_absorb_mouse_input() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "a\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "b\n").unwrap();
    let (mut ctrl, _) = controller(dir.path(), false);
    ctrl.set_pane_geometry(PaneGeometry {
        tree_inner: Some(Rect::new(1, 1, 30, 10)),
        ..PaneGeometry::default()
    });

    ctrl.handle(Intent::ShowAnnotations);
    ctrl.handle_mouse(mouse(MouseEventKind::ScrollDown, 5, 2));
    assert_eq!(ctrl.tree().cursor(), 0);
    ctrl.handle_annotations_key(key(KeyCode::Esc));

    ctrl.handle(Intent::AddAnnotation);
    ctrl.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 5, 2));
    assert_eq!(ctrl.tree().cursor(), 0);
    assert!(ctrl.annotation_editor().is_some());
}

fn snapshot_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let mut entries = std::fs::read_dir(dir)
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.is_file() {
                out.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn annotation_workflows_leave_every_file_byte_and_git_state_unchanged() {
    let repo = TempDir::new();
    init_repo_with_commit(repo.path());
    std::fs::write(repo.path().join("a.rs"), "uncommitted\n").unwrap();
    let before_status = git(
        repo.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let before_files = snapshot_files(repo.path());

    let state = Arc::new(Mutex::new(ClipboardState::default()));
    let clipboard = TestClipboard(Arc::clone(&state));
    let components = Components {
        providers: Box::new(|_resolved| RootProviders {
            git: Arc::new(StubGit),
            content: Box::new(Lines {
                source_mapped: true,
            }),
        }),
        editor: Box::new(NoopEditor),
        clipboard: Box::new(clipboard),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        resolved(repo.path().to_path_buf(), true),
        Baseline::Head,
        components,
    );
    add_file_annotation(&mut ctrl, "file");
    add_range_annotation(&mut ctrl, 3, 1, "range");
    ctrl.handle(Intent::ShowAnnotations);
    ctrl.handle_annotations_key(key(KeyCode::Char('e')));
    ctrl.handle_annotation_editor_key(key(KeyCode::End));
    type_editor(&mut ctrl, " revised");
    save_editor(&mut ctrl);
    ctrl.handle_annotations_key(key(KeyCode::Char('y')));
    ctrl.handle(Intent::ShowAnnotations);
    ctrl.handle_annotations_key(shifted('D'));

    let after_status = git(
        repo.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let after_files = snapshot_files(repo.path());
    assert_eq!(
        after_files, before_files,
        "all repository and .git bytes are unchanged"
    );
    assert_eq!(
        after_status, before_status,
        "git worktree/index state is unchanged"
    );
}

// --- Quit confirm (session-only annotations are destroyed by quitting) ---

#[test]
fn quit_with_no_annotations_is_unguarded() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, _clipboard) = controller(dir.path(), false);

    let fx = ctrl.handle(Intent::Close);
    assert!(fx.quit, "an empty store quits straight through");
    assert!(!ctrl.discard_confirm_open());
}

#[test]
fn quit_with_annotations_confirms_instead_of_quitting() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, _clipboard) = controller(dir.path(), false);
    add_file_annotation(&mut ctrl, "kept");

    let fx = ctrl.handle(Intent::Close);
    assert!(!fx.quit, "the store is not empty, so the close is held");
    assert!(ctrl.discard_confirm_open());
    assert_eq!(ctrl.annotations().len(), 1, "confirming does not mutate");
}

#[test]
fn quit_confirm_y_copies_then_quits() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, clipboard) = controller(dir.path(), false);
    add_file_annotation(&mut ctrl, "kept");
    ctrl.handle(Intent::Close);

    let fx = ctrl.handle_discard_confirm_key(key(KeyCode::Char('y')));
    assert!(fx.quit, "a successful copy quits");
    assert!(!ctrl.discard_confirm_open());
    let state = clipboard.lock().unwrap();
    assert_eq!(state.calls.len(), 1);
    assert_eq!(
        state.calls[0].as_bytes(),
        b"<file-annotations>\n- a.rs -> kept\n</file-annotations>",
        "y copies the same canonical export the overview does"
    );
}

#[test]
fn quit_confirm_q_quits_and_discards_without_touching_the_clipboard() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, clipboard) = controller(dir.path(), false);
    add_file_annotation(&mut ctrl, "kept");
    ctrl.handle(Intent::Close);

    let fx = ctrl.handle_discard_confirm_key(key(KeyCode::Char('q')));
    assert!(fx.quit, "q quits anyway");
    assert!(!ctrl.discard_confirm_open());
    assert!(
        clipboard.lock().unwrap().calls.is_empty(),
        "discarding never writes the clipboard"
    );
}

#[test]
fn quit_confirm_esc_cancels_back_to_the_viewer() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, clipboard) = controller(dir.path(), false);
    add_file_annotation(&mut ctrl, "kept");
    ctrl.handle(Intent::Close);
    assert!(
        ctrl.discard_confirm_open(),
        "precondition: the dialog is up"
    );

    let fx = ctrl.handle_discard_confirm_key(key(KeyCode::Esc));
    assert!(!fx.quit, "esc returns to the viewer");
    assert!(!ctrl.discard_confirm_open());
    assert_eq!(ctrl.annotations().len(), 1, "the annotations survive");
    assert!(clipboard.lock().unwrap().calls.is_empty());
}

#[test]
fn quit_confirm_holds_open_when_the_clipboard_write_fails() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, _clipboard) = controller(dir.path(), true);
    add_file_annotation(&mut ctrl, "kept");
    ctrl.handle(Intent::Close);

    let fx = ctrl.handle_discard_confirm_key(key(KeyCode::Char('y')));
    assert!(
        !fx.quit,
        "a failed copy must not quit: that would destroy what y promised to save"
    );
    assert!(
        ctrl.discard_confirm_open(),
        "the dialog stays up with the error"
    );
    assert!(ctrl.action_notice().unwrap().contains("Could not copy"));
    assert_eq!(ctrl.annotations().len(), 1);
}

#[test]
fn quit_confirm_sits_outside_the_unzoom_layer() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, _clipboard) = controller(dir.path(), false);
    add_file_annotation(&mut ctrl, "kept");
    ctrl.handle(Intent::ToggleZoom);

    let fx = ctrl.handle(Intent::Close);
    assert!(!fx.quit, "the first close unzooms");
    assert!(
        !ctrl.discard_confirm_open(),
        "unzooming is not a quit, so it raises no confirm"
    );

    let fx = ctrl.handle(Intent::Close);
    assert!(!fx.quit);
    assert!(
        ctrl.discard_confirm_open(),
        "the second close reaches the quit layer"
    );
}

#[test]
fn quit_confirm_owns_every_key_so_none_leaks_to_a_global_action() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, clipboard) = controller(dir.path(), false);
    add_file_annotation(&mut ctrl, "kept");
    ctrl.handle(Intent::Close);

    for code in [
        KeyCode::Char('e'),
        KeyCode::Char('j'),
        KeyCode::Char('A'),
        KeyCode::Enter,
        KeyCode::Char('Z'),
    ] {
        let fx = ctrl.handle_discard_confirm_key(key(code));
        assert!(!fx.quit, "{code:?} must not quit");
        assert!(
            ctrl.discard_confirm_open(),
            "{code:?} must not close the dialog"
        );
    }
    assert!(clipboard.lock().unwrap().calls.is_empty());
    assert_eq!(ctrl.annotations().len(), 1);
}

#[test]
fn confirm_discard_false_quits_without_confirming() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, clipboard) = controller(dir.path(), false);
    ctrl.apply_confirm_discard(false);
    add_file_annotation(&mut ctrl, "kept");

    let fx = ctrl.handle(Intent::Close);
    assert!(fx.quit, "the opt-out restores the immediate-quit behavior");
    assert!(!ctrl.discard_confirm_open(), "no confirm is raised");
    assert!(
        clipboard.lock().unwrap().calls.is_empty(),
        "opting out of the confirm never writes the clipboard"
    );
}

#[test]
fn confirm_discard_defaults_on() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "source\n").unwrap();
    let (mut ctrl, _clipboard) = controller(dir.path(), false);
    add_file_annotation(&mut ctrl, "kept");

    // No `apply_*` call: a Controller built without config must still guard, matching the
    // resolver's default, so the two defaults cannot drift apart unnoticed.
    let fx = ctrl.handle(Intent::Close);
    assert!(!fx.quit);
    assert!(ctrl.discard_confirm_open());
}

// --- Worktree switch: the other path that discards annotations ---

#[test]
fn switch_confirm_y_copies_then_switches() {
    let first = TempDir::new();
    std::fs::write(first.path().join("a.rs"), "source\n").unwrap();
    let second = TempDir::new();
    std::fs::write(second.path().join("b.rs"), "source\n").unwrap();
    let (mut ctrl, clipboard) = controller(first.path(), false);
    add_file_annotation(&mut ctrl, "kept");

    ctrl.re_root(second.path());
    assert!(ctrl.discard_confirm_open());

    ctrl.handle_discard_confirm_key(key(KeyCode::Char('y')));
    assert!(!ctrl.discard_confirm_open(), "a successful copy proceeds");
    assert!(ctrl.annotations().is_empty(), "the switch happened");
    assert_eq!(ctrl.root(), second.path(), "and re-rooted to the target");
    let state = clipboard.lock().unwrap();
    assert_eq!(
        state.calls[0].as_bytes(),
        b"<file-annotations>\n- a.rs -> kept\n</file-annotations>",
        "y copies before discarding, same export as everywhere else"
    );
}

#[test]
fn switch_confirm_esc_cancels_the_switch_entirely() {
    let first = TempDir::new();
    std::fs::write(first.path().join("a.rs"), "source\n").unwrap();
    let second = TempDir::new();
    std::fs::write(second.path().join("b.rs"), "source\n").unwrap();
    let (mut ctrl, _clipboard) = controller(first.path(), false);
    add_file_annotation(&mut ctrl, "kept");

    ctrl.re_root(second.path());
    assert!(ctrl.discard_confirm_open());

    ctrl.handle_discard_confirm_key(key(KeyCode::Esc));
    assert!(!ctrl.discard_confirm_open());
    assert_eq!(ctrl.annotations().len(), 1, "the annotations survive");
    assert_eq!(
        ctrl.root(),
        first.path(),
        "esc cancels the switch too, not just the discard: the viewer stays put"
    );
}

#[test]
fn switch_confirm_holds_open_when_the_clipboard_write_fails() {
    let first = TempDir::new();
    std::fs::write(first.path().join("a.rs"), "source\n").unwrap();
    let second = TempDir::new();
    std::fs::write(second.path().join("b.rs"), "source\n").unwrap();
    let (mut ctrl, _clipboard) = controller(first.path(), true);
    add_file_annotation(&mut ctrl, "kept");

    ctrl.re_root(second.path());
    ctrl.handle_discard_confirm_key(key(KeyCode::Char('y')));
    assert!(ctrl.discard_confirm_open(), "a failed copy does not switch");
    assert_eq!(ctrl.root(), first.path(), "still on the old root");
    assert_eq!(ctrl.annotations().len(), 1);
}

#[test]
fn switch_confirm_is_skipped_when_nothing_would_be_lost() {
    let first = TempDir::new();
    std::fs::write(first.path().join("a.rs"), "source\n").unwrap();
    let second = TempDir::new();
    std::fs::write(second.path().join("b.rs"), "source\n").unwrap();

    // Empty store: a switch loses nothing, so it must not interrupt.
    let (mut ctrl, _) = controller(first.path(), false);
    ctrl.re_root(second.path());
    assert!(
        !ctrl.discard_confirm_open(),
        "nothing held, nothing to confirm"
    );
    assert_eq!(
        ctrl.root(),
        second.path(),
        "the switch went straight through"
    );

    // Opted out: the guard is off, so a switch discards immediately as it did before.
    let (mut opted_out, _) = controller(first.path(), false);
    opted_out.apply_confirm_discard(false);
    add_file_annotation(&mut opted_out, "kept");
    opted_out.re_root(second.path());
    assert!(!opted_out.discard_confirm_open());
    assert!(
        opted_out.annotations().is_empty(),
        "discarded, not confirmed"
    );
    assert_eq!(opted_out.root(), second.path());
}

#[test]
fn switch_confirm_proceed_key_is_enter_not_q() {
    let first = TempDir::new();
    std::fs::write(first.path().join("a.rs"), "source\n").unwrap();
    let second = TempDir::new();
    std::fs::write(second.path().join("b.rs"), "source\n").unwrap();
    let (mut ctrl, _clipboard) = controller(first.path(), false);
    add_file_annotation(&mut ctrl, "kept");
    ctrl.re_root(second.path());

    // `q` is the QUIT confirm's proceed key. On a switch it must be inert, and above all must not
    // quit the viewer: the user asked to change worktree, not to close.
    let fx = ctrl.handle_discard_confirm_key(key(KeyCode::Char('q')));
    assert!(!fx.quit, "q must never quit from the switch confirm");
    assert!(ctrl.discard_confirm_open(), "q is inert here");
    assert_eq!(ctrl.root(), first.path());

    let fx = ctrl.handle_discard_confirm_key(key(KeyCode::Enter));
    assert!(!fx.quit, "proceeding with a switch is not a quit");
    assert_eq!(ctrl.root(), second.path());
}
