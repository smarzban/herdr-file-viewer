//! T-15 — Presenter: narrow-split focus-toggle (< 80 cols), AC-21.
//! Under 80 columns the focused column takes the full width and the other is hidden;
//! at ≥ 80 columns both columns are shown.

use herdr_file_viewer::git::Status;
use herdr_file_viewer::presenter::{Focus, ViewState, draw};
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
        split_pct: 40,
        zoomed: false,
        update_banner: None,
        picker: None,
        finder: None,
        root_name: "r".to_string(), // the fixture tree is rooted at /r
        branch: None,
        prompt: None,
        search: None,
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
    // review-gate R1: zoom hides the tree even below the 80-col narrow threshold and with the
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
