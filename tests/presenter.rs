//! T-14 — Presenter: two-column layout, recursive tree display, status markers, notices.
//! AC-3 (display), AC-7 (display), AC-13 (truncation notice), AC-25 (fallback notice).

use herdr_file_viewer::git::Status;
use herdr_file_viewer::presenter::{Focus, ViewState, draw};
use herdr_file_viewer::render::to_text;
use herdr_file_viewer::tree::{Node, NodeKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::PathBuf;

fn node(path: &str, kind: NodeKind, depth: usize, expanded: bool, status: Option<Status>) -> Node {
    Node { path: PathBuf::from(path), kind, depth, expanded, status }
}

/// A known tree+content+notices state, wide enough for the two-column layout.
fn sample_state() -> ViewState {
    let nodes = vec![
        node("/r/src", NodeKind::Dir, 0, true, None),
        node("/r/src/main.rs", NodeKind::File, 1, false, Some(Status::Modified)),
        node("/r/src/inner", NodeKind::Dir, 1, true, None),
        node("/r/src/inner/added.rs", NodeKind::File, 2, false, Some(Status::Added)),
        node("/r/README.md", NodeKind::File, 0, false, None),
        node("/r/gone.txt", NodeKind::File, 0, false, Some(Status::Deleted)),
        node("/r/scratch.log", NodeKind::File, 0, false, Some(Status::Untracked)),
    ];
    ViewState {
        nodes,
        selected: 1, // main.rs
        content: to_text("fn main() {\n    println!(\"hello\");\n}\n"),
        notices: vec![
            "Showing first 5000 lines (truncated)".to_string(), // AC-13
            "delta not found — showing plain diff".to_string(), // AC-25
        ],
        focus: Focus::Tree,
        width: 100,
    }
}

fn render(state: &ViewState, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| draw(f, state)).unwrap();
    format!("{}", terminal.backend())
}

#[test]
fn draws_two_columns_tree_and_content() {
    let out = render(&sample_state(), 100, 24);

    // Left column: the tree, with every visible node shown (AC-3 — recursive display).
    for name in ["src", "main.rs", "inner", "added.rs", "README.md", "gone.txt", "scratch.log"] {
        assert!(out.contains(name), "tree should show {name}\n{out}");
    }
    // Right column: the content pane (two columns are present simultaneously).
    assert!(out.contains("fn main()"), "content pane should render the file\n{out}");
    assert!(out.contains("hello"), "content pane should render the file body\n{out}");
}

#[test]
fn shows_status_markers_for_each_state() {
    // AC-7 (display): modified / added / deleted / untracked markers appear in the tree.
    let out = render(&sample_state(), 100, 24);
    for marker in ['M', 'A', 'D', '?'] {
        assert!(out.contains(marker), "status marker {marker:?} should appear\n{out}");
    }
}

#[test]
fn nests_deeper_nodes_with_more_indentation() {
    // AC-3 (display): a depth-2 file is indented further than a depth-0 file.
    let out = render(&sample_state(), 100, 24);
    let indent = |needle: &str| -> usize {
        let line = out.lines().find(|l| l.contains(needle)).expect("row present");
        let start = line.find(needle).unwrap();
        line[..start].chars().rev().take_while(|c| *c == ' ').count()
    };
    assert!(
        indent("added.rs") > indent("src"),
        "deeper nodes indent more: added.rs={} src={}\n{out}",
        indent("added.rs"),
        indent("src")
    );
}

#[test]
fn surfaces_truncation_and_fallback_notices() {
    // AC-13 + AC-25: both notices are visible in the content area.
    let out = render(&sample_state(), 100, 24);
    assert!(out.contains("truncated"), "truncation notice (AC-13) visible\n{out}");
    assert!(out.contains("delta not found"), "fallback notice (AC-25) visible\n{out}");
}

#[test]
fn hostile_file_name_emits_no_control_bytes_to_the_buffer() {
    // AC-27 (defense-in-depth): a repo-controlled file name carrying screen-clear / cursor-
    // move sequences must never reach the terminal as control bytes when drawn in the tree.
    let hostile = "evil\u{1b}[2J\u{1b}[10;10H\u{07}pwned";
    let mut state = sample_state();
    state.nodes = vec![node(&format!("/r/{hostile}"), NodeKind::File, 0, false, Some(Status::Modified))];
    state.selected = 0;

    let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
    terminal.draw(|f| draw(f, &state)).unwrap();
    let buf = terminal.backend().buffer().clone();

    let mut cells = String::new();
    for y in 0..buf.area().height {
        for x in 0..buf.area().width {
            if let Some(c) = buf.cell((x, y)) {
                cells.push_str(c.symbol());
            }
        }
    }
    assert!(!cells.chars().any(|c| c.is_control()), "no control byte may reach a cell");
    assert!(!cells.contains('\u{1b}'), "no ESC byte in the buffer");
    assert!(cells.contains("pwned"), "the printable remainder is still shown");
}

#[test]
fn wide_layout_snapshot() {
    insta::assert_snapshot!("presenter_wide", render(&sample_state(), 100, 24));
}
