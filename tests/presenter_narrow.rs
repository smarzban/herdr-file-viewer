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
    Node { path: PathBuf::from(path), kind, depth, expanded: true, status }
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
    }
}

fn render(state: &ViewState, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| draw(f, state)).unwrap();
    format!("{}", terminal.backend())
}

#[test]
fn narrow_tree_focus_gives_tree_full_width_and_hides_content() {
    let out = render(&state(60, Focus::Tree), 60, 20);
    assert!(out.contains("scratch.log"), "tree shown full-width\n{out}");
    assert!(!out.contains("fn main()"), "AC-21: content hidden when tree focused\n{out}");
    assert!(!out.contains("delta not found"), "AC-21: content notices hidden too\n{out}");
}

#[test]
fn narrow_content_focus_gives_content_full_width_and_hides_tree() {
    let out = render(&state(60, Focus::Content), 60, 20);
    assert!(out.contains("fn main()"), "content shown full-width\n{out}");
    assert!(out.contains("delta not found"), "notices shown with content\n{out}");
    assert!(!out.contains("scratch.log"), "AC-21: tree hidden when content focused\n{out}");
}

#[test]
fn wide_shows_both_columns_regardless_of_focus() {
    let out = render(&state(100, Focus::Tree), 100, 20);
    assert!(out.contains("scratch.log"), "tree column present at >= 80 cols\n{out}");
    assert!(out.contains("fn main()"), "content column present at >= 80 cols\n{out}");
}

#[test]
fn narrow_tree_snapshot() {
    insta::assert_snapshot!("presenter_narrow_tree", render(&state(60, Focus::Tree), 60, 20));
}

#[test]
fn narrow_content_snapshot() {
    insta::assert_snapshot!("presenter_narrow_content", render(&state(60, Focus::Content), 60, 20));
}
