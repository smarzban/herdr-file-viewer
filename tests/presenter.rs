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
    Node { path: PathBuf::from(path), kind, depth, expanded, status, dir_dirty: false }
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
        content_scroll: 0,
        content_hscroll: 0,
        wrap: false,
        split_pct: 40,
        zoomed: false,
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
    terminal
        .draw(|f| {
            draw(f, &state);
        })
        .unwrap();
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
fn content_pane_applies_the_vertical_scroll_offset() {
    // With content_scroll = N (and no wrap), the first visible content row is line N: the
    // lines above it are scrolled off the top.
    let mut state = sample_state();
    state.notices = vec![]; // no notice strip, so content starts at the inner top row
    state.content = to_text("c0\nc1\nc2\nc3\nc4\nc5\nc6\nc7\n");
    state.wrap = false;
    state.content_scroll = 3;
    let out = render(&state, 100, 12);
    assert!(out.contains("c3"), "the scrolled-to line is visible\n{out}");
    assert!(out.contains("c7"), "lines below it are visible\n{out}");
    assert!(!out.contains("c0"), "lines scrolled past the top are hidden\n{out}");
}

#[test]
fn wrapping_shows_more_of_a_long_line_than_truncating() {
    // A line far wider than the content pane: wrapped, it spills onto further rows; not
    // wrapped, it is truncated to a single row. So the wrapped render shows strictly more of
    // it. (The sample tree/title carry no 'W', so every 'W' counted comes from the content.)
    let long = "W ".repeat(80); // ~160 cols, far wider than the ~58-col content pane
    let mut state = sample_state();
    state.notices = vec![];
    state.content = to_text(&long);

    state.wrap = true;
    let wrapped = render(&state, 100, 12);
    state.wrap = false;
    let truncated = render(&state, 100, 12);

    let ws = |s: &str| s.matches('W').count();
    assert!(
        ws(&wrapped) > ws(&truncated),
        "wrapped shows more of the long line (wrapped W={}, truncated W={})\n{wrapped}",
        ws(&wrapped),
        ws(&truncated)
    );
}

/// Render to a buffer (for cell-style assertions).
fn render_buffer(state: &ViewState, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|f| {
            draw(f, state);
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

/// The foreground color of the first cell where `needle` begins in the buffer.
fn row_fg(buf: &ratatui::buffer::Buffer, needle: &str) -> ratatui::style::Color {
    let (w, h) = (buf.area().width, buf.area().height);
    for y in 0..h {
        for x in 0..w {
            let matches = needle.chars().enumerate().all(|(i, ch)| {
                let cx = x + i as u16;
                cx < w && buf.cell((cx, y)).is_some_and(|c| c.symbol() == ch.to_string())
            });
            if matches {
                return buf.cell((x, y)).unwrap().fg;
            }
        }
    }
    panic!("{needle:?} not found in buffer");
}

#[test]
fn tree_rows_are_colored_by_git_status() {
    use ratatui::style::Color;
    let mut state = sample_state();
    state.notices = vec![];
    state.nodes = vec![
        // A directory that contains changes (dir_dirty), a modified file, a new file, a clean
        // file. The clean file is selected so the colored rows aren't reversed (which would
        // swap fg/bg).
        Node {
            path: PathBuf::from("/r/src"),
            kind: NodeKind::Dir,
            depth: 0,
            expanded: true,
            status: None,
            dir_dirty: true,
        },
        node("/r/src/mod.rs", NodeKind::File, 1, false, Some(Status::Modified)),
        node("/r/src/new.rs", NodeKind::File, 1, false, Some(Status::Added)),
        node("/r/clean.txt", NodeKind::File, 0, false, None),
    ];
    state.selected = 3; // the clean file
    let buf = render_buffer(&state, 100, 12);

    assert_eq!(row_fg(&buf, "mod.rs"), Color::LightRed, "a modified file is light red");
    assert_eq!(row_fg(&buf, "new.rs"), Color::LightGreen, "a new file is light green");
    assert_eq!(row_fg(&buf, "src"), Color::LightRed, "a directory with changes is light red");
    assert_ne!(row_fg(&buf, "clean.txt"), Color::LightRed, "a clean file is not colored red");
    assert_ne!(row_fg(&buf, "clean.txt"), Color::LightGreen, "a clean file is not colored green");
}

#[test]
fn content_pane_applies_the_horizontal_scroll_offset() {
    // With content_hscroll = N (and no wrap), the leftmost N columns are scrolled off, so a
    // long line shows from column N onward.
    let mut state = sample_state();
    state.notices = vec![];
    state.content = to_text("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789");
    state.wrap = false;
    state.content_hscroll = 5;
    let out = render(&state, 100, 12);
    assert!(out.contains("FGHIJ"), "columns from the offset are visible\n{out}");
    assert!(!out.contains("ABCDE"), "columns scrolled past the left edge are hidden\n{out}");
}

#[test]
fn split_ratio_controls_the_tree_column_width() {
    // A larger split_pct gives the tree column more width, so its block's top-right corner
    // (the first `┐`) sits further along the top border row.
    let corner = |pct: u16| -> usize {
        let mut s = sample_state();
        s.split_pct = pct;
        let out = render(&s, 100, 24);
        out.lines().next().unwrap().find('┐').expect("tree block corner")
    };
    assert!(corner(60) > corner(20), "a wider split_pct widens the tree column");
}

#[test]
fn wide_layout_snapshot() {
    insta::assert_snapshot!("presenter_wide", render(&sample_state(), 100, 24));
}

#[test]
fn geometry_matches_the_wide_two_column_layout() {
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    let g = geometry(Rect { x: 0, y: 0, width: 100, height: 24 }, &sample_state());
    let t = g.tree_inner.expect("tree interior present in a wide layout");
    let c = g.content_inner.expect("content interior present in a wide layout");
    // Tree rows begin just inside the block border, so node `i` is at row `tree_inner.y + i`.
    assert_eq!(t.x, 1);
    assert_eq!(t.y, 1, "tree rows start just inside the top border");
    assert!(c.x > t.x + t.width, "the content column is to the right of the tree");
    assert!(g.divider_x.is_some(), "a wide layout has a draggable divider");
    assert_eq!((g.area_x, g.area_width), (0, 100));
}

#[test]
fn zoomed_layout_hides_the_tree_and_fills_with_content() {
    // The `z` zoom toggle hides the tree so the content pane fills the whole frame — even at a
    // wide width that would normally show both columns.
    let mut state = sample_state();
    state.zoomed = true;
    let out = render(&state, 100, 24);
    assert!(out.contains("fn main()"), "content fills the frame when zoomed\n{out}");
    // No tree-only rows survive: the directory row and a tree-only file are gone.
    assert!(!out.contains("scratch.log"), "the tree is hidden when zoomed\n{out}");
    assert!(!out.contains("added.rs"), "no tree rows are drawn when zoomed\n{out}");
}

#[test]
fn geometry_is_content_only_when_zoomed() {
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    let mut st = sample_state();
    st.zoomed = true;
    // Even at a wide width (which normally shows both columns), zoom draws content only.
    let g = geometry(Rect { x: 0, y: 0, width: 100, height: 24 }, &st);
    assert!(g.tree_inner.is_none(), "no tree interior when zoomed");
    assert!(g.content_inner.is_some(), "the content pane fills the frame when zoomed");
    assert!(g.divider_x.is_none(), "no divider when the tree is hidden");
}

#[test]
fn geometry_is_single_column_when_narrow() {
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    let mut st = sample_state();
    st.focus = Focus::Tree;
    // Below the 80-col narrow threshold only the focused column is drawn (AC-21).
    let g = geometry(Rect { x: 0, y: 0, width: 60, height: 24 }, &st);
    assert!(g.tree_inner.is_some(), "narrow + tree focus draws the tree");
    assert!(g.content_inner.is_none(), "the content column is not drawn when narrow");
    assert!(g.divider_x.is_none(), "no divider in a single-column layout");
}
