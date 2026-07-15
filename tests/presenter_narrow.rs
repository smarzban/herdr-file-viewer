//! Presenter: narrow-split focus-toggle (< 80 cols), AC-21.
//! Under 80 columns the focused column takes the full width and the other is hidden;
//! at ≥ 80 columns both columns are shown.

use herdr_file_viewer::annotation::LineRange;
use herdr_file_viewer::git::Status;
use herdr_file_viewer::presenter::{
    AnnotationEditorKind, AnnotationEditorView, AnnotationIndicatorsView, AnnotationOverviewView,
    AnnotationRowView, AnnotationTargetView, Focus, ViewState, draw,
};
use herdr_file_viewer::render::to_text;
use herdr_file_viewer::tree::{Node, NodeKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::PathBuf;

fn node(path: &str, kind: NodeKind, depth: usize, status: Option<Status>) -> Node {
    Node {
        path: PathBuf::from(path),
        kind,
        depth,
        expanded: true,
        status,
        dir_dirty: false,
    }
}

fn state(width: u16, focus: Focus) -> ViewState {
    ViewState {
        nodes: vec![
            node("/r/src", NodeKind::Dir, 0, None),
            node("/r/src/main.rs", NodeKind::File, 1, Some(Status::Modified)),
            node("/r/scratch.log", NodeKind::File, 0, Some(Status::Untracked)),
        ],
        selected: 1,
        content: to_text("fn main() {}\n"),
        notices: vec!["delta not found — showing plain diff".to_string()],
        focus,
        width,
        content_scroll: 0,
        content_hscroll: 0,
        tree_scroll: 0,
        tree_hscroll: 0,
        content_rows: 1, // the fixture content is one line
        wrap: false,
        content_pad_left: false,
        split_pct: 40,
        tree_position: herdr_file_viewer::config::TreePosition::Left,
        tree_max_cols: 1000, // high cap: percentage governs (narrow-layout tests ignore it anyway)
        split_manual: false,
        zoomed: false,
        update_banner: None,
        picker: None,
        finder: None,
        annotation_count: 0,
        annotation_overview: None,
        annotation_editor: None,
        discard_confirm: None,
        annotation_indicators: AnnotationIndicatorsView::default(),
        root_name: "r".to_string(), // the fixture tree is rooted at /r
        branch: None,
        prompt: None,
        content_title: Some("main.rs".to_string()),
        content_rendering: false,
        search: None,
        line_select: None,
        content_selection: None,
        help: None,
    }
}

fn render(state: &ViewState, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|f| {
            draw(f, state);
        })
        .unwrap();
    format!("{}", terminal.backend())
}

#[test]
fn narrow_tree_focus_gives_tree_full_width_and_hides_content() {
    let out = render(&state(60, Focus::Tree), 60, 20);
    assert!(out.contains("scratch.log"), "tree shown full-width\n{out}");
    assert!(
        !out.contains("fn main()"),
        "AC-21: content hidden when tree focused\n{out}"
    );
    assert!(
        !out.contains("delta not found"),
        "AC-21: content notices hidden too\n{out}"
    );
}

#[test]
fn narrow_content_focus_gives_content_full_width_and_hides_tree() {
    let out = render(&state(60, Focus::Content), 60, 20);
    assert!(out.contains("fn main()"), "content shown full-width\n{out}");
    assert!(
        out.contains("delta not found"),
        "notices shown with content\n{out}"
    );
    assert!(
        !out.contains("scratch.log"),
        "AC-21: tree hidden when content focused\n{out}"
    );
}

#[test]
fn zoom_overrides_narrow_layout_and_fills_with_content() {
    // zoom hides the tree even below the 80-col narrow threshold and with the
    // tree focused — the content pane fills the frame regardless of width or focus.
    let mut st = state(60, Focus::Tree);
    st.zoomed = true;
    let out = render(&st, 60, 20);
    assert!(
        out.contains("fn main()"),
        "content fills the frame when zoomed, even narrow\n{out}"
    );
    assert!(
        !out.contains("scratch.log"),
        "the tree is hidden when zoomed, even narrow\n{out}"
    );
}

#[test]
fn wide_shows_both_columns_regardless_of_focus() {
    let out = render(&state(100, Focus::Tree), 100, 20);
    assert!(
        out.contains("scratch.log"),
        "tree column present at >= 80 cols\n{out}"
    );
    assert!(
        out.contains("fn main()"),
        "content column present at >= 80 cols\n{out}"
    );
}

#[test]
fn split_decision_follows_the_live_frame_not_a_stale_state_width() {
    // The narrow/wide decision must come from the frame the Presenter actually draws into,
    // so a stale state.width can never disagree with the geometry. Here state.width claims
    // "wide" (100) but the real pane is 60 → must render the narrow single-column layout.
    let mut st = state(100, Focus::Tree);
    let out = render(&st, 60, 20);
    assert!(out.contains("scratch.log"), "tree shown\n{out}");
    assert!(
        !out.contains("fn main()"),
        "narrow layout follows the 60-col frame, content hidden\n{out}"
    );

    // Conversely, a stale "narrow" width with a wide frame must show both columns.
    st.width = 40;
    let out = render(&st, 100, 20);
    assert!(
        out.contains("scratch.log") && out.contains("fn main()"),
        "wide frame → both columns\n{out}"
    );
}

#[test]
fn narrow_tree_snapshot() {
    insta::assert_snapshot!(
        "presenter_narrow_tree",
        render(&state(60, Focus::Tree), 60, 20)
    );
}

#[test]
fn narrow_content_snapshot() {
    insta::assert_snapshot!(
        "presenter_narrow_content",
        render(&state(60, Focus::Content), 60, 20)
    );
}

#[test]
fn annotation_title_marker_remains_visible_with_tree_hidden_in_narrow_and_zoom_layouts() {
    let mut narrow = state(60, Focus::Content);
    narrow.annotation_indicators.displayed_file_annotated = true;
    narrow.content_pad_left = true;
    let narrow_out = render(&narrow, 60, 10);
    assert!(
        narrow_out.contains("@main.rs"),
        "narrow transformed title\n{narrow_out}"
    );
    assert!(
        !narrow_out.contains("scratch.log"),
        "tree is hidden in narrow content focus"
    );

    let mut zoomed = state(100, Focus::Content);
    zoomed.annotation_indicators.displayed_file_annotated = true;
    zoomed.zoomed = true;
    let zoomed_out = render(&zoomed, 100, 10);
    assert!(
        zoomed_out.contains("@main.rs"),
        "zoomed title\n{zoomed_out}"
    );
    assert!(
        !zoomed_out.contains("scratch.log"),
        "tree is hidden while zoomed"
    );
}

#[test]
fn annotation_overview_stays_centered_and_windowed_in_a_narrow_layout() {
    let mut st = state(34, Focus::Content);
    let rows = (0..12)
        .map(|i| AnnotationRowView {
            target: AnnotationTargetView {
                path: PathBuf::from(format!("src/file-{i:02}.rs")),
                lines: Some(LineRange::new(i + 1, i + 1).unwrap()),
            },
            note: format!("narrow annotation {i:02} with a long tail"),
        })
        .collect::<Vec<_>>();
    st.annotation_count = rows.len();
    st.annotation_overview = Some(AnnotationOverviewView { rows, cursor: 10 });

    let out = render(&st, 34, 10);
    assert!(out.contains("Annotations (12)"), "overview title\n{out}");
    assert!(
        out.contains("file-10.rs"),
        "late selection is windowed into view\n{out}"
    );
    assert!(
        !out.contains("file-00.rs"),
        "early rows are off-screen\n{out}"
    );
    assert!(
        out.contains('…'),
        "rows truncate to the narrow modal\n{out}"
    );
    insta::assert_snapshot!("presenter_narrow_annotations", out);
}

#[test]
fn annotation_editor_stays_bounded_and_usable_in_a_narrow_layout() {
    let mut empty = state(34, Focus::Content);
    let target = AnnotationTargetView {
        path: PathBuf::from("src/界\u{1b}[2J/very-long-name.rs"),
        lines: Some(LineRange::new(8, 12).unwrap()),
    };
    empty.annotation_editor = Some(AnnotationEditorView {
        kind: AnnotationEditorKind::Add,
        target: target.clone(),
        text: String::new(),
        cursor: 0,
        error: None,
    });

    let mut mutable = state(34, Focus::Content);
    let text = format!("hidden\u{7} {}界🙂", "long input ".repeat(12));
    mutable.annotation_editor = Some(AnnotationEditorView {
        kind: AnnotationEditorKind::Add,
        target,
        cursor: text.find('界').unwrap(),
        text,
        error: Some(format!("invalid\u{1b}[2J {}", "long error ".repeat(12))),
    });

    let empty_out = render(&empty, 34, 10);
    let mutable_out = render(&mutable, 34, 10);
    let popup_horizontal_bounds = |output: &str| {
        let title_row = output
            .lines()
            .find(|line| line.contains("Add annotation"))
            .expect("annotation editor title row");
        let left = title_row.find('┌').expect("popup left border");
        let right = title_row.rfind('┐').expect("popup right border");
        (left, right - left + '┐'.len_utf8())
    };

    assert_eq!(
        popup_horizontal_bounds(&empty_out),
        popup_horizontal_bounds(&mutable_out),
        "mutable input and error do not move or widen the narrow popup"
    );
    assert!(
        mutable_out.contains("Add annotation"),
        "editor title\n{mutable_out}"
    );
    assert!(
        mutable_out.contains("界"),
        "cursor-follow scrolling keeps Unicode input visible\n{mutable_out}"
    );
    assert!(
        mutable_out
            .lines()
            .any(|line| line.contains("Target:") && line.contains('…')),
        "target truncates to the narrow inner width\n{mutable_out}"
    );
    assert!(
        mutable_out
            .lines()
            .any(|line| line.contains("invalid[2J") && line.contains('…')),
        "error truncates to the narrow inner width\n{mutable_out}"
    );
    assert!(!mutable_out.contains('\u{1b}') && !mutable_out.contains('\u{7}'));
}

#[test]
fn narrow_content_border_suppresses_overlapping_annotation_chip() {
    let mut st = state(20, Focus::Content);
    st.annotation_count = 9;
    let out = render(&st, 20, 8);
    let last = out.lines().last().unwrap();
    assert!(last.contains("? help"), "help remains discoverable\n{last}");
    assert!(
        !last.contains("annotations:"),
        "overlapping chip is omitted\n{last}"
    );
}
