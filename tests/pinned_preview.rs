//! Pinned-preview interaction-state seams that are useful before pin lifecycle wiring lands.

mod common;

use common::TempDir;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use herdr_file_viewer::controller::{
    Components, ContentProvider, Controller, EditorHandoff, EditorOutcome, GitService,
    RenderResult, RootProviders,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::infile::SearchState;
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::opener::{Opener, OpenerOutcome};
use herdr_file_viewer::presenter::{
    Focus, PaneGeometry, PreviewProjection, PreviewViewports, draw,
};
use herdr_file_viewer::preview::PreviewPresentation;
use herdr_file_viewer::search::Match;
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::{Terminal, backend::TestBackend};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Default)]
struct StubGit;

impl GitService for StubGit {
    fn status(&self) -> BTreeMap<std::path::PathBuf, Status> {
        BTreeMap::new()
    }

    fn changed_set(&self, _baseline: Baseline) -> BTreeMap<std::path::PathBuf, Status> {
        BTreeMap::new()
    }

    fn diff(&self, _path: &Path, _baseline: Baseline, _full_context: bool) -> String {
        String::new()
    }

    fn diff_directory(&self, _path: &Path, _baseline: Baseline) -> String {
        String::new()
    }
}

struct ChangedGit {
    changed: BTreeMap<PathBuf, Status>,
}

impl GitService for ChangedGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        self.changed.clone()
    }

    fn changed_set(&self, _baseline: Baseline) -> BTreeMap<PathBuf, Status> {
        self.changed.clone()
    }

    fn diff(&self, _path: &Path, _baseline: Baseline, _full_context: bool) -> String {
        String::new()
    }

    fn diff_directory(&self, _path: &Path, _baseline: Baseline) -> String {
        String::new()
    }
}

#[derive(Clone)]
struct Lines;

impl ContentProvider for Lines {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        RenderResult {
            content: Text::raw(
                (1..=20)
                    .map(|n| format!("line {n} needle"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            notices: Vec::new(),
            source: None,
        }
    }
}

struct CountingLines {
    calls: Arc<AtomicUsize>,
}

impl ContentProvider for CountingLines {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Lines.render(_path, _mode, _raw_diff)
    }
}

struct DiskContent;

impl ContentProvider for DiskContent {
    fn render(&self, path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        RenderResult {
            content: Text::raw(std::fs::read_to_string(path).unwrap()),
            notices: Vec::new(),
            source: None,
        }
    }
}

/// A rendered-Markdown fixture with presentation-specific styling and renderer notices.
/// Returning a distinct fallback for any other mode makes the test assert the policy selected
/// rendered Markdown before it freezes the display representation.
struct NoticedMarkdown;

impl ContentProvider for NoticedMarkdown {
    fn render(&self, _path: &Path, mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        match mode {
            ViewMode::RenderedMarkdown => RenderResult {
                content: Text::from(Line::from(vec![
                    Span::styled(
                        "rendered markdown",
                        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" preview needle one needle two"),
                ])),
                notices: vec![
                    "markdown renderer notice".into(),
                    "wrapped table truncated".into(),
                ],
                source: None,
            },
            _ => RenderResult {
                content: Text::raw("unexpected non-markdown presentation"),
                notices: vec!["wrong presentation".into()],
                source: None,
            },
        }
    }
}

struct NoopEditor;

impl EditorHandoff for NoopEditor {
    fn open(&mut self, _file: &Path) -> EditorOutcome {
        EditorOutcome::NoTakeover
    }
}

struct CountingEditor(Arc<AtomicUsize>);

impl EditorHandoff for CountingEditor {
    fn open(&mut self, _file: &Path) -> EditorOutcome {
        self.0.fetch_add(1, Ordering::SeqCst);
        EditorOutcome::NoTakeover
    }
}

struct CountingOpener(Arc<AtomicUsize>);

impl Opener for CountingOpener {
    fn open(&mut self, _path: &Path) -> OpenerOutcome {
        self.0.fetch_add(1, Ordering::SeqCst);
        OpenerOutcome::Launched
    }

    fn reveal(&mut self, _path: &Path) -> OpenerOutcome {
        self.0.fetch_add(1, Ordering::SeqCst);
        OpenerOutcome::Launched
    }
}

fn controller(root: &Path) -> Controller {
    let components = Components {
        providers: Box::new(|_resolved| RootProviders {
            git: Arc::new(StubGit),
            content: Box::new(Lines),
        }),
        editor: Box::new(NoopEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    Controller::new(
        common::resolved(root.to_path_buf(), root.join(".git").is_dir()),
        Baseline::Head,
        components,
    )
}

/// Build a controller whose clipboard log remains observable after construction.
fn controller_with_recording_clipboard(root: &Path) -> (Controller, Arc<Mutex<Vec<String>>>) {
    let clipboard = common::RecordingClipboard::default();
    let copied = Arc::clone(&clipboard.copied);
    let components = Components {
        providers: Box::new(|_resolved| RootProviders {
            git: Arc::new(StubGit),
            content: Box::new(Lines),
        }),
        editor: Box::new(NoopEditor),
        clipboard: Box::new(clipboard),
        renderers: None,
    };
    (
        Controller::new(
            common::resolved(root.to_path_buf(), false),
            Baseline::Head,
            components,
        ),
        copied,
    )
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn await_content(ctrl: &mut Controller) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while ctrl.content().lines.len() != 20 {
        ctrl.poll();
        assert!(Instant::now() < deadline, "preview content never rendered");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn content_text(ctrl: &Controller) -> String {
    ctrl.content()
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn await_content_containing(ctrl: &mut Controller, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !content_text(ctrl).contains(expected) {
        ctrl.poll();
        assert!(
            Instant::now() < deadline,
            "preview content never rendered {expected:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn await_active_relative_path(ctrl: &mut Controller, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while ctrl
        .active_document()
        .is_none_or(|document| document.origin().root_relative_path() != Path::new(expected))
    {
        ctrl.poll();
        assert!(
            Instant::now() < deadline,
            "active preview never rendered {expected:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Compare every pinned field that represents the frozen display and independent interaction.
/// `PreviewProjection` intentionally has no blanket equality because its active-only overlays do
/// not, so keep the pin oracle explicit rather than silently comparing a subset.
fn assert_pinned_projection_eq(actual: &PreviewProjection, expected: &PreviewProjection) {
    assert_eq!(actual.content, expected.content, "displayed styled lines");
    assert_eq!(actual.notices, expected.notices, "content-specific notices");
    assert_eq!(actual.title, expected.title, "captured title");
    assert_eq!(actual.rendering, expected.rendering, "rendering state");
    assert_eq!(actual.scroll, expected.scroll, "vertical scroll");
    assert_eq!(actual.hscroll, expected.hscroll, "horizontal scroll");
    assert_eq!(actual.rows, expected.rows, "display-row extent");
    assert_eq!(actual.wrap, expected.wrap, "wrapping presentation");
    assert_eq!(actual.pad_left, expected.pad_left, "presentation inset");
    assert_eq!(actual.origin, expected.origin, "captured origin");
    assert_eq!(
        actual.flash.is_none(),
        expected.flash.is_none(),
        "flash presence"
    );
    assert_eq!(
        actual.line_select.is_none(),
        expected.line_select.is_none(),
        "line selection presence"
    );
    assert_eq!(
        actual.selection.is_none(),
        expected.selection.is_none(),
        "character selection presence"
    );
    match (&actual.search, &expected.search) {
        (Some(actual), Some(expected)) => {
            assert_eq!(actual.matches, expected.matches, "search matches");
            assert_eq!(actual.current, expected.current, "current search match");
        }
        (None, None) => {}
        _ => panic!("search presence changed"),
    }
}

/// Assert no render was dispatched, deterministically.
///
/// The provider counter alone cannot prove this: `dispatch_render` runs the provider on a spawned
/// thread, so reading the counter immediately can race a real dispatch and pass, and sleeping to
/// wait for it makes the negative wall-clock-dependent (this file was previously bitten by exactly
/// that). `Controller::render_seq` is bumped SYNCHRONOUSLY inside `dispatch_render` before the
/// worker is spawned, so an unchanged seq is a race-free proof; the counter then corroborates it.
fn assert_no_render_dispatched(
    ctrl: &Controller,
    calls: &AtomicUsize,
    expected_calls: usize,
    seq_before: u64,
) {
    assert_eq!(
        ctrl.render_seq(),
        seq_before,
        "dispatch_render bumps the render seq synchronously: no render must have been dispatched"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        expected_calls,
        "pinning is synchronous state copying and must not dispatch content rendering"
    );
}

fn draw_viewports(ctrl: &Controller, width: u16, height: u16) -> PreviewViewports {
    let state = ctrl.view_state();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    let mut viewports = PreviewViewports::default();
    terminal
        .draw(|frame| viewports = draw(frame, &state))
        .expect("draw test frame");
    viewports
}

fn pin_ready_controller() -> (TempDir, Controller) {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("preview.rs"), "placeholder\n").unwrap();
    let mut ctrl = controller(dir.path());
    await_content(&mut ctrl);
    ctrl.set_preview_viewports(herdr_file_viewer::presenter::PreviewViewports {
        active: (8, 4),
        pinned: None,
    });
    ctrl.handle(Intent::PinPreview);
    ctrl.set_preview_viewports(herdr_file_viewer::presenter::PreviewViewports {
        active: (8, 4),
        pinned: Some((8, 4)),
    });
    (dir, ctrl)
}

#[test]
fn pin_focus_cycles_and_removal_returns_focus_to_active() {
    let (_dir, mut ctrl) = pin_ready_controller();

    assert_eq!(ctrl.focus(), Focus::Tree);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Tree);

    ctrl.handle(Intent::ToggleFocus);
    ctrl.handle(Intent::PinPreview);
    assert!(ctrl.view_state().pinned.is_none());
    assert_eq!(ctrl.focus(), Focus::Content);
}

#[test]
fn pinned_search_navigation_moves_only_the_pinned_search_and_viewport_with_wrap_notices() {
    let (_dir, mut ctrl) = pin_ready_controller();
    {
        let active = ctrl.active_interaction_mut();
        active.vertical_scroll = 6;
        active.search = Some(SearchState {
            query: "active search stays put".into(),
            matches: vec![Match {
                line: 12,
                start: 0,
                end: 6,
            }],
            current: 0,
        });
    }
    // Re-pin after seeding active state so the pin's own prompt has a separate starting point.
    ctrl.handle(Intent::PinPreview);
    ctrl.handle(Intent::PinPreview);
    ctrl.set_preview_viewports(PreviewViewports {
        active: (8, 4),
        pinned: Some((8, 4)),
    });
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
    let active_before = ctrl.active_interaction().clone();

    ctrl.handle(Intent::OpenSearch);
    for ch in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(ch)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));

    ctrl.handle(Intent::NextMatch);
    let pinned = ctrl.view_state().pinned.expect("pin remains present");
    assert_eq!(pinned.search.as_ref().map(|search| search.current), Some(1));
    assert_eq!(pinned.scroll, 1, "next match scrolls the pinned viewport");
    assert_eq!(ctrl.active_interaction(), &active_before);

    ctrl.handle(Intent::PrevMatch);
    let pinned = ctrl.view_state().pinned.expect("pin remains present");
    assert_eq!(pinned.search.as_ref().map(|search| search.current), Some(0));
    assert_eq!(
        pinned.scroll, 0,
        "previous match scrolls the pinned viewport back"
    );
    assert_eq!(ctrl.active_interaction(), &active_before);

    ctrl.handle(Intent::PrevMatch);
    let pinned = ctrl.view_state().pinned.expect("pin remains present");
    assert_eq!(
        pinned.search.as_ref().map(|search| search.current),
        Some(19)
    );
    assert_eq!(
        pinned.scroll, 16,
        "wrapped previous match reaches pinned bottom"
    );
    assert_eq!(ctrl.action_notice(), Some("Search: wrapped to last match"));
    assert_eq!(ctrl.active_interaction(), &active_before);

    ctrl.handle(Intent::NextMatch);
    let pinned = ctrl.view_state().pinned.expect("pin remains present");
    assert_eq!(pinned.search.as_ref().map(|search| search.current), Some(0));
    assert_eq!(pinned.scroll, 0, "wrapped next match returns to pinned top");
    assert_eq!(ctrl.action_notice(), Some("Search: wrapped to first match"));
    assert_eq!(ctrl.active_interaction(), &active_before);
}

#[test]
fn pinned_search_cancel_restores_its_saved_scroll_and_clears_the_search() {
    let (_dir, mut ctrl) = pin_ready_controller();
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
    for _ in 0..3 {
        ctrl.handle(Intent::NavDown);
    }
    let saved_scroll = ctrl
        .view_state()
        .pinned
        .expect("pin remains present")
        .scroll;
    assert_eq!(saved_scroll, 3, "precondition: pinned preview was scrolled");

    ctrl.handle(Intent::OpenSearch);
    for ch in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(ch)));
    }
    assert_eq!(
        ctrl.view_state()
            .pinned
            .expect("pin remains present")
            .scroll,
        0,
        "incremental search moved the pinned viewport before cancellation"
    );

    ctrl.handle_prompt_key(key(KeyCode::Esc));
    let pinned = ctrl.view_state().pinned.expect("pin remains present");
    assert_eq!(
        pinned.scroll, saved_scroll,
        "Esc restores pinned prompt scroll"
    );
    assert!(
        pinned.search.is_none(),
        "Esc clears pinned search highlights"
    );
}

#[test]
fn close_from_pinned_focus_dismisses_the_visible_active_search_before_quitting() {
    let (_dir, mut ctrl) = pin_ready_controller();
    ctrl.handle(Intent::OpenSearch);
    for ch in "place".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(ch)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert!(
        ctrl.view_state().active.search.is_some(),
        "precondition: a committed active search is highlighted"
    );
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);

    let fx = ctrl.handle(Intent::Close);
    assert!(
        !fx.quit,
        "the first close dismisses the still-visible active search instead of quitting"
    );
    assert!(
        ctrl.view_state().active.search.is_none(),
        "the visible active search was dismissed from pinned focus"
    );
    assert_eq!(ctrl.focus(), Focus::Pinned, "dismissal does not move focus");
    assert!(
        ctrl.view_state().pinned.is_some(),
        "dismissal does not unpin"
    );

    let fx = ctrl.handle(Intent::Close);
    assert!(fx.quit, "with no search left, close quits");
}

#[test]
fn close_from_pinned_focus_quits_past_a_hidden_active_search() {
    let (_dir, mut ctrl) = pin_ready_controller();
    ctrl.handle(Intent::OpenSearch);
    for ch in "place".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(ch)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
    // This fixture explicitly hides the active viewport, so close must not burn a keypress on its
    // off-screen search even though the pin remains visible.
    ctrl.set_preview_viewports(PreviewViewports {
        active: (0, 0),
        pinned: Some((8, 4)),
    });

    let fx = ctrl.handle(Intent::Close);
    assert!(fx.quit, "no visible search stands between close and quit");
    assert!(
        ctrl.active_interaction().search.is_some(),
        "the hidden active search was not consumed on the way out"
    );
}

#[test]
fn close_from_active_focus_dismisses_the_visible_pinned_search_before_quitting() {
    let (_dir, mut ctrl) = pin_ready_controller();
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
    ctrl.handle(Intent::OpenSearch);
    for ch in "place".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(ch)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Tree);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);

    let fx = ctrl.handle(Intent::Close);
    assert!(
        !fx.quit,
        "the first close dismisses the still-visible pinned search instead of quitting"
    );
    assert!(
        ctrl.view_state()
            .pinned
            .expect("pin remains present")
            .search
            .is_none(),
        "the visible pinned search was dismissed from active focus"
    );

    let fx = ctrl.handle(Intent::Close);
    assert!(fx.quit, "with no search left, close quits");
}

#[test]
fn pinned_scroll_search_and_paging_do_not_touch_active_interaction() {
    let (_dir, mut ctrl) = pin_ready_controller();
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);

    ctrl.handle(Intent::NavDown);
    assert_eq!(ctrl.active_interaction().vertical_scroll, 0);
    assert_eq!(ctrl.view_state().pinned.as_ref().unwrap().scroll, 1);
    ctrl.handle(Intent::PageDown);
    assert_eq!(ctrl.view_state().pinned.as_ref().unwrap().scroll, 5);
    ctrl.handle(Intent::Expand);
    assert_eq!(ctrl.active_interaction().horizontal_scroll, 0);
    assert!(ctrl.view_state().pinned.as_ref().unwrap().hscroll > 0);

    ctrl.handle(Intent::OpenSearch);
    for ch in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(ch)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert!(ctrl.active_interaction().search.is_none());
    assert_eq!(
        ctrl.view_state()
            .pinned
            .as_ref()
            .and_then(|p| p.search.as_ref())
            .map(|s| s.matches.len()),
        Some(20)
    );
}

#[test]
fn pinned_unavailable_actions_are_consumed_with_a_notice() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("preview.rs"), "placeholder\n").unwrap();
    let editor_calls = Arc::new(AtomicUsize::new(0));
    let opener_calls = Arc::new(AtomicUsize::new(0));
    let clipboard = common::RecordingClipboard::default();
    let copied = Arc::clone(&clipboard.copied);
    let editor_counter = Arc::clone(&editor_calls);
    let components = Components {
        providers: Box::new(|_resolved| RootProviders {
            git: Arc::new(StubGit),
            content: Box::new(Lines),
        }),
        editor: Box::new(CountingEditor(editor_counter)),
        clipboard: Box::new(clipboard),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );
    await_content(&mut ctrl);
    ctrl.set_preview_viewports(herdr_file_viewer::presenter::PreviewViewports {
        active: (8, 4),
        pinned: None,
    });
    ctrl.handle(Intent::PinPreview);
    ctrl.set_preview_viewports(herdr_file_viewer::presenter::PreviewViewports {
        active: (8, 4),
        pinned: Some((8, 4)),
    });
    ctrl.set_opener(Box::new(CountingOpener(Arc::clone(&opener_calls))));
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
    let unavailable = [
        Intent::Activate,
        Intent::OpenFullscreen,
        Intent::OpenGoToLine,
        Intent::TreeScrollRight,
        Intent::OpenInEditor,
        Intent::OpenWithApp,
        Intent::OpenRichPreview,
        Intent::RevealInFileManager,
        Intent::AddAnnotation,
        Intent::ShowAnnotations,
        Intent::CycleDiffRender,
        Intent::CycleView,
        Intent::ToggleWrap,
    ];
    for intent in unavailable {
        let before = ctrl.view_state();
        let fx = ctrl.handle(intent);
        assert!(fx.redraw, "{intent:?} reports rejection");
        assert_eq!(ctrl.focus(), Focus::Pinned, "{intent:?} keeps pinned focus");
        assert!(
            ctrl.action_notice().is_some(),
            "{intent:?} explains rejection"
        );
        assert_eq!(
            ctrl.view_state().pinned.as_ref().unwrap().scroll,
            before.pinned.as_ref().unwrap().scroll
        );
        assert_eq!(
            ctrl.active_interaction().vertical_scroll,
            before.active.scroll
        );
        assert_eq!(editor_calls.load(Ordering::SeqCst), 0, "{intent:?}");
        assert_eq!(opener_calls.load(Ordering::SeqCst), 0, "{intent:?}");
        assert!(copied.lock().unwrap().is_empty(), "{intent:?}");
    }
}

#[test]
fn pinned_incremental_search_borrows_its_document_and_preserves_matches() {
    let (_dir, mut ctrl) = pin_ready_controller();
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
    ctrl.handle(Intent::OpenSearch);

    for (typed, matches, first) in [
        (
            'n',
            40,
            Match {
                line: 0,
                start: 2,
                end: 3,
            },
        ),
        (
            'e',
            40,
            Match {
                line: 0,
                start: 2,
                end: 4,
            },
        ),
        (
            'e',
            20,
            Match {
                line: 0,
                start: 7,
                end: 10,
            },
        ),
        (
            'd',
            20,
            Match {
                line: 0,
                start: 7,
                end: 11,
            },
        ),
    ] {
        ctrl.handle_prompt_key(key(KeyCode::Char(typed)));
        let search = ctrl
            .view_state()
            .pinned
            .and_then(|pin| pin.search)
            .expect("each incremental query keeps pinned search state");
        assert_eq!(
            search.current, 0,
            "a changed query starts at its first match"
        );
        assert_eq!(search.matches.len(), matches);
        assert_eq!(search.matches[0], first);
    }

    ctrl.handle_prompt_key(key(KeyCode::Enter));
    ctrl.handle(Intent::NextMatch);
    let committed = ctrl
        .view_state()
        .pinned
        .and_then(|pin| pin.search)
        .expect("the committed pinned search remains available");
    assert_eq!(committed.current, 1);
    assert_eq!(committed.matches[1].line, 1);

    assert!(
        !include_str!("../src/controller/infile.rs").contains("document.content().lines.clone()"),
        "incremental pinned search must borrow the frozen document lines rather than clone them"
    );
}

#[test]
fn hiding_a_pin_keeps_no_pin_geometry_and_skips_pinned_focus() {
    let (_dir, mut ctrl) = pin_ready_controller();
    let no_pin = {
        ctrl.handle(Intent::PinPreview);
        let viewports = draw_viewports(&ctrl, 60, 12);
        ctrl.handle(Intent::PinPreview);
        viewports
    };
    let narrow = draw_viewports(&ctrl, 60, 12);
    assert_eq!(narrow.pinned, None, "the narrow layout hides the pin only");
    assert_eq!(
        narrow.active, no_pin.active,
        "AC-16: pinning never changes active geometry"
    );

    ctrl.set_preview_viewports(narrow);
    assert_eq!(ctrl.focus(), Focus::Tree);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Tree, "the hidden pin is never focused");
}

#[test]
fn pinned_y_copies_the_captured_root_relative_path_after_re_root() {
    let original_root = TempDir::new();
    let original_file = original_root.path().join("pinned.rs");
    std::fs::write(&original_file, "original\n").unwrap();
    let new_root = TempDir::new();
    std::fs::write(new_root.path().join("current.rs"), "current\n").unwrap();
    let (mut ctrl, copied) = controller_with_recording_clipboard(original_root.path());

    await_content(&mut ctrl);
    ctrl.handle(Intent::PinPreview);
    ctrl.re_root(new_root.path());
    await_content(&mut ctrl);
    ctrl.set_preview_viewports(PreviewViewports {
        active: (8, 4),
        pinned: Some((8, 4)),
    });
    std::fs::remove_dir_all(original_root.path()).unwrap();

    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
    ctrl.handle(Intent::CopyRepoPath);

    assert_eq!(
        copied.lock().unwrap().as_slice(),
        ["pinned.rs"],
        "pinned y uses its captured origin, not the current tree or filesystem"
    );
}

// Windows forbids control bytes in filesystem entry names. The controller's sanitizer is
// platform-independent, but this end-to-end proof needs a hostile path that can be pinned.
#[cfg(unix)]
#[test]
fn pinned_capital_y_copies_a_sanitized_captured_absolute_path_after_root_removal() {
    let original_root = TempDir::new();
    let hostile_name = "pin\u{1b}[2J\u{7}\n.rs";
    let original_file = original_root.path().join(hostile_name);
    std::fs::write(&original_file, "original\n").unwrap();
    let expected = original_file
        .to_string_lossy()
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>();
    let new_root = TempDir::new();
    std::fs::write(new_root.path().join("current.rs"), "current\n").unwrap();
    let (mut ctrl, copied) = controller_with_recording_clipboard(original_root.path());

    await_content(&mut ctrl);
    ctrl.handle(Intent::PinPreview);
    ctrl.re_root(new_root.path());
    await_content(&mut ctrl);
    ctrl.set_preview_viewports(PreviewViewports {
        active: (8, 4),
        pinned: Some((8, 4)),
    });
    std::fs::remove_dir_all(original_root.path()).unwrap();

    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
    ctrl.handle(Intent::CopyAbsPath);

    let copied = copied.lock().unwrap();
    assert_eq!(
        copied.as_slice(),
        [expected],
        "pinned Y uses the captured absolute origin without reading its removed root"
    );
    assert!(
        copied[0].chars().all(|ch| !ch.is_control()),
        "clipboard text never carries hostile terminal or paste control characters"
    );
}

#[test]
fn active_zoom_keeps_pin_while_pinned_fullscreen_is_rejected() {
    let (_dir, mut ctrl) = pin_ready_controller();
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
    ctrl.handle(Intent::OpenFullscreen);
    assert!(!ctrl.zoomed());
    assert!(ctrl.view_state().pinned.is_some());
    assert!(ctrl.action_notice().is_some());

    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Tree);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::OpenFullscreen);
    assert!(ctrl.zoomed());
    assert!(ctrl.view_state().pinned.is_some());
}

#[test]
fn tree_hidden_focus_cycles_between_pinned_and_active() {
    let (_dir, mut ctrl) = pin_ready_controller();
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
    ctrl.handle(Intent::ToggleZoom);
    assert!(ctrl.zoomed());
    assert_eq!(ctrl.focus(), Focus::Pinned);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
}

#[test]
fn active_interaction_can_be_copied_and_mutated_independently() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("preview.rs"), "placeholder\n").unwrap();
    let mut ctrl = controller(dir.path());

    let interaction = ctrl.active_interaction_mut();
    interaction.vertical_scroll = 7;
    interaction.horizontal_scroll = 3;
    interaction.search = Some(SearchState {
        query: "needle".into(),
        matches: vec![
            Match {
                line: 2,
                start: 7,
                end: 13,
            },
            Match {
                line: 4,
                start: 7,
                end: 13,
            },
        ],
        current: 1,
    });

    let mut copied = ctrl.active_interaction().clone();
    copied.vertical_scroll = 11;
    copied.horizontal_scroll = 9;
    copied.search.as_mut().unwrap().query = "changed".into();

    assert_eq!(ctrl.active_interaction().vertical_scroll, 7);
    assert_eq!(ctrl.active_interaction().horizontal_scroll, 3);
    assert_eq!(
        ctrl.active_interaction().search.as_ref().unwrap().query,
        "needle"
    );
    assert_eq!(
        ctrl.active_interaction().search.as_ref().unwrap().current,
        1
    );
    assert_ne!(copied, *ctrl.active_interaction());
}

#[test]
fn unpinned_navigation_projects_the_existing_active_interaction() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("preview.rs"), "placeholder\n").unwrap();
    let mut ctrl = controller(dir.path());
    await_content(&mut ctrl);
    ctrl.set_content_viewport(40, 5);
    ctrl.set_pane_geometry(PaneGeometry {
        content_inner: Some(Rect::new(0, 0, 40, 5)),
        ..Default::default()
    });

    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::NavDown);
    ctrl.handle(Intent::OpenSearch);
    for ch in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(ch)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    ctrl.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 1,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    ctrl.handle_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: 6,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    ctrl.handle_mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 6,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(ctrl.active_interaction().vertical_scroll, 0);
    assert_eq!(ctrl.view_state().active.scroll, 0);
    assert_eq!(ctrl.active_interaction().horizontal_scroll, 0);
    assert_eq!(
        ctrl.search().map(|search| search.query.as_str()),
        Some("needle")
    );
    assert_eq!(
        ctrl.view_state()
            .active
            .search
            .as_ref()
            .map(|search| search.matches.len()),
        Some(20)
    );
    assert!(ctrl.active_interaction().selection.is_some());
    assert!(ctrl.view_state().active.selection.is_some());
}

#[test]
fn layout_only_wrap_toggle_keeps_the_active_document_presentation_in_sync() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("preview.rs"), "placeholder\n").unwrap();
    let mut ctrl = controller(dir.path());
    await_content(&mut ctrl);

    ctrl.handle(Intent::ToggleWrap);

    let projected = ctrl.view_state();
    let settled = *ctrl
        .active_document()
        .expect("the syntax preview remains a settled active document")
        .presentation();
    assert_eq!(
        settled,
        PreviewPresentation::new(
            settled.view_mode(),
            projected.active.wrap,
            projected.active.pad_left,
        )
    );
}

#[test]
fn no_pin_navigation_does_not_add_content_provider_work() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("preview.rs"), "placeholder\n").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider_calls = Arc::clone(&calls);
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(StubGit),
            content: Box::new(CountingLines {
                calls: Arc::clone(&provider_calls),
            }),
        }),
        editor: Box::new(NoopEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );
    await_content(&mut ctrl);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one initial file render");

    ctrl.set_content_viewport(40, 5);
    ctrl.set_pane_geometry(PaneGeometry {
        content_inner: Some(Rect::new(0, 0, 40, 5)),
        ..Default::default()
    });
    let seq_before = ctrl.render_seq();
    ctrl.handle(Intent::ToggleFocus);
    ctrl.handle(Intent::NavDown);
    ctrl.handle(Intent::OpenSearch);
    for ch in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(ch)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    ctrl.handle(Intent::NextMatch);
    ctrl.handle(Intent::PrevMatch);

    assert_no_render_dispatched(&ctrl, &calls, 1, seq_before);
}

#[test]
fn loading_pin_attempt_is_rejected_with_the_rendering_notice_and_no_snapshot() {
    let loading_root = TempDir::new();
    std::fs::write(loading_root.path().join("preview.rs"), "placeholder\n").unwrap();
    let mut loading = controller(loading_root.path());

    assert!(
        loading.active_document().is_none(),
        "loading is not pinnable"
    );
    assert!(loading.pin_active_preview().redraw);
    assert_eq!(
        loading.action_notice(),
        Some("Cannot pin while preview is rendering")
    );
    assert!(loading.view_state().pinned.is_none());
}

#[test]
fn loading_directory_and_empty_tree_are_not_active_documents() {
    let loading_root = TempDir::new();
    std::fs::write(loading_root.path().join("preview.rs"), "placeholder\n").unwrap();
    let loading = controller(loading_root.path());
    assert!(
        loading.active_document().is_none(),
        "loading is not pinnable"
    );

    let directory_root = TempDir::new();
    std::fs::create_dir(directory_root.path().join("child")).unwrap();
    let directory = controller(directory_root.path());
    assert!(
        directory.active_document().is_none(),
        "directory guidance is not pinnable"
    );

    let empty_root = TempDir::new();
    let empty = controller(empty_root.path());
    assert!(
        empty.active_document().is_none(),
        "empty-tree guidance is not pinnable"
    );
}

#[test]
fn pin_lifecycle_clones_the_settled_preview_and_toggles_the_same_identity() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("preview.md"), "placeholder\n").unwrap();
    let components = Components {
        providers: Box::new(|_resolved| RootProviders {
            git: Arc::new(StubGit),
            content: Box::new(NoticedMarkdown),
        }),
        editor: Box::new(NoopEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );
    await_content_containing(&mut ctrl, "rendered markdown preview");
    assert_eq!(
        ctrl.active_document()
            .expect("settled Markdown document")
            .presentation()
            .view_mode(),
        ViewMode::RenderedMarkdown,
        "the pin captures a rendered-Markdown presentation, not just syntax content"
    );
    let active_content = ctrl.content().clone();
    let active_notices = ctrl.view_state().active.notices;
    let interaction = ctrl.active_interaction_mut();
    interaction.vertical_scroll = 7;
    interaction.horizontal_scroll = 3;
    interaction.search = Some(SearchState {
        query: "needle".into(),
        matches: vec![
            Match {
                line: 0,
                start: 26,
                end: 32,
            },
            Match {
                line: 0,
                start: 37,
                end: 43,
            },
        ],
        current: 1,
    });

    assert!(ctrl.pin_active_preview().redraw);
    let pin = ctrl.view_state().pinned.expect("settled file is pinned");
    assert_eq!(
        *pin.content, active_content,
        "styled displayed lines are frozen"
    );
    assert_eq!(
        pin.notices, active_notices,
        "content-specific renderer notices are frozen with the presentation"
    );
    assert_eq!(pin.scroll, 7);
    assert_eq!(pin.hscroll, 3);
    assert_eq!(
        pin.search.as_ref().map(|search| search.matches.len()),
        Some(2)
    );
    assert_eq!(
        pin.search.as_ref().map(|search| search.current),
        Some(1),
        "the captured search keeps its non-zero current match"
    );
    assert_eq!(ctrl.view_state().preview_split_pct, 50);
    ctrl.set_preview_viewports(PreviewViewports {
        active: (8, 4),
        pinned: Some((8, 4)),
    });
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
    assert!(
        ctrl.view_state()
            .prompt
            .as_deref()
            .is_some_and(|status| status.contains("Search: needle (2/2)")),
        "the captured search keeps its query as well as its current match"
    );

    assert!(ctrl.pin_active_preview().redraw);
    assert!(
        ctrl.view_state().pinned.is_none(),
        "same identity removes the pin"
    );

    assert!(ctrl.pin_active_preview().redraw);
    assert!(ctrl.view_state().pinned.is_some());
    assert_eq!(ctrl.view_state().preview_split_pct, 50);
}

#[test]
fn pin_preview_intent_invokes_the_existing_snapshot_lifecycle() {
    // T-13 only wires the registry/dispatcher to T-6's already-tested lifecycle; it must not
    // create a second pin implementation.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("preview.rs"), "placeholder\n").unwrap();
    let mut ctrl = controller(dir.path());
    await_content(&mut ctrl);

    assert!(ctrl.handle(Intent::PinPreview).redraw);
    assert!(
        ctrl.view_state().pinned.is_some(),
        "dispatch must create T-6's snapshot"
    );
}

#[test]
fn preview_resize_intents_move_only_the_pinned_share_and_preserve_it_across_repinning() {
    let (_dir, mut ctrl) = pin_ready_controller();
    let tree_split = ctrl.split_pct();

    ctrl.handle(Intent::ShrinkPreview);
    assert_eq!(ctrl.view_state().preview_split_pct, 45);
    assert_eq!(
        ctrl.split_pct(),
        tree_split,
        "preview resize must not move the tree divider"
    );

    for _ in 0..10 {
        ctrl.handle(Intent::ShrinkPreview);
    }
    assert_eq!(
        ctrl.view_state().preview_split_pct,
        20,
        "shrink clamps at 20%"
    );

    for _ in 0..20 {
        ctrl.handle(Intent::GrowPreview);
    }
    assert_eq!(
        ctrl.view_state().preview_split_pct,
        80,
        "grow clamps at 80%"
    );
    assert_eq!(
        ctrl.split_pct(),
        tree_split,
        "preview resize must not move the tree divider"
    );

    ctrl.handle(Intent::PinPreview);
    assert!(ctrl.view_state().pinned.is_none());
    ctrl.handle(Intent::PinPreview);
    assert!(ctrl.view_state().pinned.is_some());
    assert_eq!(
        ctrl.view_state().preview_split_pct,
        80,
        "repinning preserves the ratio"
    );
    assert_eq!(
        ctrl.split_pct(),
        tree_split,
        "repinning must not move the tree divider"
    );
}

#[test]
fn preview_resize_intents_are_inert_when_the_pin_is_not_drawn() {
    let (_dir, mut ctrl) = pin_ready_controller();
    ctrl.set_preview_viewports(PreviewViewports {
        active: (8, 4),
        pinned: None,
    });
    let ratio = ctrl.view_state().preview_split_pct;
    assert!(!ctrl.handle(Intent::ShrinkPreview).redraw);
    assert!(!ctrl.handle(Intent::GrowPreview).redraw);
    assert_eq!(ctrl.view_state().preview_split_pct, ratio);
}

#[test]
fn preview_resize_intents_are_inert_without_a_pin() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("preview.rs"), "placeholder\n").unwrap();
    let mut ctrl = controller(dir.path());
    let ratio = ctrl.view_state().preview_split_pct;
    let tree_split = ctrl.split_pct();

    assert!(!ctrl.handle(Intent::ShrinkPreview).redraw);
    assert!(!ctrl.handle(Intent::GrowPreview).redraw);
    assert_eq!(ctrl.view_state().preview_split_pct, ratio);
    assert_eq!(ctrl.split_pct(), tree_split);
}

#[test]
fn mouse_routes_pinned_scroll_and_preview_divider_drag_without_touching_active() {
    let (_dir, mut ctrl) = pin_ready_controller();
    ctrl.set_pane_geometry(PaneGeometry {
        // This is the measured wide split layout: the tree ends before the preview area, and
        // the pinned/active divider is inside that area. Mouse routing must use these live
        // measurements rather than infer either boundary from the configured percentages.
        preview_area_x: 40,
        preview_area_width: 50,
        preview_divider_x: Some(65),
        content_inner: Some(Rect::new(40, 1, 8, 4)),
        pinned_inner: Some(Rect::new(65, 1, 8, 4)),
        ..PaneGeometry::default()
    });

    let active_before = ctrl.active_interaction().clone();
    let pinned_before = ctrl.view_state().pinned.expect("pin is present");
    let tree_split_before = ctrl.split_pct();

    ctrl.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 67,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    ctrl.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollRight,
        column: 67,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });

    let pinned_after_scroll = ctrl.view_state().pinned.expect("pin remains present");
    assert!(pinned_after_scroll.scroll > pinned_before.scroll);
    assert!(pinned_after_scroll.hscroll > pinned_before.hscroll);
    assert_eq!(ctrl.active_interaction(), &active_before);

    ctrl.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 65,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    ctrl.handle_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: 41,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    ctrl.handle_mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 41,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(
        ctrl.view_state().preview_split_pct,
        80,
        "dragging toward the active edge grows the pinned share"
    );
    ctrl.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 65,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    ctrl.handle_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: 89,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    ctrl.handle_mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 89,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(ctrl.view_state().preview_split_pct, 20);
    assert_eq!(ctrl.split_pct(), tree_split_before);
    assert_eq!(ctrl.active_interaction(), &active_before);
}

#[test]
fn preview_divider_drag_maps_the_cursor_column_to_a_proportional_pinned_share() {
    let (_dir, mut ctrl) = pin_ready_controller();
    // Measured geometry: active spans columns 40..65, the pin spans 65..90, divider at 65.
    ctrl.set_pane_geometry(PaneGeometry {
        preview_area_x: 40,
        preview_area_width: 50,
        preview_divider_x: Some(65),
        content_inner: Some(Rect::new(40, 1, 8, 4)),
        pinned_inner: Some(Rect::new(65, 1, 8, 4)),
        ..PaneGeometry::default()
    });

    // The pin is on the right, so its proportional share is measured from the right edge.
    // Interior columns must not merely snap toward the drag direction.
    for (column, expected_pct) in [(51, 78), (55, 70), (65, 50), (72, 36), (79, 22)] {
        ctrl.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 65,
            row: 2,
            modifiers: KeyModifiers::NONE,
        });
        ctrl.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column,
            row: 2,
            modifiers: KeyModifiers::NONE,
        });
        ctrl.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row: 2,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(
            ctrl.view_state().preview_split_pct,
            expected_pct,
            "dragging the divider to column {column} maps to a {expected_pct}% pinned share"
        );
    }
}

#[test]
fn pinned_scrollbar_press_and_drag_preserve_the_active_selection() {
    let (_dir, mut ctrl) = pin_ready_controller();
    ctrl.set_pane_geometry(PaneGeometry {
        content_inner: Some(Rect::new(60, 1, 8, 4)),
        ..PaneGeometry::default()
    });

    // Establish a standing active selection first. A pinned snapshot has no selection/copy path,
    // so input directed at its scrollbars must not dismiss this active-pane selection.
    ctrl.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 61,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    ctrl.handle_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: 65,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    ctrl.handle_mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 65,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    let selection_before = ctrl
        .active_interaction()
        .selection
        .expect("the active drag establishes a standing selection");

    ctrl.set_pane_geometry(PaneGeometry {
        pinned_vbar: Some(Rect::new(40, 1, 1, 4)),
        pinned_hbar: Some(Rect::new(41, 5, 8, 1)),
        ..PaneGeometry::default()
    });

    for event in [
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 40,
            row: 4,
            modifiers: KeyModifiers::NONE,
        },
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 40,
            row: 1,
            modifiers: KeyModifiers::NONE,
        },
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 40,
            row: 1,
            modifiers: KeyModifiers::NONE,
        },
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 48,
            row: 5,
            modifiers: KeyModifiers::NONE,
        },
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 41,
            row: 5,
            modifiers: KeyModifiers::NONE,
        },
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 41,
            row: 5,
            modifiers: KeyModifiers::NONE,
        },
    ] {
        ctrl.handle_mouse(event);
    }

    assert_eq!(
        ctrl.active_interaction().selection.as_ref(),
        Some(&selection_before)
    );
}

#[test]
fn pinning_a_different_file_replaces_one_frozen_snapshot_without_rendering() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "a\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "b\n").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider_calls = Arc::clone(&calls);
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(StubGit),
            content: Box::new(CountingLines {
                calls: Arc::clone(&provider_calls),
            }),
        }),
        editor: Box::new(NoopEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );
    await_content(&mut ctrl);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    ctrl.pin_active_preview();
    let first = ctrl
        .view_state()
        .pinned
        .expect("first file was captured")
        .origin
        .expect("pins retain origins");

    ctrl.handle(Intent::NavDown);
    await_content(&mut ctrl);
    let frozen = ctrl.view_state().pinned.expect("navigation keeps the pin");
    assert_eq!(frozen.origin.as_ref(), Some(&first));

    // Scroll the active preview off the top first: `dispatch_render` zeroes the active scroll
    // synchronously, so a preserved offset is the race-free tell that replacing a pin dispatched
    // no render (see `assert_no_render_dispatched`).
    let calls_before_pin = calls.load(Ordering::SeqCst);
    let seq_before_pin = ctrl.render_seq();
    ctrl.pin_active_preview();
    assert_no_render_dispatched(&ctrl, &calls, calls_before_pin, seq_before_pin);
    let replacement = ctrl.view_state().pinned.expect("different file replaces");
    assert_ne!(replacement.origin.as_ref(), Some(&first));
    assert_eq!(replacement.content.lines.len(), 20);
}

#[test]
fn rejected_pin_attempts_leave_no_file_and_directory_targets_explained() {
    let empty_root = TempDir::new();
    let mut empty = controller(empty_root.path());
    assert!(empty.pin_active_preview().redraw);
    assert!(empty.view_state().pinned.is_none());
    assert_eq!(
        empty.action_notice(),
        Some("Cannot pin: no file is selected")
    );

    let directory_root = TempDir::new();
    std::fs::create_dir(directory_root.path().join("child")).unwrap();
    let mut directory = controller(directory_root.path());
    assert!(directory.pin_active_preview().redraw);
    assert!(directory.view_state().pinned.is_none());
    assert_eq!(directory.action_notice(), Some("Cannot pin a directory"));

    let existing_root = TempDir::new();
    std::fs::create_dir(existing_root.path().join("folder")).unwrap();
    std::fs::write(existing_root.path().join("preview.rs"), "placeholder\n").unwrap();
    let mut existing = controller(existing_root.path());
    // The directory sorts first. Pin the file, then return to the ineligible directory.
    existing.handle(Intent::NavDown);
    await_content(&mut existing);
    {
        let interaction = existing.active_interaction_mut();
        interaction.vertical_scroll = 7;
        interaction.horizontal_scroll = 3;
        interaction.search = Some(SearchState {
            query: "needle".into(),
            matches: vec![Match {
                line: 4,
                start: 7,
                end: 13,
            }],
            current: 0,
        });
    }
    existing.pin_active_preview();
    let before = existing.view_state().pinned.expect("file pin precondition");
    existing.handle(Intent::NavUp);
    assert!(
        existing.active_document().is_none(),
        "directory is ineligible"
    );

    existing.pin_active_preview();
    assert_eq!(existing.action_notice(), Some("Cannot pin a directory"));
    let after = existing
        .view_state()
        .pinned
        .expect("directory rejection keeps the existing pin");
    assert_pinned_projection_eq(&after, &before);

    // Empty the tree after the file was frozen. The next rejected pin is AC-4's no-selection
    // path, which must retain the already-captured document rather than treating no selection as
    // a request to clear it.
    std::fs::remove_dir(existing_root.path().join("folder")).unwrap();
    std::fs::remove_file(existing_root.path().join("preview.rs")).unwrap();
    existing.handle(Intent::Refresh);
    assert!(existing.tree().selected().is_none(), "no file is selected");
    assert!(
        existing.active_document().is_none(),
        "empty tree is ineligible"
    );

    existing.pin_active_preview();
    assert_eq!(
        existing.action_notice(),
        Some("Cannot pin: no file is selected")
    );
    let after_no_selection = existing
        .view_state()
        .pinned
        .expect("no-selection rejection keeps the existing pin");
    assert_pinned_projection_eq(&after_no_selection, &before);
}

#[test]
fn changed_file_jumps_from_pinned_focus_retarget_only_the_active_preview() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.rs"), "a\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "b\n").unwrap();
    let changed = BTreeMap::from([
        (PathBuf::from("a.rs"), Status::Modified),
        (PathBuf::from("b.rs"), Status::Modified),
    ]);
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(ChangedGit {
                changed: changed.clone(),
            }),
            content: Box::new(Lines),
        }),
        editor: Box::new(NoopEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), true),
        Baseline::Head,
        components,
    );
    await_active_relative_path(&mut ctrl, "a.rs");
    {
        let active = ctrl.active_interaction_mut();
        active.vertical_scroll = 6;
        active.horizontal_scroll = 3;
        active.search = Some(SearchState {
            query: "needle".into(),
            matches: vec![Match {
                line: 4,
                start: 7,
                end: 13,
            }],
            current: 0,
        });
    }
    ctrl.handle(Intent::PinPreview);
    ctrl.set_preview_viewports(PreviewViewports {
        active: (8, 4),
        pinned: Some((8, 4)),
    });
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
    let pinned_before = ctrl.view_state().pinned.expect("pin precondition");
    let pinned_search_status = ctrl
        .view_state()
        .prompt
        .expect("pinned search status exposes the captured query");
    let active_before = ctrl
        .active_document()
        .expect("active document precondition")
        .origin()
        .identity();

    ctrl.handle(Intent::NextChanged);
    await_active_relative_path(&mut ctrl, "b.rs");
    let active_after_next = ctrl
        .active_document()
        .expect("next changed document")
        .origin()
        .identity();
    assert_ne!(
        active_after_next, active_before,
        "next changed retargets active"
    );
    assert_eq!(ctrl.focus(), Focus::Pinned, "pinned focus stays put");
    let pinned_after_next = ctrl.view_state().pinned.expect("pin remains present");
    assert_pinned_projection_eq(&pinned_after_next, &pinned_before);
    assert_eq!(ctrl.view_state().prompt, Some(pinned_search_status.clone()));

    ctrl.handle(Intent::PrevChanged);
    await_active_relative_path(&mut ctrl, "a.rs");
    assert_eq!(
        ctrl.active_document()
            .expect("previous changed document")
            .origin()
            .identity(),
        active_before,
        "previous changed retargets only the active preview back"
    );
    assert_eq!(ctrl.focus(), Focus::Pinned, "pinned focus stays put");
    let pinned_after_prev = ctrl.view_state().pinned.expect("pin remains present");
    assert_pinned_projection_eq(&pinned_after_prev, &pinned_before);
    assert_eq!(ctrl.view_state().prompt, Some(pinned_search_status));
}

#[test]
fn branch_changed_pin_replacement_names_named_and_detached_states() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("preview.rs"), "placeholder\n").unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .status()
            .expect("run git init")
            .success(),
        "git fixture initializes"
    );
    assert!(
        Command::new("git")
            .args(["add", "preview.rs"])
            .current_dir(dir.path())
            .status()
            .expect("stage fixture file")
            .success()
    );
    assert!(
        Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                "initial",
            ])
            .current_dir(dir.path())
            .status()
            .expect("commit fixture file")
            .success()
    );
    let mut ctrl = controller(dir.path());
    await_content(&mut ctrl);
    ctrl.pin_active_preview();

    assert!(
        Command::new("git")
            .args(["switch", "-c", "feature"])
            .current_dir(dir.path())
            .status()
            .expect("switch fixture branch")
            .success(),
        "git fixture switches branch"
    );
    ctrl.handle(Intent::Refresh);
    await_content(&mut ctrl);
    ctrl.pin_active_preview();
    assert_eq!(
        ctrl.action_notice(),
        Some("Replaced pin (branch changed: main → feature)"),
        "AC-42 names both branch states when same-root/path replacement is caused by a branch change"
    );
}

#[test]
fn finder_confirm_from_pinned_focus_moves_to_active_while_changed_jumps_do_not() {
    let (_dir, mut ctrl) = pin_ready_controller();
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);

    ctrl.handle(Intent::OpenFinder);
    assert!(ctrl.finder_open(), "finder opens from pinned focus");
    ctrl.handle_finder_key(key(KeyCode::Char('p')));
    ctrl.handle_finder_key(key(KeyCode::Enter));
    assert_eq!(
        ctrl.focus(),
        Focus::Content,
        "AC-41 finder confirmation takes focus to its file"
    );
}

/// AC-41 moves focus from the PINNED pane only. A confirm from the tree keeps tree focus, which is
/// the pre-feature behaviour: widening the rule to every confirm would silently take `j`/`k` away
/// from the tree cursor, which no criterion asks for.
#[test]
fn finder_confirm_from_tree_focus_leaves_focus_on_the_tree() {
    let (_dir, mut ctrl) = pin_ready_controller();
    assert_eq!(ctrl.focus(), Focus::Tree, "precondition: tree focus");

    ctrl.handle(Intent::OpenFinder);
    assert!(ctrl.finder_open(), "finder opens from tree focus");
    ctrl.handle_finder_key(key(KeyCode::Char('p')));
    ctrl.handle_finder_key(key(KeyCode::Enter));
    assert_eq!(
        ctrl.focus(),
        Focus::Tree,
        "a confirm from the tree must not steal focus to the preview"
    );
}

/// AC-12: a pin captured in the CURRENT worktree names no worktree, so the title stays short. The
/// comparison is over full root paths, not basenames, because two checkouts of one repo commonly
/// share a basename and a basename comparison would silently call a foreign pin local.
#[test]
fn a_pin_from_the_viewed_worktree_names_no_worktree() {
    let (_dir, ctrl) = pin_ready_controller();
    assert!(
        ctrl.view_state().pinned.is_some(),
        "precondition: a pin exists"
    );
    assert_eq!(
        ctrl.view_state().pinned_foreign_root,
        None,
        "a same-worktree pin must not repeat the viewed worktree's name"
    );
}

#[test]
fn active_scroll_and_search_leave_the_pinned_interaction_unchanged() {
    let (_dir, mut ctrl) = pin_ready_controller();
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
    ctrl.handle(Intent::NavDown);
    ctrl.handle(Intent::Expand);
    ctrl.handle(Intent::OpenSearch);
    for ch in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(ch)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    let pinned_before = ctrl
        .view_state()
        .pinned
        .expect("pinned interaction precondition");

    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Tree);
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Content);
    ctrl.handle(Intent::NavDown);
    ctrl.handle(Intent::Expand);
    ctrl.handle(Intent::OpenSearch);
    for ch in "needle".chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(ch)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    assert!(
        ctrl.active_interaction().search.is_some(),
        "precondition: active search committed"
    );

    let pinned_after = ctrl.view_state().pinned.expect("pin remains present");
    assert_eq!(pinned_after.scroll, pinned_before.scroll);
    assert_eq!(pinned_after.hscroll, pinned_before.hscroll);
    match (&pinned_after.search, &pinned_before.search) {
        (Some(actual), Some(expected)) => {
            assert_eq!(actual.matches, expected.matches);
            assert_eq!(actual.current, expected.current);
        }
        _ => panic!("active interaction changed pinned search presence"),
    }
    ctrl.handle(Intent::ToggleFocus);
    assert_eq!(ctrl.focus(), Focus::Pinned);
    assert!(
        ctrl.view_state()
            .prompt
            .as_deref()
            .is_some_and(|status| status.contains("Search: needle (1/20)")),
        "active search changes cannot replace the pinned query"
    );
}

#[test]
fn active_refresh_and_width_reflow_do_not_change_a_pinned_snapshot() {
    let dir = TempDir::new();
    let path = dir.path().join("preview.md");
    std::fs::write(&path, "before reflow\n").unwrap();
    let components = Components {
        providers: Box::new(|_resolved| RootProviders {
            git: Arc::new(StubGit),
            content: Box::new(DiskContent),
        }),
        editor: Box::new(NoopEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        components,
    );
    await_content_containing(&mut ctrl, "before reflow");
    ctrl.pin_active_preview();
    let before = ctrl.view_state().pinned.expect("pin before active changes");

    std::fs::write(&path, "after reflow\n").unwrap();
    ctrl.set_content_viewport(40, 5);
    await_content_containing(&mut ctrl, "after reflow");
    std::fs::write(&path, "after refresh\n").unwrap();
    ctrl.handle(Intent::Refresh);
    await_content_containing(&mut ctrl, "after refresh");
    let after = ctrl
        .view_state()
        .pinned
        .expect("pin survives active refresh");
    assert_eq!(after.content, before.content);
    assert_eq!(after.notices, before.notices);
    assert_eq!(after.origin, before.origin);
    assert_eq!(after.scroll, before.scroll);
    assert_eq!(after.hscroll, before.hscroll);
}
