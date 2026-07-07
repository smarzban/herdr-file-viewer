//! Presenter: two-column layout, recursive tree display, status markers, notices.
//! AC-3 (display), AC-7 (display), AC-13 (truncation notice), AC-25 (fallback notice).

use herdr_file_viewer::git::Status;
use herdr_file_viewer::presenter::{
    ContentSearch, FinderView, Focus, HelpView, LineSelectView, PickerRowView, PickerView,
    ViewState, draw,
};
use herdr_file_viewer::render::to_text;
use herdr_file_viewer::search::Match;
use herdr_file_viewer::tree::{Node, NodeKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::PathBuf;

fn node(path: &str, kind: NodeKind, depth: usize, expanded: bool, status: Option<Status>) -> Node {
    Node {
        path: PathBuf::from(path),
        kind,
        depth,
        expanded,
        status,
        dir_dirty: false,
    }
}

/// A known tree+content+notices state, wide enough for the two-column layout.
fn sample_state() -> ViewState {
    let nodes = vec![
        node("/r/src", NodeKind::Dir, 0, true, None),
        node(
            "/r/src/main.rs",
            NodeKind::File,
            1,
            false,
            Some(Status::Modified),
        ),
        node("/r/src/inner", NodeKind::Dir, 1, true, None),
        node(
            "/r/src/inner/added.rs",
            NodeKind::File,
            2,
            false,
            Some(Status::Added),
        ),
        node("/r/README.md", NodeKind::File, 0, false, None),
        node(
            "/r/gone.txt",
            NodeKind::File,
            0,
            false,
            Some(Status::Deleted),
        ),
        node(
            "/r/scratch.log",
            NodeKind::File,
            0,
            false,
            Some(Status::Untracked),
        ),
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
        tree_scroll: 0,
        tree_hscroll: 0,
        content_rows: 3, // the fixture content is three lines
        wrap: false,
        split_pct: 40,
        zoomed: false,
        update_banner: None,
        picker: None,
        finder: None,
        root_name: "r".to_string(), // the fixture tree is rooted at /r
        branch: None,
        prompt: None,
        content_title: Some("main.rs".to_string()),
        content_rendering: false,
        search: None,
        line_select: None,
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
fn tree_borders_show_root_name_on_top_and_branch_on_bottom() {
    // the tree column's TOP border is the root directory basename (not the old static
    // "Files"), and the BOTTOM border is the current branch when present.
    let mut state = sample_state();
    state.root_name = "myrepo".to_string();
    state.branch = Some("featzz".to_string());
    let out = render(&state, 100, 24);
    assert!(
        out.contains("myrepo"),
        "tree top border shows the root basename\n{out}"
    );
    assert!(
        out.contains("featzz"),
        "tree bottom border shows the current branch\n{out}"
    );
    assert!(
        !out.contains("Files"),
        "the static 'Files' title is gone\n{out}"
    );

    // Outside a repo (branch None) the branch is omitted entirely — no leftover label.
    state.branch = None;
    let out = render(&state, 100, 24);
    assert!(
        out.contains("myrepo"),
        "top border still shows the root name when branchless\n{out}"
    );
    assert!(
        !out.contains("featzz"),
        "no branch is rendered when branch is None\n{out}"
    );
}

#[test]
fn tree_title_truncates_an_overlong_root_name() {
    // a root name wider than the tree column is truncated with an ellipsis so it can't
    // break the border.
    let mut state = sample_state();
    state.root_name = "x".repeat(80);
    let out = render(&state, 100, 24);
    assert!(
        out.contains('…'),
        "an over-long root name is truncated with an ellipsis\n{out}"
    );
    assert!(
        !out.contains(&"x".repeat(70)),
        "the full over-long name is not rendered (it was truncated)\n{out}"
    );
}

#[test]
fn tree_branch_uses_middle_ellipsis_when_long() {
    // A long branch on the bottom border is truncated in the MIDDLE so both the
    // prefix and the trailing (most distinctive) part stay visible, instead of losing the tail.
    let mut state = sample_state();
    state.root_name = "repo".to_string();
    state.branch = Some(format!("PFX{}SFX", "z".repeat(60)));
    let out = render(&state, 100, 24);
    assert!(out.contains('…'), "the long branch is truncated\n{out}");
    assert!(
        out.contains("PFX"),
        "the branch prefix stays visible (head kept)\n{out}"
    );
    assert!(
        out.contains("SFX"),
        "the branch suffix stays visible (tail kept — the middle-ellipsis point)\n{out}"
    );
    assert!(
        !out.contains(&"z".repeat(40)),
        "the middle is dropped (the full run is not rendered)\n{out}"
    );
}

#[test]
fn draws_two_columns_tree_and_content() {
    let out = render(&sample_state(), 100, 24);

    // Left column: the tree, with every visible node shown (AC-3 — recursive display).
    for name in [
        "src",
        "main.rs",
        "inner",
        "added.rs",
        "README.md",
        "gone.txt",
        "scratch.log",
    ] {
        assert!(out.contains(name), "tree should show {name}\n{out}");
    }
    // Right column: the content pane (two columns are present simultaneously).
    assert!(
        out.contains("fn main()"),
        "content pane should render the file\n{out}"
    );
    assert!(
        out.contains("hello"),
        "content pane should render the file body\n{out}"
    );
}

#[test]
fn shows_status_markers_for_each_state() {
    // AC-7 (display): modified / added / deleted / untracked markers appear in the tree.
    let out = render(&sample_state(), 100, 24);
    for marker in ['M', 'A', 'D', '?'] {
        assert!(
            out.contains(marker),
            "status marker {marker:?} should appear\n{out}"
        );
    }
}

#[test]
fn dirty_directory_carries_a_non_color_glyph_marker() {
    // a directory containing any change (dir_dirty) shows a `●` glyph beside it, so the
    // "dirty directory" state is distinguishable with color stripped — previously it was color-only
    // (LightRed) and lost to a colorblind user or a non-default theme. Files keep their M/A/D/?
    // letters; clean directories and clean files show a blank, so the column stays aligned.
    let mut state = sample_state();
    state.notices = vec![];
    // A dirty directory (dir_dirty = true) at the root, plus a clean directory for contrast.
    state.nodes = vec![
        Node {
            path: PathBuf::from("/r/changed"),
            kind: NodeKind::Dir,
            depth: 0,
            expanded: true,
            status: None,
            dir_dirty: true,
        },
        Node {
            path: PathBuf::from("/r/clean"),
            kind: NodeKind::Dir,
            depth: 0,
            expanded: true,
            status: None,
            dir_dirty: false,
        },
    ];
    state.selected = 1; // the clean dir, so the dirty dir row isn't REVERSED
    let out = render(&state, 100, 24);
    // The dirty directory row carries `●`; the clean directory row carries a blank.
    let dirty_line = out
        .lines()
        .find(|l| l.contains("▾ changed") || l.contains("▸ changed"))
        .expect("the dirty directory row is drawn");
    assert!(
        dirty_line.contains('●'),
        "the dirty directory shows a `●` glyph (non-color cue)\n{dirty_line}"
    );
    let clean_line = out
        .lines()
        .find(|l| l.contains("▾ clean") || l.contains("▸ clean"))
        .expect("the clean directory row is drawn");
    assert!(
        !clean_line.contains('●'),
        "a clean directory shows no `●` glyph (only dirty dirs do)\n{clean_line}"
    );
}

#[test]
fn dirty_directory_glyph_snapshot() {
    // snapshot the tree with a dirty directory so the `●` glyph is locked into the
    // recorded layout — a regression that drops the glyph (back to color-only) is caught here.
    let mut state = sample_state();
    state.notices = vec![];
    state.nodes = vec![
        Node {
            path: PathBuf::from("/r/changed"),
            kind: NodeKind::Dir,
            depth: 0,
            expanded: true,
            status: None,
            dir_dirty: true,
        },
        node(
            "/r/changed/a.rs",
            NodeKind::File,
            1,
            false,
            Some(Status::Modified),
        ),
        node(
            "/r/changed/b.rs",
            NodeKind::File,
            1,
            false,
            Some(Status::Added),
        ),
        Node {
            path: PathBuf::from("/r/clean"),
            kind: NodeKind::Dir,
            depth: 0,
            expanded: false,
            status: None,
            dir_dirty: false,
        },
        node(
            "/r/gone.txt",
            NodeKind::File,
            0,
            false,
            Some(Status::Deleted),
        ),
        node(
            "/r/scratch.log",
            NodeKind::File,
            0,
            false,
            Some(Status::Untracked),
        ),
    ];
    state.selected = 5; // scratch.log, so the dirty dir row isn't REVERSED
    insta::assert_snapshot!("presenter_dirty_dir_glyph", render(&state, 60, 14));
}

#[test]
fn nests_deeper_nodes_with_more_indentation() {
    // AC-3 (display): a depth-2 file is indented further than a depth-0 file.
    let out = render(&sample_state(), 100, 24);
    let indent = |needle: &str| -> usize {
        let line = out
            .lines()
            .find(|l| l.contains(needle))
            .expect("row present");
        let start = line.find(needle).unwrap();
        line[..start]
            .chars()
            .rev()
            .take_while(|c| *c == ' ')
            .count()
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
    assert!(
        out.contains("truncated"),
        "truncation notice (AC-13) visible\n{out}"
    );
    assert!(
        out.contains("delta not found"),
        "fallback notice (AC-25) visible\n{out}"
    );
}

#[test]
fn hostile_file_name_emits_no_control_bytes_to_the_buffer() {
    // AC-27 (defense-in-depth): a repo-controlled file name carrying screen-clear / cursor-
    // move sequences must never reach the terminal as control bytes when drawn in the tree.
    let hostile = "evil\u{1b}[2J\u{1b}[10;10H\u{07}pwned";
    let mut state = sample_state();
    state.nodes = vec![node(
        &format!("/r/{hostile}"),
        NodeKind::File,
        0,
        false,
        Some(Status::Modified),
    )];
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
    assert!(
        !cells.chars().any(|c| c.is_control()),
        "no control byte may reach a cell"
    );
    assert!(!cells.contains('\u{1b}'), "no ESC byte in the buffer");
    assert!(
        cells.contains("pwned"),
        "the printable remainder is still shown"
    );
}

#[test]
fn hostile_notice_emits_no_control_bytes_to_the_buffer() {
    // AC-27 (defense-in-depth): an action notice carrying a worktree path with screen-clear /
    // cursor-move sequences must be sanitized at the notice sink, exactly like tree rows, the
    // content title, and picker rows — no control byte may reach the terminal as drawn.
    let hostile = "switched to \u{1b}[2J\u{1b}[10;10H\u{07}/work/pwned";
    let mut state = sample_state();
    state.notices = vec![hostile.to_string()];

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
    assert!(
        !cells.chars().any(|c| c.is_control()),
        "no control byte may reach a cell from a notice"
    );
    assert!(!cells.contains('\u{1b}'), "no ESC byte in the buffer");
    assert!(
        cells.contains("/work/pwned"),
        "the printable remainder of the notice is still shown"
    );
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
    assert!(
        !out.contains("c0"),
        "lines scrolled past the top are hidden\n{out}"
    );
}

#[test]
fn tree_scrolls_to_keep_selection_visible() {
    // #45: with more nodes than fit the tree interior, moving the selection toward the end must
    // scroll the tree window so the selected row stays visible. Pre-fix the tree rendered every
    // node into the fixed interior with no scroll offset, so a selection past the fold never
    // reached the buffer — the reported bug ("I can see files being selected but the tree
    // doesn't move"). Mirrors the picker's keeps-cursor-visible test.
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;
    let mut state = sample_state();
    state.notices = vec![];
    // 40 files — far more than fit the ~22-row tree interior at 100x24.
    state.nodes = (0..40)
        .map(|i| {
            node(
                &format!("/r/file-{i:02}.rs"),
                NodeKind::File,
                0,
                false,
                None,
            )
        })
        .collect();
    state.selected = 37; // near the end

    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 24,
    };
    let buf = render_buffer(&state, area.width, area.height);
    // Scope the search to the TREE interior. The content pane's block title is the SELECTED
    // node's name ("file-37.rs"), so a frame-wide search would false-match there even when the
    // tree never scrolled — the bug. Bounding to `tree_inner` asserts the row is in the tree.
    let t = geometry(area, &state).tree_inner.expect("tree interior");

    // The selected row's distinctive name must be present in the tree (the window scrolled to it)...
    let needle = "file-37";
    let mut found_at: Option<(u16, u16)> = None;
    'scan: for y in t.y..(t.y + t.height) {
        for x in t.x..(t.x + t.width) {
            let matches = needle.chars().enumerate().all(|(i, ch)| {
                let cx = x + i as u16;
                cx < t.x + t.width
                    && buf
                        .cell((cx, y))
                        .is_some_and(|c| c.symbol() == ch.to_string())
            });
            if matches {
                found_at = Some((x, y));
                break 'scan;
            }
        }
    }
    let (cx, cy) =
        found_at.expect("the selected tree row (file-37) must be visible after scrolling");
    // ...and it carries the REVERSED selection highlight, so it reads as selected.
    assert!(
        buf.cell((cx, cy))
            .unwrap()
            .modifier
            .contains(Modifier::REVERSED),
        "the visible selected row must be REVERSED-highlighted"
    );
    // And an early node has scrolled off the top of the tree.
    let mut early_in_tree = false;
    for y in t.y..(t.y + t.height) {
        for x in t.x..(t.x + t.width) {
            if "file-00".chars().enumerate().all(|(i, ch)| {
                let cx = x + i as u16;
                cx < t.x + t.width
                    && buf
                        .cell((cx, y))
                        .is_some_and(|c| c.symbol() == ch.to_string())
            }) {
                early_in_tree = true;
            }
        }
    }
    assert!(
        !early_in_tree,
        "early nodes (file-00) scroll off the top when the selection is near the end"
    );
}

#[test]
fn geometry_reports_the_tree_scroll_offset_for_hit_testing() {
    // #45 coupling: the geometry fed back to the controller carries the SAME scroll offset
    // draw_tree applied, so a click maps to the node drawn on that row. It is 0 when every node
    // fits, and when overflowing it keeps the selection inside the visible window.
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 24,
    };

    // Few nodes: no scroll.
    assert_eq!(
        geometry(area, &sample_state()).tree_scroll,
        0,
        "tree_scroll is 0 when every node fits the interior"
    );

    // Many nodes, selection near the end: the window scrolls and keeps the selection visible.
    let mut many = sample_state();
    many.notices = vec![];
    many.nodes = (0..40)
        .map(|i| {
            node(
                &format!("/r/file-{i:02}.rs"),
                NodeKind::File,
                0,
                false,
                None,
            )
        })
        .collect();
    many.selected = 37;
    let g = geometry(area, &many);
    let t = g.tree_inner.expect("tree interior");
    assert!(g.tree_scroll > 0, "an overflowing tree scrolls");
    let off = g.tree_scroll as usize;
    assert!(
        off <= many.selected && many.selected < off + t.height as usize,
        "the selection (37) stays within the visible window [{off}, {})",
        off + t.height as usize
    );
}

#[test]
fn tree_shows_a_vertical_scrollbar_only_when_it_overflows() {
    // The tree gets a vertical scrollbar exactly when there are more nodes than fit (#45 follow-up:
    // "add a scrollbar where there is something to be moved"). The thumb is a half-block bar
    // (▐), distinct from the light border (│) and absent elsewhere in these fixtures, so a
    // frame-wide check is unambiguous: the only ▐ here is the tree scrollbar thumb.
    let fits = render(&sample_state(), 100, 24); // 7 nodes in a 24-row frame → fits
    assert!(
        !fits.contains('▐'),
        "a tree that fits shows no scrollbar\n{fits}"
    );

    let mut many = sample_state();
    many.notices = vec![];
    many.nodes = (0..40)
        .map(|i| {
            node(
                &format!("/r/file-{i:02}.rs"),
                NodeKind::File,
                0,
                false,
                None,
            )
        })
        .collect();
    many.selected = 0;
    let overflow = render(&many, 100, 24);
    assert!(
        overflow.contains('▐'),
        "an overflowing tree shows a scrollbar thumb (▐)\n{overflow}"
    );
}

#[test]
fn tree_shows_a_horizontal_scrollbar_and_scrolls_long_rows() {
    // The tree gets a horizontal scrollbar (▄ thumb on the bottom border) + horizontal scroll when
    // the widest row overflows the column — so a long / deeply-nested name can be read sideways.
    // `tree_hscroll` clips the leading columns, like the content pane. Assertions are scoped to the
    // TREE column: the content pane's block title is the selected node's name, so a frame-wide
    // check would false-match the head there regardless of the tree's horizontal scroll.
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 24,
    };
    let mut state = sample_state();
    state.notices = vec![];
    let long = format!("START_{}_END", "x".repeat(60));
    state.nodes = vec![node(&format!("/r/{long}"), NodeKind::File, 0, false, None)];
    state.selected = 0;

    // The text inside the tree column (interior + its borders), one line per row.
    let tree_region = |st: &ViewState| -> String {
        let buf = render_buffer(st, area.width, area.height);
        let t = geometry(area, st).tree_inner.expect("tree interior");
        let mut s = String::new();
        for y in t.y..(t.y + t.height) {
            for x in t.x..(t.x + t.width) {
                s.push_str(buf.cell((x, y)).map_or(" ", |c| c.symbol()));
            }
            s.push('\n');
        }
        s
    };

    state.tree_hscroll = 0;
    let head_region = tree_region(&state);
    let full = render(&state, area.width, area.height);
    assert!(
        full.contains('▄'),
        "an overflowing tree shows a horizontal scrollbar (▄)\n{full}"
    );
    assert!(
        head_region.contains("START_"),
        "the row head shows in the tree at hscroll 0\n{head_region}"
    );
    assert!(
        !head_region.contains("_END"),
        "the row tail is off-screen in the tree at hscroll 0\n{head_region}"
    );

    state.tree_hscroll = 45;
    let tail_region = tree_region(&state);
    assert!(
        tail_region.contains("_END"),
        "scrolling right reveals the row tail in the tree\n{tail_region}"
    );
    assert!(
        !tail_region.contains("START_"),
        "the head is clipped in the tree once scrolled right\n{tail_region}"
    );
}

#[test]
fn scrollbars_are_inside_the_pane_with_a_one_column_gap() {
    // The bars live INSIDE the box (a reserved gutter), not on the border, with a one-cell gap
    // between the text and the bar. Asserted structurally via geometry: the vertical bar is a
    // 1-col track exactly one column right of the text's right edge (the gap), spanning the text
    // rows; the horizontal bar is one row below the text's bottom edge.
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 24,
    };
    let mut state = sample_state();
    state.notices = vec![];
    // Overflow both ways: many rows (vbar) + one very wide row (hbar).
    state.nodes = (0..40)
        .map(|i| {
            node(
                &format!("/r/file-{i:02}.rs"),
                NodeKind::File,
                0,
                false,
                None,
            )
        })
        .collect();
    state.nodes[0] = node(
        &format!("/r/{}", "w".repeat(80)),
        NodeKind::File,
        0,
        false,
        None,
    );
    state.selected = 0;

    let g = geometry(area, &state);
    let t = g.tree_inner.expect("tree text rect");
    let v = g
        .tree_vbar
        .expect("tree vbar present when overflowing vertically");
    let h = g
        .tree_hbar
        .expect("tree hbar present when overflowing horizontally");

    assert_eq!(v.width, 1, "the vertical bar is one column wide");
    assert_eq!(
        v.x,
        t.x + t.width + 1,
        "exactly one gap column between the text and the vertical bar (bar is inside, not on the border)"
    );
    assert_eq!(v.height, t.height, "the vertical bar spans the text rows");

    assert_eq!(h.height, 1, "the horizontal bar is one row tall");
    assert_eq!(
        h.y,
        t.y + t.height + 1,
        "exactly one gap row between the text and the horizontal bar"
    );
    assert_eq!(
        h.x, t.x,
        "the horizontal bar starts at the text's left edge"
    );
}

#[test]
fn content_pane_shows_a_vertical_scrollbar_when_content_overflows() {
    // The content pane gets a vertical scrollbar when it has more lines than the viewport is tall.
    // The tree (7 nodes) does NOT overflow a 12-row frame, so the only ▐ is the content scrollbar.
    let mut state = sample_state();
    state.notices = vec![];
    state.content = to_text(
        &(0..60)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    state.content_rows = 60; // the controller's rendered-row count drives the vertical bar
    let out = render(&state, 100, 12);
    assert!(
        out.contains('▐'),
        "overflowing content shows a vertical scrollbar (▐)\n{out}"
    );

    // A short file shows none.
    state.content = to_text("only\ntwo\n");
    state.content_rows = 2;
    let short = render(&state, 100, 12);
    assert!(
        !short.contains('▐'),
        "content that fits shows no vertical scrollbar\n{short}"
    );
}

#[test]
fn content_vertical_scrollbar_is_driven_by_rendered_rows_not_raw_lines() {
    // Review (codex/opus/kimi/glm): under wrap, the vertical bar must reflect WRAPPED rows, not raw
    // lines — else a file with few but long lines that wraps past the viewport gets no bar. The
    // presenter sizes the bar from `content_rows` (the controller's rendered-row count), so a single
    // raw line with a large content_rows shows a bar, and a small content_rows shows none.
    let mut state = sample_state();
    state.notices = vec![];
    state.content = to_text(&"word ".repeat(400)); // ONE raw line
    state.wrap = true;

    state.content_rows = 60; // wraps to ~60 rows >> the ~22-row viewport
    let overflow = render(&state, 100, 24);
    assert!(
        overflow.contains('▐'),
        "a single long line that wraps past the viewport shows a vertical scrollbar\n{overflow}"
    );

    state.content_rows = 5; // wraps to only 5 rows → fits
    let fits = render(&state, 100, 24);
    assert!(
        !fits.contains('▐'),
        "no vertical bar when the wrapped rows fit (despite content_scroll units)\n{fits}"
    );
}

#[test]
fn tree_vertical_thumb_tracks_the_selection() {
    // Review (codex/glm): the tree's vertical thumb reflects the CURSOR position, so dragging it
    // (which scrubs the selection) makes the thumb follow — rather than the thumb being driven by
    // the viewport offset while the drag moves the cursor (they'd diverge). With many nodes, a low
    // selection puts the thumb near the top; a high selection puts it near the bottom.
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 24,
    };
    let mut state = sample_state();
    state.notices = vec![];
    state.nodes = (0..40)
        .map(|i| {
            node(
                &format!("/r/file-{i:02}.rs"),
                NodeKind::File,
                0,
                false,
                None,
            )
        })
        .collect();
    let track = geometry(area, &state).tree_vbar.expect("tree vbar present");
    let thumb_top = |st: &ViewState| -> u16 {
        let buf = render_buffer(st, area.width, area.height);
        (track.y..track.y + track.height)
            .find(|&y| buf.cell((track.x, y)).is_some_and(|c| c.symbol() == "▐"))
            .expect("a thumb cell")
    };

    state.selected = 0;
    let low = thumb_top(&state);
    state.selected = 39;
    let high = thumb_top(&state);
    assert!(
        high > low,
        "the thumb moves down as the selection moves down (tracks the cursor): sel0={low} sel39={high}"
    );
}

#[test]
fn content_vertical_scrollbar_thumb_reaches_the_bottom_at_max_scroll() {
    // Review (codex): at the last scroll position the thumb must reach the bottom of the track —
    // stopping short would falsely imply more content remains. The bar is thumb-only (no track
    // line), so use its fed-back rect for the track extent and check where the thumb (`▐`) lands:
    // at scroll 0 it includes the top row but not the bottom; at max scroll it reaches the bottom.
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    let (w, h) = (100u16, 18u16);
    let area = Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };
    let mut state = sample_state();
    state.notices = vec![];
    // 60 lines into a 16-row text area → max scroll 44.
    state.content = to_text(
        &(0..60)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    state.wrap = false;
    state.content_rows = 60; // the rendered-row count drives the vertical bar
    let track = geometry(area, &state)
        .content_vbar
        .expect("content vbar present");
    let thumb_rows = |buf: &ratatui::buffer::Buffer| -> Vec<u16> {
        (track.y..track.y + track.height)
            .filter(|&y| buf.cell((track.x, y)).is_some_and(|c| c.symbol() == "▐"))
            .collect()
    };

    state.content_scroll = 0;
    let top = thumb_rows(&render_buffer(&state, w, h));
    assert!(
        top.contains(&track.y),
        "thumb is at the top of the track at scroll 0"
    );
    assert!(
        !top.contains(&(track.y + track.height - 1)),
        "thumb is NOT at the bottom at scroll 0"
    );

    state.content_scroll = 44; // the max scroll for this content/viewport
    let bottom = thumb_rows(&render_buffer(&state, w, h));
    assert!(
        bottom.contains(&(track.y + track.height - 1)),
        "thumb reaches the bottom of the track at max scroll"
    );
}

#[test]
fn content_pane_shows_a_horizontal_scrollbar_for_a_too_wide_unwrapped_line() {
    // A line far wider than the content pane gets a horizontal scrollbar when NOT wrapped (so it
    // can be read sideways). Its thumb is a half-block bar (▄), distinct from the block's
    // light border (─), so it is an unambiguous marker. Wrapping the same line removes the
    // overflow, so no horizontal scrollbar is drawn.
    let mut state = sample_state();
    state.notices = vec![];
    state.content = to_text(&"x".repeat(300)); // far wider than the ~58-col content pane

    state.wrap = false;
    let unwrapped = render(&state, 100, 24);
    assert!(
        unwrapped.contains('▄'),
        "a too-wide unwrapped line shows a horizontal scrollbar (▄)\n{unwrapped}"
    );

    state.wrap = true;
    let wrapped = render(&state, 100, 24);
    assert!(
        !wrapped.contains('▄'),
        "a wrapped line needs no horizontal scrollbar\n{wrapped}"
    );
}

#[test]
fn tree_scroll_is_sticky_when_the_selection_stays_visible() {
    // #45 follow-up (review): selecting a row already on screen — e.g. a mouse click — must NOT
    // move the viewport. Pre-fix the offset was a pure function of the cursor, so clicking a
    // visible row in a scrolled tree snapped the view (and made a double-click land on the wrong
    // row). With sticky scrolling, geometry reports the SAME offset when the selection is in view.
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 24,
    };
    let mut state = sample_state();
    state.notices = vec![];
    state.nodes = (0..40)
        .map(|i| {
            node(
                &format!("/r/file-{i:02}.rs"),
                NodeKind::File,
                0,
                false,
                None,
            )
        })
        .collect();
    let t_height = geometry(area, &state)
        .tree_inner
        .expect("tree interior")
        .height as usize;

    // Currently scrolled down by 10; select a row INSIDE the visible window [10, 10+height).
    state.tree_scroll = 10;
    state.selected = 12;
    assert!(
        12 < 10 + t_height,
        "precondition: the selection is within the current window"
    );
    assert_eq!(
        geometry(area, &state).tree_scroll,
        10,
        "selecting a visible row keeps the offset — the tree does not jump"
    );

    // Selecting a row ABOVE the window scrolls minimally up to it (cursor at the top edge).
    state.selected = 4;
    assert_eq!(
        geometry(area, &state).tree_scroll,
        4,
        "a selection above the window scrolls up just to it"
    );
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
                cx < w
                    && buf
                        .cell((cx, y))
                        .is_some_and(|c| c.symbol() == ch.to_string())
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
        node(
            "/r/src/mod.rs",
            NodeKind::File,
            1,
            false,
            Some(Status::Modified),
        ),
        node(
            "/r/src/new.rs",
            NodeKind::File,
            1,
            false,
            Some(Status::Added),
        ),
        node("/r/clean.txt", NodeKind::File, 0, false, None),
    ];
    state.selected = 3; // the clean file
    let buf = render_buffer(&state, 100, 12);

    assert_eq!(
        row_fg(&buf, "mod.rs"),
        Color::LightRed,
        "a modified file is light red"
    );
    assert_eq!(
        row_fg(&buf, "new.rs"),
        Color::LightGreen,
        "a new file is light green"
    );
    assert_eq!(
        row_fg(&buf, "src"),
        Color::LightRed,
        "a directory with changes is light red"
    );
    assert_ne!(
        row_fg(&buf, "clean.txt"),
        Color::LightRed,
        "a clean file is not colored red"
    );
    assert_ne!(
        row_fg(&buf, "clean.txt"),
        Color::LightGreen,
        "a clean file is not colored green"
    );
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
    assert!(
        out.contains("FGHIJ"),
        "columns from the offset are visible\n{out}"
    );
    assert!(
        !out.contains("ABCDE"),
        "columns scrolled past the left edge are hidden\n{out}"
    );
}

#[test]
fn split_ratio_controls_the_tree_column_width() {
    // A larger split_pct gives the tree column more width, so its block's top-right corner
    // (the first `┐`) sits further along the top border row.
    let corner = |pct: u16| -> usize {
        let mut s = sample_state();
        s.split_pct = pct;
        let out = render(&s, 100, 24);
        out.lines()
            .next()
            .unwrap()
            .find('┐')
            .expect("tree block corner")
    };
    assert!(
        corner(60) > corner(20),
        "a wider split_pct widens the tree column"
    );
}

#[test]
fn wide_layout_snapshot() {
    insta::assert_snapshot!("presenter_wide", render(&sample_state(), 100, 24));
}

#[test]
fn geometry_matches_the_wide_two_column_layout() {
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    let g = geometry(
        Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 24,
        },
        &sample_state(),
    );
    let t = g
        .tree_inner
        .expect("tree interior present in a wide layout");
    let c = g
        .content_inner
        .expect("content interior present in a wide layout");
    // Tree rows begin just inside the block border, so node `i` is at row `tree_inner.y + i`.
    assert_eq!(t.x, 1);
    assert_eq!(t.y, 1, "tree rows start just inside the top border");
    assert!(
        c.x > t.x + t.width,
        "the content column is to the right of the tree"
    );
    assert!(
        g.divider_x.is_some(),
        "a wide layout has a draggable divider"
    );
    assert_eq!((g.area_x, g.area_width), (0, 100));
}

#[test]
fn zoomed_layout_hides_the_tree_and_fills_with_content() {
    // The `z` zoom toggle hides the tree so the content pane fills the whole frame — even at a
    // wide width that would normally show both columns.
    let mut state = sample_state();
    state.zoomed = true;
    let out = render(&state, 100, 24);
    assert!(
        out.contains("fn main()"),
        "content fills the frame when zoomed\n{out}"
    );
    // No tree-only rows survive: the directory row and a tree-only file are gone.
    assert!(
        !out.contains("scratch.log"),
        "the tree is hidden when zoomed\n{out}"
    );
    assert!(
        !out.contains("added.rs"),
        "no tree rows are drawn when zoomed\n{out}"
    );
}

#[test]
fn geometry_is_content_only_when_zoomed() {
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    let mut st = sample_state();
    st.zoomed = true;
    // Even at a wide width (which normally shows both columns), zoom draws content only.
    let g = geometry(
        Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 24,
        },
        &st,
    );
    assert!(g.tree_inner.is_none(), "no tree interior when zoomed");
    assert!(
        g.content_inner.is_some(),
        "the content pane fills the frame when zoomed"
    );
    assert!(g.divider_x.is_none(), "no divider when the tree is hidden");
}

#[test]
fn geometry_is_single_column_when_narrow() {
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    let mut st = sample_state();
    st.focus = Focus::Tree;
    // Below the 80-col narrow threshold only the focused column is drawn (AC-21).
    let g = geometry(
        Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 24,
        },
        &st,
    );
    assert!(g.tree_inner.is_some(), "narrow + tree focus draws the tree");
    assert!(
        g.content_inner.is_none(),
        "the content column is not drawn when narrow"
    );
    assert!(
        g.divider_x.is_none(),
        "no divider in a single-column layout"
    );
}

#[test]
fn update_banner_renders_as_a_bottom_status_line() {
    // AC-U1: when behind, the bottom row carries the version + the install command.
    let mut state = sample_state();
    state.update_banner = Some(
        "↑ v1.1.0 available · herdr plugin install smarzban/herdr-file-viewer · u to dismiss"
            .to_string(),
    );
    let out = render(&state, 100, 24);
    assert!(out.contains("v1.1.0 available"), "names the version\n{out}");
    assert!(
        out.contains("herdr plugin install smarzban/herdr-file-viewer"),
        "shows the install command\n{out}"
    );
    // The banner sits on the last interior row; the tree/content are still drawn above it.
    assert!(
        out.contains("fn main()"),
        "content still shows above the banner\n{out}"
    );
    let last = out.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("");
    assert!(
        last.contains("u to dismiss"),
        "banner is the bottom line: {last:?}"
    );
}

#[test]
fn no_banner_reserves_no_row_and_shows_nothing() {
    // AC-U2: up-to-date users see no banner text at all.
    let out = render(&sample_state(), 100, 24); // update_banner: None
    assert!(
        !out.contains("available"),
        "no update text when up-to-date\n{out}"
    );
}

/// A picker overlay over the two-column layout: the CURRENT worktree (current marker, no agent),
/// a second branch hosting a `working` agent (cursor row + agent badge), and a detached-HEAD
/// worktree (no agent). Exercises the current marker, the agent badge, and the detached marker
/// together — and proves the current marker (row 0) is distinct from the REVERSED cursor (row 1).
fn picker_state() -> ViewState {
    let mut state = sample_state();
    state.picker = Some(PickerView {
        rows: vec![
            PickerRowView {
                path: "/work/main".to_string(),
                branch: Some("main".to_string()),
                detached: false,
                is_current: true,
                agent: None,
            },
            PickerRowView {
                path: "/work/feature".to_string(),
                branch: Some("feature-x".to_string()),
                detached: false,
                is_current: false,
                agent: Some("working".to_string()),
            },
            PickerRowView {
                path: "/work/detached".to_string(),
                branch: None,
                detached: true,
                is_current: false,
                agent: None,
            },
        ],
        cursor: 1, // the feature-x row is highlighted (non-zero) — NOT the current row (row 0)
        hscroll: 0,
    });
    state
}

#[test]
fn picker_overlay_renders_rows_over_the_two_columns() {
    // AC-1/AC-5: the picker overlay is drawn on top of the columns.
    let out = render(&picker_state(), 100, 24);
    assert!(
        out.contains("Switch worktree"),
        "the picker title is shown\n{out}"
    );
    // Each worktree row's path + branch label appears.
    assert!(out.contains("main"), "the main row is shown\n{out}");
    assert!(
        out.contains("feature-x"),
        "the feature branch label is shown\n{out}"
    );
    // The two columns are still drawn underneath (the overlay is partial, centered). The tree's
    // titled top border ("┌r…") proves its column drew beneath the modal (the title is
    // the root basename, not the old static "Files").
    assert!(
        out.contains("┌r"),
        "the tree column is drawn under the overlay\n{out}"
    );
}

#[test]
fn picker_detached_row_shows_a_marker_not_an_empty_branch() {
    // Gate L-1 / AC-2: a detached-HEAD worktree shows a detached marker, never an empty branch.
    let out = render(&picker_state(), 100, 24);
    assert!(
        out.contains("detached"),
        "the detached worktree shows a detached marker\n{out}"
    );
    // Defense: no row renders an empty `[]` branch label.
    assert!(
        !out.contains("[]"),
        "no row renders an empty branch label\n{out}"
    );
}

#[test]
fn picker_highlights_the_cursor_row() {
    // AC-5: the cursor row is highlighted (REVERSED) — its background differs from a
    // non-cursor row. Find the `feature` path (cursor row, idx 1) and a non-cursor `main` row.
    use ratatui::style::Modifier;
    let buf = render_buffer(&picker_state(), 100, 24);
    let row_modifier = |needle: &str| -> Modifier {
        let (w, h) = (buf.area().width, buf.area().height);
        for y in 0..h {
            for x in 0..w {
                let matches = needle.chars().enumerate().all(|(i, ch)| {
                    let cx = x + i as u16;
                    cx < w
                        && buf
                            .cell((cx, y))
                            .is_some_and(|c| c.symbol() == ch.to_string())
                });
                if matches {
                    return buf.cell((x, y)).unwrap().modifier;
                }
            }
        }
        panic!("{needle:?} not found in buffer");
    };
    assert!(
        row_modifier("feature-x").contains(Modifier::REVERSED),
        "the cursor row (feature-x) is REVERSED"
    );
    assert!(
        !row_modifier("main").contains(Modifier::REVERSED),
        "a non-cursor row (main) is not REVERSED"
    );
}

#[test]
fn picker_marks_the_current_worktree_distinctly_from_the_cursor() {
    // AC-18: the current worktree carries a "current" marker that is visually distinct from the
    // selection cursor. In the fixture, row 0 (/work/main) is the current root but NOT the cursor
    // (cursor is row 1, the agent row). So: the current marker glyph (●) renders, and the current
    // row is NOT reversed while the cursor row IS — a row can be current without being selected.
    use ratatui::style::Modifier;
    let out = render(&picker_state(), 100, 24);
    assert!(
        out.contains('●'),
        "the current worktree shows a current marker (●)\n{out}"
    );

    let buf = render_buffer(&picker_state(), 100, 24);
    // Locate the row containing "main" (the current, non-cursor row) and "feature-x" (the cursor
    // row) and compare their modifiers: current ≠ reversed; cursor = reversed.
    let find = |needle: &str| -> Modifier {
        let (w, h) = (buf.area().width, buf.area().height);
        for y in 0..h {
            for x in 0..w {
                let matches = needle.chars().enumerate().all(|(i, ch)| {
                    let cx = x + i as u16;
                    cx < w
                        && buf
                            .cell((cx, y))
                            .is_some_and(|c| c.symbol() == ch.to_string())
                });
                if matches {
                    return buf.cell((x, y)).unwrap().modifier;
                }
            }
        }
        panic!("{needle:?} not found");
    };
    assert!(
        !find("main").contains(Modifier::REVERSED),
        "the current row (main) is the current root but NOT the cursor → not REVERSED"
    );
    assert!(
        find("feature-x").contains(Modifier::REVERSED),
        "the cursor row (feature-x) IS REVERSED — distinct from the current marker"
    );
}

#[test]
fn picker_shows_an_agent_status_badge_only_for_agent_rows() {
    // AC-19: a worktree whose workspace hosts a running agent shows that agent's status as a
    // badge; a worktree with no agent shows none. In the fixture only the feature-x row has an
    // agent (`working`), so the status text appears exactly once and the current/detached rows
    // carry no badge.
    let out = render(&picker_state(), 100, 24);
    assert!(
        out.contains("working"),
        "the agent row shows its status badge (working)\n{out}"
    );
    // The status string is colored — assert it lands in the buffer with a non-default fg so it
    // reads as a badge, not plain text.
    use ratatui::style::Color;
    let buf = render_buffer(&picker_state(), 100, 24);
    let (w, h) = (buf.area().width, buf.area().height);
    let mut badge_fg: Option<Color> = None;
    'outer: for y in 0..h {
        for x in 0..w {
            let matches = "working".chars().enumerate().all(|(i, ch)| {
                let cx = x + i as u16;
                cx < w
                    && buf
                        .cell((cx, y))
                        .is_some_and(|c| c.symbol() == ch.to_string())
            });
            if matches {
                badge_fg = Some(buf.cell((x, y)).unwrap().fg);
                break 'outer;
            }
        }
    }
    assert!(
        matches!(badge_fg, Some(c) if c != Color::Reset),
        "the agent badge is colored by status, got {badge_fg:?}"
    );
}

#[test]
fn picker_overlay_keeps_cursor_visible_with_many_rows() {
    // AC-5: on a repo with more worktrees than fit in the popup (herdr's multi-agent use case),
    // moving the cursor toward the end must scroll the row window so the highlighted row stays
    // visible. Pre-fix the overlay rendered all rows into the fixed popup with no scroll, so a
    // cursor near the end fell below the visible area and its row never reached the buffer.
    use ratatui::style::Modifier;

    let mut state = sample_state();
    // 40 worktrees — far more than fit in the ~12-row popup interior at 100x24.
    let rows: Vec<PickerRowView> = (0..40)
        .map(|i| PickerRowView {
            path: format!("/work/wt-{i:02}"),
            branch: Some(format!("branch-{i:02}")),
            detached: false,
            is_current: false,
            agent: None,
        })
        .collect();
    state.picker = Some(PickerView {
        rows,
        cursor: 37,
        hscroll: 0,
    }); // near the end

    let buf = render_buffer(&state, 100, 24);

    // The cursor row's distinctive path must be present in the buffer (the window scrolled to it).
    let cursor_needle = "wt-37";
    let mut found_at: Option<(u16, u16)> = None;
    let (w, h) = (buf.area().width, buf.area().height);
    for y in 0..h {
        for x in 0..w {
            let matches = cursor_needle.chars().enumerate().all(|(i, ch)| {
                let cx = x + i as u16;
                cx < w
                    && buf
                        .cell((cx, y))
                        .is_some_and(|c| c.symbol() == ch.to_string())
            });
            if matches {
                found_at = Some((x, y));
                break;
            }
        }
        if found_at.is_some() {
            break;
        }
    }
    let (cx, cy) = found_at.expect("the cursor row (wt-37) must be visible after scrolling");
    // And it carries the REVERSED highlight, so it reads as the selected row.
    assert!(
        buf.cell((cx, cy))
            .unwrap()
            .modifier
            .contains(Modifier::REVERSED),
        "the visible cursor row must be REVERSED-highlighted"
    );
}

/// Locate the picker overlay's border box in the buffer. The two-column layout also draws
/// `┌…┐` corners, so anchor on the overlay's title ("Switch worktree"): its `┌` corner is the
/// cell just left of the title, the `┐` corner is the first `┐` to the right of the title on
/// that row (gives the right edge), and the box's bottom edge is the first `┘` directly below
/// the right edge. Returns `(x0, y0, x1, y1)` inclusive. Panics if the picker is not found.
fn picker_border_box(buf: &ratatui::buffer::Buffer) -> (u16, u16, u16, u16) {
    let (w, h) = (buf.area().width, buf.area().height);
    let title = "Switch worktree";
    // Find the title row + its starting column.
    let mut anchor: Option<(u16, u16)> = None;
    'find: for y in 0..h {
        for x in 0..w {
            let here = title.chars().enumerate().all(|(i, ch)| {
                let cx = x + i as u16;
                cx < w
                    && buf
                        .cell((cx, y))
                        .is_some_and(|c| c.symbol() == ch.to_string())
            });
            if here {
                anchor = Some((x, y));
                break 'find;
            }
        }
    }
    let (tx, y0) = anchor.expect("picker title not found in buffer");
    let x0 = tx - 1; // the `┌` corner is just left of the title
    // The top-right `┐` is the first one to the right of the title on the same row.
    let mut x1 = None;
    for x in tx..w {
        if buf.cell((x, y0)).is_some_and(|c| c.symbol() == "┐") {
            x1 = Some(x);
            break;
        }
    }
    let x1 = x1.expect("picker top-right corner");
    // The bottom edge is the first `┘` at column x1 below the title row.
    let mut y1 = None;
    for y in (y0 + 1)..h {
        if buf.cell((x1, y)).is_some_and(|c| c.symbol() == "┘") {
            y1 = Some(y);
            break;
        }
    }
    let y1 = y1.expect("picker bottom-right corner");
    (x0, y0, x1, y1)
}

#[test]
fn picker_box_is_sized_to_its_content_not_the_whole_pane() {
    // Picker-layout §2: the overlay box is sized to its content (a few short worktree rows),
    // not a fixed 60% of the pane. With 3 short rows in an 80x24 frame, the box must be far
    // smaller than the frame — a tidy box, not a half-screen modal.
    let buf = render_buffer(&picker_state(), 80, 24);
    let (x0, y0, x1, y1) = picker_border_box(&buf);
    let box_w = x1 - x0 + 1;
    let box_h = y1 - y0 + 1;
    // 3 content rows + title + 2 borders ⇒ height ~6; the longest row
    // "  /work/feature [feature-x]  ● working" is ~38 cols + 2 borders ⇒ width ~40.
    assert!(
        box_w < 60,
        "the box is sized to its content, not ~60% of an 80-col pane (got width {box_w})"
    );
    assert!(
        box_h <= 8,
        "the box height tracks the row count, not ~60% of 24 rows (got height {box_h})"
    );
    assert!(
        box_w >= 30,
        "the box is wide enough for the rows (got {box_w})"
    );
}

#[test]
fn picker_box_clamps_to_a_narrow_frame_without_panicking() {
    // Picker-layout §2: rows wider than the frame must not overflow it — the box caps at the
    // pane (minus a small margin) and the draw must not panic at a small frame size.
    let mut state = picker_state();
    // A very long path — far wider than a 30-col frame.
    if let Some(p) = state.picker.as_mut() {
        p.rows[1].path = "/work/some/really/long/nested/worktree/path/that/overflows".to_string();
    }
    let buf = render_buffer(&state, 30, 10);
    let (x0, y0, x1, y1) = picker_border_box(&buf);
    assert!(x1 < 30, "the box right edge stays inside the 30-col frame");
    assert!(y1 < 10, "the box bottom edge stays inside the 10-row frame");
    assert!(x0 < x1 && y0 < y1, "the box is a valid non-empty rect");
    // No panic getting here is the core assertion (saturating Rect math at a small size).
}

#[test]
fn picker_rows_are_inset_from_the_left_border_by_one_padding_col() {
    // Picker-padding: the rows render into the block's PADDED inner, so a row's first non-blank
    // cell sits at least 2 columns right of the box's left `│` — one for the border, one for the
    // horizontal padding gutter. (Before the padding, the first content cell sat at x0 + 1, flush
    // against the border.)
    let buf = render_buffer(&picker_state(), 100, 24);
    let (x0, y0, _x1, y1) = picker_border_box(&buf);

    // Scan the interior rows (between the top and bottom border) for the first one carrying a
    // non-blank cell, and find that cell's column.
    let (w, _h) = (buf.area().width, buf.area().height);
    let mut first_content_col: Option<u16> = None;
    'rows: for y in (y0 + 1)..y1 {
        for x in (x0 + 1)..w {
            let cell = buf.cell((x, y));
            let sym = cell.map_or(" ", |c| c.symbol());
            // Stop at the right border; only look inside the box.
            if sym == "│" && x > x0 {
                break;
            }
            if sym != " " && sym != "│" {
                first_content_col = Some(x);
                break 'rows;
            }
        }
    }
    let col = first_content_col.expect("the picker has at least one row with content");
    assert!(
        col >= x0 + 2,
        "row content is inset from the left border by the padding gutter \
         (first content col {col} must be >= left border {x0} + 2)"
    );
}

#[test]
fn picker_rows_are_inset_from_the_top_border_by_one_padding_row() {
    // Picker-padding: uniform padding also insets the rows VERTICALLY, so the first row's text
    // sits at least 2 rows below the box's top border `─` line — one for the border row (which
    // carries the title), one for the top padding row (a blank line). (With horizontal-only
    // padding the first row sat at y0 + 1, flush against the top border.) Complements the
    // left-inset test, which checks the horizontal gutter.
    let buf = render_buffer(&picker_state(), 100, 24);
    let (x0, y0, x1, y1) = picker_border_box(&buf);

    // Scan interior rows top-to-bottom for the first one carrying a non-blank, non-border cell,
    // and record which row it lands on.
    let mut first_content_row: Option<u16> = None;
    'rows: for y in (y0 + 1)..y1 {
        for x in (x0 + 1)..x1 {
            let sym = buf.cell((x, y)).map_or(" ", |c| c.symbol());
            if sym != " " && sym != "│" {
                first_content_row = Some(y);
                break 'rows;
            }
        }
    }
    let row = first_content_row.expect("the picker has at least one row with content");
    assert!(
        row >= y0 + 2,
        "row content is inset from the top border by the padding row \
         (first content row {row} must be >= top border {y0} + 2)"
    );
}

#[test]
fn picker_border_is_blue() {
    // Picker-layout §1: the overlay border renders in the terminal's ANSI blue (matching herdr's
    // terminal-theme chrome). Assert the `┌` corner cell carries Color::Blue.
    use ratatui::style::Color;
    let buf = render_buffer(&picker_state(), 100, 24);
    let (x0, y0, _, _) = picker_border_box(&buf);
    let corner = buf.cell((x0, y0)).expect("picker corner cell");
    assert_eq!(
        corner.symbol(),
        "┌",
        "located the picker's top-left border corner"
    );
    assert_eq!(
        corner.fg,
        Color::Blue,
        "the picker border is ANSI blue (herdr terminal-theme chrome)"
    );
}

#[test]
fn picker_applies_horizontal_scroll_to_clip_long_rows() {
    // Picker-layout §3: when a row is wider than the box (long path on a narrow pane), a non-zero
    // hscroll shifts the visible text left so later columns become readable, and the leading
    // columns are clipped off.
    let mut state = picker_state();
    if let Some(p) = state.picker.as_mut() {
        p.rows = vec![PickerRowView {
            path: "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".to_string(),
            branch: None,
            detached: false,
            is_current: false,
            agent: None,
        }];
        p.cursor = 0;
        p.hscroll = 0;
    }
    // A narrow frame so the box caps and the row genuinely overflows.
    let out0 = render(&state, 24, 8);
    assert!(
        out0.contains("ABCDE"),
        "at hscroll 0 the row shows from its left edge\n{out0}"
    );

    if let Some(p) = state.picker.as_mut() {
        p.hscroll = 8;
    }
    let out_shifted = render(&state, 24, 8);
    assert!(
        !out_shifted.contains("ABCDE"),
        "with hscroll the leading columns are clipped off\n{out_shifted}"
    );
    assert!(
        out_shifted.contains("IJKLM"),
        "with hscroll later columns of the path become visible\n{out_shifted}"
    );
}

#[test]
fn picker_horizontal_scroll_clamps_past_the_end() {
    // Picker-layout §3: an hscroll far past the widest row is clamped at draw — no panic and no
    // over-scroll into blank space (the last columns stay visible, not scrolled off entirely).
    let mut state = picker_state();
    if let Some(p) = state.picker.as_mut() {
        p.rows = vec![PickerRowView {
            path: "ABCDEFGHIJKLMNOPQRSTUVWXYZ".to_string(),
            branch: None,
            detached: false,
            is_current: false,
            agent: None,
        }];
        p.cursor = 0;
        p.hscroll = 9999; // absurdly far past the end
    }
    let out = render(&state, 24, 8);
    // Clamped, not blanked: the tail of the path is still visible.
    assert!(
        out.contains("XYZ"),
        "an over-scroll clamps so the row's tail stays visible (no blank box)\n{out}"
    );
}

#[test]
fn picker_hscroll_is_a_noop_when_all_rows_fit_even_if_chrome_caps_the_box() {
    // Gate fix: the hscroll clamp must be against the widest ROW, not the (chrome-inflated)
    // desired inner width. On a narrow frame the box caps so the WIDE footer hint
    // ("↑↓ move · ←→ scroll · …", ~40 cols) drives `desired_inner_w`, while the SHORT rows fit the
    // capped interior. A non-zero hscroll must then be a no-op — every row still shows from its
    // left edge. (Before the fix, `desired_inner_w - inner.width > 0` let scroll-right clip the
    // rows off-screen for no reason.)
    let mut state = picker_state();
    if let Some(p) = state.picker.as_mut() {
        // A single SHORT row — far narrower than the footer chrome.
        p.rows = vec![PickerRowView {
            path: "/a".to_string(),
            branch: None,
            detached: false,
            is_current: false,
            agent: None,
        }];
        p.cursor = 0;
        p.hscroll = 0;
    }
    // A frame narrow enough that the box caps below the chrome width (~40), but wide enough that
    // the short row still fits the capped interior.
    let (w, h) = (20, 8);
    let out0 = render(&state, w, h);
    assert!(
        out0.contains("/a"),
        "at hscroll 0 the short row shows from its left edge\n{out0}"
    );

    // Scroll right hard. With the clamp against max_row_width (the fix), max_hscroll is 0 here, so
    // this is a no-op and the row stays fully visible. With the buggy desired_inner_w clamp the
    // leading `/a` would be clipped off-screen.
    if let Some(p) = state.picker.as_mut() {
        p.hscroll = 30;
    }
    let out_scrolled = render(&state, w, h);
    assert!(
        out_scrolled.contains("/a"),
        "scrolling right is a no-op while every row fits — the row must not be clipped\n{out_scrolled}"
    );
    assert_eq!(
        out0, out_scrolled,
        "hscroll has no effect when all rows fit the capped interior"
    );
}

#[test]
fn picker_shows_an_esc_close_chip_on_the_top_border() {
    // Picker-hints §1: herdr-style chrome — an `esc close` chip on the TOP border, right side.
    // It is a Block title, not an inner row, so it lands on the title row (the box's top edge),
    // to the right of the existing "Switch worktree" title.
    let buf = render_buffer(&picker_state(), 100, 24);
    let (x0, y0, x1, _y1) = picker_border_box(&buf);

    // Read the whole top border row text inside the box and confirm the chip is present, to the
    // right of the left-aligned title.
    let top = border_row_text(&buf, x0, x1, y0);
    assert!(
        top.contains("esc close"),
        "the top border shows an `esc close` chip\n{top}"
    );
    let title_col = first_col_of(&buf, x0, x1, y0, "Switch worktree").expect("title on top border");
    let chip_col = first_col_of(&buf, x0, x1, y0, "esc close").expect("chip on top border");
    assert!(
        chip_col > title_col,
        "the `esc close` chip is to the RIGHT of the title (top={top:?})"
    );

    // The chip is no longer dimmed: its fg matches the worktree PATH text (the default/terminal
    // foreground, `Color::Reset`), not DarkGray. Compare it to a path cell's fg directly.
    let path_fg = path_text_fg(&buf);
    let chip_fg = buf.cell((chip_col, y0)).unwrap().fg;
    assert_eq!(
        chip_fg, path_fg,
        "the `esc close` chip uses the same fg as the worktree path text (default), not DarkGray"
    );
    assert_eq!(
        chip_fg,
        ratatui::style::Color::Reset,
        "the chip uses the default/terminal foreground (Color::Reset)"
    );
}

/// The foreground color of a worktree PATH cell in the picker rows — the baseline the chrome
/// hints are expected to match. Anchors on the `/work/main` path (the current, non-cursor row),
/// reading the fg of its first path glyph. The path spans carry no explicit fg, so this is the
/// default `Color::Reset`.
fn path_text_fg(buf: &ratatui::buffer::Buffer) -> ratatui::style::Color {
    let needle = "/work/main";
    let (w, h) = (buf.area().width, buf.area().height);
    for y in 0..h {
        for x in 0..w {
            let matches = needle.chars().enumerate().all(|(i, ch)| {
                let cx = x + i as u16;
                cx < w
                    && buf
                        .cell((cx, y))
                        .is_some_and(|c| c.symbol() == ch.to_string())
            });
            if matches {
                return buf.cell((x, y)).unwrap().fg;
            }
        }
    }
    panic!("{needle:?} path text not found in buffer");
}

/// The text of a border row between columns `x0..=x1` (one char per cell, multi-width cells
/// contribute their leading char) — for asserting which titles landed on the border.
fn border_row_text(buf: &ratatui::buffer::Buffer, x0: u16, x1: u16, y: u16) -> String {
    (x0..=x1)
        .map(|x| {
            buf.cell((x, y))
                .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
        })
        .collect()
}

/// The buffer column where `needle` first begins on row `y` within `x0..=x1`, matching one char
/// per cell. Returns the absolute column (not a byte offset) so an fg lookup is exact even when
/// the row contains multi-byte glyphs (the box-drawing border, `·`, arrows).
fn first_col_of(
    buf: &ratatui::buffer::Buffer,
    x0: u16,
    x1: u16,
    y: u16,
    needle: &str,
) -> Option<u16> {
    let chars: Vec<char> = needle.chars().collect();
    for start in x0..=x1 {
        let fits = chars.iter().enumerate().all(|(i, ch)| {
            let cx = start + i as u16;
            cx <= x1
                && buf
                    .cell((cx, y))
                    .is_some_and(|c| c.symbol() == ch.to_string())
        });
        if fits {
            return Some(start);
        }
    }
    None
}

#[test]
fn picker_shows_a_key_hint_footer_on_the_bottom_border() {
    // Picker-hints §2: a dim `·`-separated footer of the picker's real keys on the BOTTOM border.
    let buf = render_buffer(&picker_state(), 100, 24);
    let (x0, _y0, x1, y1) = picker_border_box(&buf);

    let bottom = border_row_text(&buf, x0, x1, y1);
    // The footer names move / scroll / switch / cancel keys with herdr's ` · ` separator.
    assert!(
        bottom.contains("move") && bottom.contains("scroll"),
        "the bottom border footer names move + scroll\n{bottom}"
    );
    assert!(
        bottom.contains("switch") && bottom.contains("cancel"),
        "the bottom border footer names switch + cancel\n{bottom}"
    );
    assert!(
        bottom.contains('·'),
        "the footer uses herdr's ` · ` separator\n{bottom}"
    );

    // The footer is no longer dimmed: its fg matches the worktree PATH text (the default/terminal
    // foreground, `Color::Reset`), not DarkGray. Check the fg at the first hint glyph (`move`).
    let move_col = first_col_of(&buf, x0, x1, y1, "move").expect("footer names `move`");
    let path_fg = path_text_fg(&buf);
    let footer_fg = buf.cell((move_col, y1)).unwrap().fg;
    assert_eq!(
        footer_fg, path_fg,
        "the footer hint uses the same fg as the worktree path text (default), not DarkGray"
    );
    assert_eq!(
        footer_fg,
        ratatui::style::Color::Reset,
        "the footer uses the default/terminal foreground (Color::Reset)"
    );
}

#[test]
fn picker_box_is_wide_enough_for_the_chrome_when_rows_are_short() {
    // Picker-hints §3: size-to-content must include the chrome — even with very short rows, the
    // box must be wide enough that neither the title+`esc close` nor the footer hint is clipped.
    let mut state = picker_state();
    // Rows much shorter than the chrome (the footer is the widest element here).
    if let Some(p) = state.picker.as_mut() {
        p.rows = vec![
            PickerRowView {
                path: "a".to_string(),
                branch: None,
                detached: false,
                is_current: true,
                agent: None,
            },
            PickerRowView {
                path: "b".to_string(),
                branch: None,
                detached: false,
                is_current: false,
                agent: None,
            },
        ];
        p.cursor = 0;
    }
    let buf = render_buffer(&state, 100, 24);
    let (x0, y0, x1, y1) = picker_border_box(&buf);

    let top = border_row_text(&buf, x0, x1, y0);
    let bottom = border_row_text(&buf, x0, x1, y1);

    // The full title + chip both appear on the top border (not clipped) despite tiny rows.
    assert!(
        top.contains("Switch worktree") && top.contains("esc close"),
        "short rows still leave room for the title + `esc close` chip\n{top}"
    );
    // The full footer appears on the bottom border (not clipped) despite tiny rows.
    assert!(
        bottom.contains("move")
            && bottom.contains("scroll")
            && bottom.contains("switch")
            && bottom.contains("cancel"),
        "short rows still leave room for the full key-hint footer\n{bottom}"
    );
}

#[test]
fn picker_chrome_renders_without_panic_on_a_small_frame() {
    // Picker-hints §3: at a frame too narrow for the full chrome (24x8 is below the footer's
    // natural ~43-col width) the box caps at the pane and the hints simply truncate — the draw
    // must NOT panic (saturating layout math). The draw itself is the assertion: `render_buffer`
    // would unwrap-panic on a `draw` error, so reaching the size checks proves it stayed sane.
    let buf = render_buffer(&picker_state(), 24, 8);
    // The clamped box never exceeds the frame (right/bottom edges stay inside).
    assert!(buf.area().width <= 24 && buf.area().height <= 8);
    // A truncated footer is acceptable; the box is still bordered (a `┐` corner exists somewhere).
    let (w, h) = (buf.area().width, buf.area().height);
    let has_corner =
        (0..h).any(|y| (0..w).any(|x| buf.cell((x, y)).is_some_and(|c| c.symbol() == "┐")));
    assert!(
        has_corner,
        "the picker still draws a bordered box at a small frame"
    );
}

#[test]
fn picker_overlay_snapshot() {
    insta::assert_snapshot!("presenter_picker", render(&picker_state(), 100, 24));
}

#[test]
fn banner_carves_exactly_one_row_off_the_columns() {
    // AC-U2: showing the banner shrinks the content interior by exactly one row vs. no banner,
    // so mouse hit-testing (which reads the same geometry) stays correct.
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 24,
    };

    let plain = sample_state();
    let mut withbanner = sample_state();
    withbanner.update_banner = Some("↑ v1.1.0 available · u to dismiss".to_string());

    let h_plain = geometry(area, &plain).content_inner.unwrap().height;
    let h_banner = geometry(area, &withbanner).content_inner.unwrap().height;
    assert_eq!(
        h_plain - h_banner,
        1,
        "the banner takes exactly one row from the body"
    );
}

// ── Finder overlay tests (AC-1, AC-2, AC-5) ─────────────────────────────

/// A `ViewState` with the finder open and an EMPTY query (no matches yet).
fn finder_state_empty_query() -> ViewState {
    let mut state = sample_state();
    state.finder = Some(FinderView {
        query: String::new(),
        matches: vec![],
        cursor: 0,
        hscroll: 0,
    });
    state
}

/// A `ViewState` with the finder open, a query typed, and 3 matched paths.
fn finder_state_with_matches() -> ViewState {
    let mut state = sample_state();
    state.finder = Some(FinderView {
        query: "main".to_string(),
        matches: vec![
            "src/main.rs".to_string(),
            "src/inner/main_helper.rs".to_string(),
            "README.md".to_string(),
        ],
        cursor: 1, // the second row is highlighted
        hscroll: 0,
    });
    state
}

#[test]
fn finder_overlay_empty_query_shows_title_and_placeholder_no_rows() {
    // AC-2: when the query is empty the finder shows the query-input line with a placeholder
    // and NO match rows — not a blank box, and not the full candidate list.
    let out = render(&finder_state_empty_query(), 100, 24);
    assert!(
        out.contains("Go to file"),
        "the finder title is shown\n{out}"
    );
    // The placeholder must appear (AC-2).
    assert!(
        out.contains("type to find"),
        "the placeholder is shown when the query is empty\n{out}"
    );
    // No file rows (matches is empty so no paths appear yet).
    // The tree still renders under the overlay (AC-1 — partial overlay). Its titled top border
    // ("┌r…") proves the column drew beneath the modal (root basename, not "Files").
    assert!(
        out.contains("┌r"),
        "the tree column is drawn under the overlay (AC-1)\n{out}"
    );
}

#[test]
fn finder_overlay_with_matches_shows_rows_and_highlights_cursor() {
    // AC-5: each match row is a root-relative path; the cursor row is highlighted (REVERSED).
    // AC-1: the overlay is drawn ON TOP of the two-column layout.
    use ratatui::style::Modifier;

    let buf = render_buffer(&finder_state_with_matches(), 100, 24);
    let (w, h) = (buf.area().width, buf.area().height);

    // All three matched paths appear in the buffer.
    let out = format!("{}", {
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        t.draw(|f| {
            draw(f, &finder_state_with_matches());
        })
        .unwrap();
        t.backend().clone()
    });
    assert!(
        out.contains("src/main.rs"),
        "first match row is shown\n{out}"
    );
    assert!(
        out.contains("main_helper"),
        "second match row is shown\n{out}"
    );
    assert!(out.contains("README.md"), "third match row is shown\n{out}");
    // The two-column layout is still underneath (AC-1). Its titled top border ("┌r…") proves the
    // tree column drew beneath the modal (root basename, not the old static "Files").
    assert!(
        out.contains("┌r"),
        "the tree column is drawn under the overlay (AC-1)\n{out}"
    );

    // The cursor row (index 1 = "src/inner/main_helper.rs") is REVERSED.
    let needle = "main_helper";
    let mut cursor_cell: Option<(u16, u16)> = None;
    'outer: for y in 0..h {
        for x in 0..w {
            let matches = needle.chars().enumerate().all(|(i, ch)| {
                let cx = x + i as u16;
                cx < w
                    && buf
                        .cell((cx, y))
                        .is_some_and(|c| c.symbol() == ch.to_string())
            });
            if matches {
                cursor_cell = Some((x, y));
                break 'outer;
            }
        }
    }
    let (cx, cy) = cursor_cell.expect("cursor row (main_helper) is in the buffer");
    assert!(
        buf.cell((cx, cy))
            .unwrap()
            .modifier
            .contains(Modifier::REVERSED),
        "the cursor row (main_helper) is REVERSED-highlighted (AC-5)"
    );
}

/// A `ViewState` with the finder open, many matches that overflow the visible rows area, and
/// the cursor set near the end — exercises the scrollbar path and the viewport window.
fn finder_state_overflow() -> ViewState {
    let mut state = sample_state();
    // 30 matches in a height-16 terminal: the popup interior is small, so the rows area height is
    // well below 30 and the scrollbar must be shown. Cursor at index 25 — near the end.
    let matches: Vec<String> = (0..30).map(|i| format!("src/file_{i:02}.rs")).collect();
    state.finder = Some(FinderView {
        query: "file".to_string(),
        matches,
        cursor: 25,
        hscroll: 0,
    });
    state
}

#[test]
fn finder_overlay_overflow_scrolls_viewport_and_border_stays_intact() {
    // Finding 1 + Finding 2 guard: with more matches than the popup rows area is tall —
    // 30 matches in a height-16 terminal — the presenter must:
    //   (a) draw only the viewport window of rows (not all 30),
    //   (b) keep the REVERSED highlight on the cursor row after scrolling,
    //   (c) leave the right border column as `│` (NOT overwritten by the scrollbar thumb).
    use ratatui::style::Modifier;

    let state = finder_state_overflow();
    let (w, h) = (100u16, 16u16);
    let buf = render_buffer(&state, w, h);

    // (a) viewport clipping: only rows near the cursor (25) should be in the buffer.
    // "src/file_00.rs" is near the top; with cursor=25 the window has scrolled past it.
    let out = render(&state, w, h);
    assert!(
        !out.contains("file_00"),
        "early row (file_00) must be scrolled off when cursor is near the end\n{out}"
    );
    // The cursor row's distinctive name must be visible.
    assert!(
        out.contains("file_25"),
        "the cursor row (file_25) must be visible after scrolling\n{out}"
    );

    // (b) REVERSED modifier on the cursor row cell.
    let needle = "file_25";
    let mut cursor_cell: Option<(u16, u16)> = None;
    'outer: for y in 0..h {
        for x in 0..w {
            let matches_here = needle.chars().enumerate().all(|(i, ch)| {
                let cx = x + i as u16;
                cx < w
                    && buf
                        .cell((cx, y))
                        .is_some_and(|c| c.symbol() == ch.to_string())
            });
            if matches_here {
                cursor_cell = Some((x, y));
                break 'outer;
            }
        }
    }
    let (cx, cy) = cursor_cell.expect("cursor row (file_25) must be visible in the buffer");
    assert!(
        buf.cell((cx, cy))
            .unwrap()
            .modifier
            .contains(Modifier::REVERSED),
        "the cursor row (file_25) must carry the REVERSED highlight after scrolling"
    );

    // (c) right border of the popup is `│`, not corrupted by the scrollbar.
    // Locate the finder popup's right border column by finding "Go to file" on the title row and
    // scanning right for the `┐` corner, then assert every interior cell in that column is `│`.
    let title = "Go to file";
    let mut title_anchor: Option<(u16, u16)> = None;
    'find_title: for y in 0..h {
        for x in 0..w {
            let here = title.chars().enumerate().all(|(i, ch)| {
                let cx = x + i as u16;
                cx < w
                    && buf
                        .cell((cx, y))
                        .is_some_and(|c| c.symbol() == ch.to_string())
            });
            if here {
                title_anchor = Some((x, y));
                break 'find_title;
            }
        }
    }
    let (tx, ty) = title_anchor.expect("finder title 'Go to file' must be in the buffer");
    // Scan right from the title to find the `┐` top-right corner.
    let mut x1: Option<u16> = None;
    for x in tx..w {
        if buf.cell((x, ty)).is_some_and(|c| c.symbol() == "┐") {
            x1 = Some(x);
            break;
        }
    }
    let x1 = x1.expect("popup top-right corner `┐` must be to the right of the title");
    // Find the bottom-right `┘` corner.
    let mut y1: Option<u16> = None;
    for y in (ty + 1)..h {
        if buf.cell((x1, y)).is_some_and(|c| c.symbol() == "┘") {
            y1 = Some(y);
            break;
        }
    }
    let y1 = y1.expect("popup bottom-right corner `┘` must exist");
    // Every cell on the right border column between the top and bottom corners must be `│`.
    for y in (ty + 1)..y1 {
        let cell = buf.cell((x1, y)).expect("right border cell");
        assert_eq!(
            cell.symbol(),
            "│",
            "right border col {x1} row {y} must be `│` — the scrollbar must NOT overwrite the border"
        );
    }
}

#[test]
fn finder_overlay_nonempty_query_zero_matches_shows_prompt_not_placeholder() {
    // Finding 3: a non-empty query with NO matches must show the `> query` prompt line (not the
    // placeholder) and must NOT draw any match rows.
    let mut state = sample_state();
    state.finder = Some(FinderView {
        query: "zzzzz".to_string(),
        matches: vec![],
        cursor: 0,
        hscroll: 0,
    });
    let out = render(&state, 100, 24);

    // The prompt prefix + query text must appear.
    assert!(
        out.contains("> zzzzz"),
        "the prompt line must show the typed query\n{out}"
    );
    // The placeholder must NOT appear (it is only for an empty query).
    assert!(
        !out.contains("type to find"),
        "the placeholder must not appear when a query is typed\n{out}"
    );
    // No match rows (the matches list is empty, so nothing else should appear).
    // The overlay is still drawn (title present).
    assert!(
        out.contains("Go to file"),
        "the finder title is still drawn\n{out}"
    );
}

#[test]
fn finder_overlay_empty_query_snapshot() {
    insta::assert_snapshot!(
        "presenter_finder_empty_query",
        render(&finder_state_empty_query(), 100, 24)
    );
}

#[test]
fn finder_overlay_with_matches_snapshot() {
    insta::assert_snapshot!(
        "presenter_finder_with_matches",
        render(&finder_state_with_matches(), 100, 24)
    );
}

#[test]
fn finder_geometry_agrees_with_draw_for_mouse_click_hit_testing() {
    // Regression: the controller maps a mouse click in the finder overlay to a match index via
    //   `finder_scroll + (click_y - finder_rows.y)`.
    // This test renders the overlay and scans the ACTUAL buffer to find which screen row each
    // match path is drawn on, then asserts that geometry().finder_rows / finder_scroll produce
    // the same row for that match index.  A mismatch here means geometry and draw have drifted
    // and a click would resolve to the wrong file.
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;

    let view = finder_state_with_matches();
    // Matches: index 0 = "src/main.rs", index 1 = "src/inner/main_helper.rs", index 2 = "README.md"
    let (w, h) = (100u16, 24u16);
    let area = Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };

    let buf = render_buffer(&view, w, h);

    // Helper: scan the full buffer for the first row where `needle` appears, return that row's y.
    let find_draw_row = |needle: &str| -> u16 {
        for y in 0..h {
            for x in 0..w {
                let hit = needle.chars().enumerate().all(|(i, ch)| {
                    let cx = x + i as u16;
                    cx < w
                        && buf
                            .cell((cx, y))
                            .is_some_and(|c| c.symbol() == ch.to_string())
                });
                if hit {
                    return y;
                }
            }
        }
        panic!("{needle:?} not found in buffer — draw did not render it");
    };

    let draw_row_0 = find_draw_row("src/main.rs");
    let draw_row_1 = find_draw_row("main_helper.rs");

    let g = geometry(area, &view);
    let rows_rect = g
        .finder_rows
        .expect("geometry().finder_rows must be Some when the finder has matches");
    let scroll = g.finder_scroll as usize;

    // The drawn row must lie inside finder_rows.
    assert!(
        draw_row_1 >= rows_rect.y && draw_row_1 < rows_rect.y + rows_rect.height,
        "draw put 'main_helper.rs' at screen row {draw_row_1}, \
         but geometry().finder_rows is {rows_rect:?} — they disagree"
    );
    assert!(
        draw_row_0 >= rows_rect.y && draw_row_0 < rows_rect.y + rows_rect.height,
        "draw put 'src/main.rs' at screen row {draw_row_0}, \
         but geometry().finder_rows is {rows_rect:?} — they disagree"
    );

    // Apply the controller's click-to-index formula and verify the result.
    let index_1 = scroll + (draw_row_1 - rows_rect.y) as usize;
    assert_eq!(
        index_1, 1,
        "click on the drawn 'main_helper.rs' row (screen y={draw_row_1}) should map to match \
         index 1 via scroll({scroll}) + (draw_y - rows_rect.y({})): got {index_1}",
        rows_rect.y
    );

    let index_0 = scroll + (draw_row_0 - rows_rect.y) as usize;
    assert_eq!(
        index_0, 0,
        "click on the drawn 'src/main.rs' row (screen y={draw_row_0}) should map to match \
         index 0 via scroll({scroll}) + (draw_y - rows_rect.y({})): got {index_0}",
        rows_rect.y
    );

    // Sanity: a Position inside finder_rows at the found row is within the rect.
    let probe_x = rows_rect.x + rows_rect.width / 2;
    assert!(
        rows_rect.contains(ratatui::layout::Position {
            x: probe_x,
            y: draw_row_1
        }),
        "a position inside finder_rows at the drawn row must be contained in the rect"
    );
}

#[test]
fn finder_geometry_exposes_the_scrollbar_track_only_when_rows_overflow() {
    // The finder's vertical scrollbar track is fed back via geometry().finder_vbar so the controller
    // can map a press/drag on it to a selection (click-drag scroll). It is Some exactly when the
    // match rows overflow the visible height, and is the gutter column right of the rows
    // (x == rows.x + rows.width) — the SAME rect draw_finder_overlay renders the scrollbar into.
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 16,
    };

    // Overflow: 30 matches in a height-16 terminal → the bar is present and aligned to the rows.
    let overflow = finder_state_overflow();
    let g = geometry(area, &overflow);
    let rows = g.finder_rows.expect("rows present with matches");
    let vbar = g
        .finder_vbar
        .expect("geometry().finder_vbar must be Some when the rows overflow");
    assert_eq!(
        vbar.x,
        rows.x + rows.width,
        "the bar sits in the gutter column right of the rows"
    );
    assert_eq!(vbar.y, rows.y, "the bar aligns with the rows top");
    assert_eq!(vbar.height, rows.height, "the bar spans the rows height");
    assert_eq!(vbar.width, 1, "the bar is one column wide");

    // No overflow: a few matches that fit → no bar.
    let small = finder_state_with_matches();
    assert!(
        geometry(area, &small).finder_vbar.is_none(),
        "geometry().finder_vbar must be None when every match row fits"
    );
}

// ── bottom prompt line (AC-1) + no-file-notice path (AC-7) ──────────────

#[test]
fn an_open_go_to_line_prompt_renders_a_bottom_line() {
    // AC-1: when a prompt is open (`ViewState.prompt = Some("Go to line: 42")`), the Presenter draws
    // a one-row line at the very bottom of the frame showing the prompt string.
    let mut st = sample_state();
    st.update_banner = None; // no banner — prompt is the sole bottom row
    st.prompt = Some("Go to line: 42".into());
    // Render at a known size; the bottom row (row h-1) must contain the prompt label + number.
    let (w, h) = (100u16, 24u16);
    let out = render(&st, w, h);
    let last_row = out
        .lines()
        .last()
        .expect("at least one row in the rendered output");
    assert!(
        last_row.contains("Go to line: 42"),
        "the last row must contain 'Go to line: 42' when a prompt is open\n{out}"
    );
    // The two-column layout is still drawn above the prompt row.
    assert!(
        out.contains("fn main()"),
        "content still shows above the prompt line\n{out}"
    );
}

#[test]
fn no_prompt_open_leaves_layout_unchanged() {
    // AC-1 (negative): with `prompt: None` the layout is byte-identical to the pre-prompt baseline —
    // body_footer_prompt falls through to body_and_footer with no prompt row reserved.
    let st = sample_state(); // prompt: None (set by the sample_state helper)
    let out_no_prompt = render(&st, 100, 24);
    // The bottom row must NOT contain the go-to-line prompt label (no phantom prompt).
    let last_row = out_no_prompt.lines().last().expect("at least one row");
    assert!(
        !last_row.contains("Go to line:"),
        "no prompt row rendered when prompt is None\n{out_no_prompt}"
    );
}

#[test]
fn go_to_line_no_file_notice_renders_for_ac7() {
    // AC-7 (revised): the ONLY go-to-line notice is the no-file case — `:` with a directory / nothing
    // selected emits "Go to line: select a file first" and opens no prompt. (A transformed view no
    // longer shows an "unavailable" notice — confirming there auto-switches and jumps.) This verifies
    // the Presenter surfaces that notice via the existing notices channel, with no new code path.
    let mut st = sample_state();
    st.notices = vec!["Go to line: select a file first".into()];
    let out = render(&st, 100, 24);
    assert!(
        out.contains("Go to line: select a file first"),
        "the no-file go-to-line notice (AC-7) must appear in the rendered frame\n{out}"
    );
}

// ── ContentSearch overlay — AC-8, AC-9, AC-11 ─────────────────────────

/// A `ViewState` with a known three-line content and two search matches:
/// - match 0 on line 0 (bytes 3..7 = "main") — non-current, gets `HIGHLIGHT`
/// - match 1 on line 2 (bytes 0..1 = "}") — CURRENT, gets `CURRENT_HIGHLIGHT`
///
/// cursor = 1 → match 1 is the current one.
fn search_state() -> ViewState {
    use herdr_file_viewer::render::to_text;
    let mut st = sample_state();
    st.notices = vec![];
    // Content: exactly three lines whose text is predictable byte-by-byte.
    // "fn main() {\n    println!(\"hello\");\n}\n"  (from sample_state, but override)
    // We use simple ASCII-only content so byte offsets are trivial.
    st.content = to_text("fn main() {\n    println!;\n}\n");
    st.content_rows = 3;
    // match 0 = "main" on line 0, bytes 3..7
    // match 1 = "}" on line 2, bytes 0..1  → current (current = 1)
    st.search = Some(ContentSearch {
        matches: vec![
            Match {
                line: 0,
                start: 3,
                end: 7,
            }, // "main"
            Match {
                line: 2,
                start: 0,
                end: 1,
            }, // "}"
        ],
        current: 1,
    });
    st
}

#[test]
fn search_highlight_colors_match_cells_with_highlight_style() {
    // AC-9: every non-current match is highlighted with HIGHLIGHT (black on cyan).
    // AC-11: the current match is highlighted with CURRENT_HIGHLIGHT (REVERSED+BOLD, a
    // theme-relative style distinguishable with color stripped), distinct from the non-current ones.
    use herdr_file_viewer::highlight::{CURRENT_HIGHLIGHT, HIGHLIGHT};
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;

    let st = search_state();
    let (w, h) = (100u16, 24u16);
    let area = Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };
    let buf = render_buffer(&st, w, h);

    // Find the content text rect via geometry so we only scan there.
    let content_inner = geometry(area, &st)
        .content_inner
        .expect("content inner must be present");

    // Helper: find the first cell matching `needle` inside `content_inner`, return its (x, y).
    let find_in_content = |needle: &str| -> Option<(u16, u16)> {
        for y in content_inner.y..(content_inner.y + content_inner.height) {
            for x in content_inner.x..(content_inner.x + content_inner.width) {
                let hit = needle.chars().enumerate().all(|(i, ch)| {
                    let cx = x + i as u16;
                    cx < content_inner.x + content_inner.width
                        && buf
                            .cell((cx, y))
                            .is_some_and(|c| c.symbol() == ch.to_string())
                });
                if hit {
                    return Some((x, y));
                }
            }
        }
        None
    };

    // AC-9: "main" (match 0, non-current) must be in the content area and carry HIGHLIGHT bg.
    let (mx, my) = find_in_content("main").expect("'main' must appear in the content area");
    let main_bg = buf.cell((mx, my)).unwrap().bg;
    assert_eq!(
        main_bg,
        HIGHLIGHT.bg.unwrap(),
        "AC-9: the non-current match 'main' must have the HIGHLIGHT background (cyan), got {main_bg:?}"
    );
    let main_fg = buf.cell((mx, my)).unwrap().fg;
    assert_eq!(
        main_fg,
        HIGHLIGHT.fg.unwrap(),
        "AC-9: the non-current match 'main' must have the HIGHLIGHT foreground (black), got {main_fg:?}"
    );

    // AC-11: "}" (match 1, current) must carry CURRENT_HIGHLIGHT — REVERSED+BOLD (a
    // theme-relative style, not a hardcoded color), so it is distinguishable with color stripped.
    let (cx2, cy2) = find_in_content("}").expect("'}' must appear in the content area");
    let cur_modifier = buf.cell((cx2, cy2)).unwrap().modifier;
    assert!(
        cur_modifier.contains(Modifier::REVERSED),
        "AC-11: the current match '}}' must be REVERSED (theme-relative), got modifier {cur_modifier:?}"
    );
    assert!(
        cur_modifier.contains(Modifier::BOLD),
        "AC-11: the current match '}}' must be BOLD (weight cue), got modifier {cur_modifier:?}"
    );

    // AC-11 distinctness: the two highlight styles differ — HIGHLIGHT is color-only (cyan bg),
    // CURRENT_HIGHLIGHT is theme-relative (REVERSED+BOLD). They must not be equal.
    assert_ne!(
        HIGHLIGHT, CURRENT_HIGHLIGHT,
        "AC-11: HIGHLIGHT and CURRENT_HIGHLIGHT must differ"
    );
}

#[test]
fn search_none_keeps_draw_content_byte_identical() {
    // Zero-churn: `search: None` must produce output byte-identical to a state that never had a
    // search field. We compare two sample_state() renders — both have search: None — confirming
    // the new field has no side-effects on existing rendering paths.
    let st = sample_state(); // search: None
    let out1 = render(&st, 100, 24);
    let out2 = render(&st, 100, 24);
    assert_eq!(
        out1, out2,
        "deterministic: two identical states produce the same output"
    );
    // And the wide-layout snapshot still passes (it references search: None implicitly).
    insta::assert_snapshot!("presenter_wide", render(&sample_state(), 100, 24));
}

#[test]
fn search_prompt_renders_labelled_term_on_bottom_row() {
    // AC-8: while a search prompt is open, the `Search: foo (1/3)`-style string appears on the
    // bottom row. The presenter renders whatever string is in ViewState.prompt; this test checks
    // that the Presenter draws the bottom row correctly (it does not build the string itself).
    let mut st = sample_state();
    st.prompt = Some("Search: foo (1/3)".into());
    let (w, h) = (100u16, 24u16);
    let out = render(&st, w, h);
    let last_row = out.lines().last().expect("at least one row");
    assert!(
        last_row.contains("Search: foo (1/3)"),
        "AC-8: the bottom row must show the labelled search prompt string\n{out}"
    );
    // Content still visible above the prompt.
    assert!(
        out.contains("fn main()"),
        "content is still drawn above the prompt line\n{out}"
    );
}

#[test]
fn search_highlight_snapshot() {
    // Snapshot the highlighted content pane so regressions in highlight::apply's output are caught.
    insta::assert_snapshot!(
        "presenter_search_highlight",
        render(&search_state(), 100, 24)
    );
}

// ── LineSelectView overlay — AC-1 (marker visible on entry), AC-7 (marker stays visible) ─────

/// A `ViewState` with a known six-line content and the copy-line-reference modal active over the
/// given 1-based `marker`/`start`/`end`. Focus is Content so the pane is drawn full/side by side.
/// Notices are cleared so the content sits at the top of the pane and the marked rows are easy to
/// read in the snapshot.
fn line_select_state(marker: usize, start: usize, end: usize) -> ViewState {
    use herdr_file_viewer::render::to_text;
    let mut st = sample_state();
    st.notices = vec![];
    st.focus = Focus::Content;
    st.content = to_text("line one\nline two\nline three\nline four\nline five\nline six\n");
    st.content_rows = 6;
    st.line_select = Some(LineSelectView { marker, start, end });
    st
}

#[test]
fn line_select_marker_snapshot() {
    // AC-1: a single-line selection (marker on line 3) draws the marker caret on exactly that row —
    // the gutter caret ▶ sits on "line three" and nowhere else.
    insta::assert_snapshot!(
        "presenter_line_select_marker",
        render(&line_select_state(3, 3, 3), 100, 24)
    );
}

#[test]
fn selection_range_highlight_snapshot() {
    // AC-7: a multi-line selection (lines 2–4, marker on the end at line 4) draws the selection bar │
    // on lines 2 and 3 and the marker caret ▶ on line 4 — the highlight spans exactly those rows.
    insta::assert_snapshot!(
        "presenter_line_select_range",
        render(&line_select_state(4, 2, 4), 100, 24)
    );
}

#[test]
fn line_select_marker_and_selection_carry_theme_styles() {
    // The marker row carries CURRENT_HIGHLIGHT (REVERSED+BOLD, theme-relative) and the other
    // selection rows carry HIGHLIGHT (black on cyan) — reusing the search theme seam, not raw
    // hardcoded colors. Rows outside the selection are untouched. (AC-1/AC-7, read-only styling.)
    use herdr_file_viewer::highlight::{CURRENT_HIGHLIGHT, HIGHLIGHT};
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;

    let st = line_select_state(4, 2, 4); // marker line 4, selection lines 2–4
    let (w, h) = (100u16, 24u16);
    let area = Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };
    let buf = render_buffer(&st, w, h);
    let content_inner = geometry(area, &st)
        .content_inner
        .expect("content inner must be present");

    // Locate a text needle within the content area and return its (x, y).
    let find_in_content = |needle: &str| -> Option<(u16, u16)> {
        for y in content_inner.y..(content_inner.y + content_inner.height) {
            for x in content_inner.x..(content_inner.x + content_inner.width) {
                let hit = needle.chars().enumerate().all(|(i, ch)| {
                    let cx = x + i as u16;
                    cx < content_inner.x + content_inner.width
                        && buf
                            .cell((cx, y))
                            .is_some_and(|c| c.symbol() == ch.to_string())
                });
                if hit {
                    return Some((x, y));
                }
            }
        }
        None
    };

    // Marker line (line 4 = "line four"): REVERSED+BOLD (CURRENT_HIGHLIGHT).
    let (mx, my) = find_in_content("line four").expect("'line four' must appear");
    let marker_mod = buf.cell((mx, my)).unwrap().modifier;
    assert!(
        marker_mod.contains(Modifier::REVERSED) && marker_mod.contains(Modifier::BOLD),
        "AC-1: the marker row must carry CURRENT_HIGHLIGHT (REVERSED+BOLD), got {marker_mod:?}"
    );
    assert_eq!(
        CURRENT_HIGHLIGHT.add_modifier,
        marker_mod & CURRENT_HIGHLIGHT.add_modifier
    );

    // Selection row (line 2 = "line two", not the marker): HIGHLIGHT (black on cyan).
    let (sx, sy) = find_in_content("line two").expect("'line two' must appear");
    let sel_bg = buf.cell((sx, sy)).unwrap().bg;
    assert_eq!(
        sel_bg,
        HIGHLIGHT.bg.unwrap(),
        "AC-7: a selection row must carry the HIGHLIGHT background (cyan), got {sel_bg:?}"
    );

    // Outside the selection (line 6 = "line six"): no highlight background.
    let (ox, oy) = find_in_content("line six").expect("'line six' must appear");
    let out_bg = buf.cell((ox, oy)).unwrap().bg;
    assert_ne!(
        out_bg,
        HIGHLIGHT.bg.unwrap(),
        "a row outside the selection must not be highlighted"
    );
}

// ── Help overlay tests (AC-5, AC-11) ────────────────────────────────────

/// A `ViewState` with the help overlay open. Two sections, active = the SECOND (About) so the
/// snapshot proves the active indicator picks the active tab — not just the first. The body is a
/// few lines of plain text; the hint string is what the controller carries (AC-11).
fn help_state() -> ViewState {
    let mut state = sample_state();
    state.help = Some(HelpView {
        active: 1, // About is active (the second tab) — proves the active indicator
        labels: vec!["What's New".to_string(), "About".to_string()],
        body: to_text(
            "Herdr File Viewer\n\
             A git-aware, read-only file viewer\n\
             \n\
             github.com/smarzban/herdr-file-viewer\n\
             \n\
             v1.10.0 · Up to date\n\
             MIT License\n\
             \n\
             If you enjoy the file viewer, don't forget to give it a ★ on GitHub!",
        ),
        scroll: 0,
        hint: "Tab/←→ switch · Esc/q close".to_string(),
        center: true, // About is center-aligned (What's New stays left)
    });
    state
}

#[test]
fn help_overlay_indicates_active_section_and_shows_footer_hints() {
    // AC-5: the section tabs are shown with the ACTIVE one (About) visibly indicated.
    // AC-11: the footer shows how to switch sections and how to close.
    let out = render(&help_state(), 100, 24);
    assert!(out.contains("What's New"), "What's New tab is shown\n{out}");
    assert!(out.contains("About"), "About tab is shown\n{out}");
    // The footer hints (switch + close) appear (AC-11).
    assert!(
        out.contains("switch") && out.contains("close"),
        "footer shows how to switch sections and close\n{out}"
    );
    // The active body (About) is shown — its top line is the display title.
    assert!(
        out.contains("Herdr File Viewer"),
        "the active section's body is drawn\n{out}"
    );
    // The two-column layout is still underneath (AC-1 — partial overlay).
    assert!(
        out.contains("┌r"),
        "the tree column is drawn under the overlay\n{out}"
    );
}

#[test]
fn help_overlay_active_tab_is_reversed() {
    // AC-5: the active tab ("About") is rendered with a visible indicator (REVERSED), distinct
    // from the inactive tab ("What's New").
    use ratatui::style::Modifier;

    let st = help_state();
    let buf = render_buffer(&st, 100, 24);
    let (w, h) = (buf.area().width, buf.area().height);

    // Locate "About" in the buffer and confirm its first cell is REVERSED (the active indicator).
    let needle = "About";
    let mut found: Option<(u16, u16)> = None;
    'outer: for y in 0..h {
        for x in 0..w {
            let matches = needle.chars().enumerate().all(|(i, ch)| {
                let cx = x + i as u16;
                cx < w
                    && buf
                        .cell((cx, y))
                        .is_some_and(|c| c.symbol() == ch.to_string())
            });
            if matches {
                found = Some((x, y));
                break 'outer;
            }
        }
    }
    let (ax, ay) = found.expect("the active tab label 'About' is in the buffer");
    assert!(
        buf.cell((ax, ay))
            .unwrap()
            .modifier
            .contains(Modifier::REVERSED),
        "the active tab (About) is REVERSED-highlighted (AC-5)"
    );
}

#[test]
fn help_overlay_snapshot() {
    insta::assert_snapshot!("presenter_help", render(&help_state(), 100, 24));
}

// follow-up regression (AC-8/AC-9): the help body is drawn with `Paragraph::wrap`, so its
// scroll extent must be measured in WRAPPED rows, not raw `lines.len()`. A body with only a few
// raw lines that each wrap many times overflows the viewport even though the raw line count fits —
// `geometry()` must report `help_body_rows > body.lines.len()` AND surface the vertical scrollbar.
// Without this, the bottom of a long (wrapping) changelog is unreachable and the bar is mis-sized.
#[test]
fn geometry_help_body_rows_counts_wrapped_rows_and_triggers_scrollbar() {
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;

    // Four raw lines, each ~300 cols of space-separated words → each wraps to several rows at the
    // ~72-col body width. Raw count (4) easily FITS the viewport; the wrapped total does NOT.
    let long_line = "word ".repeat(60); // 300 chars
    let body_str = format!("{long_line}\n{long_line}\n{long_line}\n{long_line}");
    let raw_lines = 4usize;

    let mut state = help_state();
    state.help = Some(HelpView {
        active: 0,
        labels: vec!["What's New".to_string(), "About".to_string()],
        body: to_text(&body_str),
        scroll: 0,
        hint: "Tab/←→ switch · Esc/q close".to_string(),
        center: false, // What's New stays left-aligned
    });

    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 24,
    };
    let g = geometry(area, &state);

    let body_height = g.help_body_height;
    assert!(
        body_height as usize >= raw_lines,
        "precondition: the {raw_lines} raw lines fit the viewport (height {body_height}) — \
         so a RAW-line trigger would not show a scrollbar"
    );
    assert!(
        g.help_body_rows as usize > raw_lines,
        "help_body_rows ({}) must exceed the raw line count ({raw_lines}) — wrapped rows are counted",
        g.help_body_rows
    );
    assert!(
        g.help_body_rows > body_height,
        "help_body_rows ({}) must exceed the viewport height ({body_height}) so the body overflows",
        g.help_body_rows
    );
    assert!(
        g.help_vbar.is_some(),
        "the vertical scrollbar must be present when the WRAPPED body overflows, even though the \
         raw line count fits"
    );
}

// FIX-A regression (AC-8/AC-9): `help_body_rows` must be measured at the ACTUAL drawn body width
// (post scrollbar-gutter), not the full `inner.width`. When the scrollbar shows, the body draws into
// a NARROWER `text.width` (inner.width − 2), so the true wrapped count is HIGHER than a count taken at
// `inner.width`. If `geometry()` reported the wider-width count, the controller's scroll clamp would
// stop short and a long wrapping changelog's tail would be unreachable. We craft a body whose wrapped
// count strictly differs between the two widths and assert the reported total equals the NARROWER
// (post-gutter) measurement — i.e. clamping reaches the true last wrapped row.
#[test]
fn geometry_help_body_rows_measured_at_post_gutter_width() {
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::Rect;

    // A 100-wide frame ⇒ help popup inner width 72; once the vbar gutter is reserved the body draws
    // at width 70. Use unbroken tokens (no spaces) so wrapping is pure char-wrap = ceil(len / width),
    // letting the test predict both counts exactly without re-implementing word-wrap.
    let inner_w = 72usize;
    let text_w = inner_w - 2; // one gutter column + one gap (bar_layout reserves 2)

    // Per-line length chosen so ceil(len/70) > ceil(len/72): 72*2 = 144 wraps to 2 rows at width 72
    // but 3 rows at width 70. Stack enough such lines that the body overflows the viewport (forcing
    // the scrollbar — the only case where the two widths diverge).
    let token = "x".repeat(144);
    let n_lines = 20usize;
    let body_str = vec![token.as_str(); n_lines].join("\n");

    let rows_at_inner = (144usize.div_ceil(inner_w)) * n_lines; // 2 * 20 = 40
    let rows_at_text = (144usize.div_ceil(text_w)) * n_lines; // 3 * 20 = 60
    assert!(
        rows_at_text > rows_at_inner,
        "test precondition: the post-gutter width must yield more wrapped rows"
    );

    let mut state = help_state();
    state.help = Some(HelpView {
        active: 0,
        labels: vec!["What's New".to_string(), "About".to_string()],
        body: to_text(&body_str),
        scroll: 0,
        hint: "Tab/←→ switch · Esc/q close".to_string(),
        center: false, // What's New stays left-aligned
    });

    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 24,
    };
    let g = geometry(area, &state);

    // The scrollbar must be present (the body overflows), which is the only regime where the body
    // draws into the narrower post-gutter width.
    assert!(
        g.help_vbar.is_some(),
        "the vertical scrollbar must show when the wrapped body overflows"
    );
    // The reported total must match the POST-GUTTER (narrower) measurement — not the wider
    // `inner.width` one. A regression to `inner.width` would report `rows_at_inner` and leave the
    // changelog's tail (the missing `rows_at_text − rows_at_inner` rows) unreachable.
    assert_eq!(
        g.help_body_rows as usize, rows_at_text,
        "help_body_rows ({}) must be measured at the drawn body width {text_w} (= {rows_at_text}), \
         not at inner.width {inner_w} (= {rows_at_inner}) — else the tail is unreachable",
        g.help_body_rows
    );
}

// crux (AC-10): the section-tab rects `geometry()` feeds back (`help_tabs`) must line up with
// where the tabs are ACTUALLY drawn, so a click maps to the tab actually drawn (draw + hit-test
// can't drift). Render the overlay, scan the buffer for each label, and assert the drawn label
// sits inside its reported rect; also assert the ACTIVE tab's REVERSED cell falls in its rect.
#[test]
fn help_tab_rects_agree_with_drawn_tab_positions() {
    use herdr_file_viewer::presenter::geometry;
    use ratatui::layout::{Position, Rect};
    use ratatui::style::Modifier;

    let st = help_state(); // active = 1 (About); labels = ["What's New", "About"]
    let (w, h) = (100u16, 24u16);
    let area = Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };
    let buf = render_buffer(&st, w, h);

    // Find the first (x,y) where `needle` is drawn contiguously.
    let find_cell = |needle: &str| -> (u16, u16) {
        for y in 0..h {
            for x in 0..w {
                let hit = needle.chars().enumerate().all(|(i, ch)| {
                    let cx = x + i as u16;
                    cx < w
                        && buf
                            .cell((cx, y))
                            .is_some_and(|c| c.symbol() == ch.to_string())
                });
                if hit {
                    return (x, y);
                }
            }
        }
        panic!("{needle:?} not found in buffer — draw did not render it");
    };

    let g = geometry(area, &st);
    assert_eq!(g.help_tabs.len(), 2, "both section tabs have a rect");

    // Each label's drawn start cell must be contained in its reported tab rect.
    let labels = ["What's New", "About"];
    for (idx, rect) in &g.help_tabs {
        let (dx, dy) = find_cell(labels[*idx]);
        assert!(
            rect.contains(Position { x: dx, y: dy }),
            "tab {idx} ({:?}) drawn at ({dx},{dy}) must lie inside its reported rect {rect:?}",
            labels[*idx]
        );
        assert_eq!(rect.y, dy, "the tab rect row must match the drawn row");
        // the active tab carries a leading `▶ ` marker, so its rect start col is the
        // marker's col (the label follows 2 cells later). Inactive tabs have no marker, so the
        // rect start col equals the label's drawn col.
        if *idx == 1 {
            assert_eq!(
                dx - rect.x,
                2,
                "the active tab's rect starts at the `▶ ` marker (2 cells before the label)"
            );
        } else {
            assert_eq!(
                rect.x, dx,
                "the inactive tab rect start col matches the drawn col (no marker)"
            );
        }
    }

    // The ACTIVE tab (index 1 = About) is REVERSED; its first cell (the `▶ ` marker)
    // must be inside its rect and REVERSED — the rect tracks the drawn tab including the marker.
    let (about_idx, about_rect) = g
        .help_tabs
        .iter()
        .find(|(i, _)| *i == 1)
        .copied()
        .expect("the active tab (About) has a rect");
    assert_eq!(about_idx, 1);
    assert!(
        buf.cell((about_rect.x, about_rect.y))
            .unwrap()
            .modifier
            .contains(Modifier::REVERSED),
        "the active tab's first cell (at its rect origin) is REVERSED — the rect tracks the drawn tab"
    );
}

// ── discoverability — help hint + empty-state guidance ──────────────

#[test]
fn content_pane_shows_a_help_hint_on_its_bottom_border() {
    // The `? help` affordance rides the content block's bottom border so a new user discovers
    // the help overlay without opening it first. Visible on the default (file-selected) screen.
    let out = render(&sample_state(), 100, 24);
    assert!(
        out.contains("? help"),
        "the '? help' hint must be visible on the content pane's bottom border\n{out}"
    );
    // It sits on the LAST line (the content block's bottom border), not in the tree column.
    let last = out.lines().last().unwrap();
    assert!(
        last.contains("? help"),
        "the hint is on the bottom border row\n{last}"
    );
}

#[test]
fn content_pane_shows_help_hint_even_when_a_file_is_loaded() {
    // Sanity: the hint is persistent, not only on an empty pane.
    let out = render(&sample_state(), 100, 24);
    assert!(out.contains("fn main()"), "a file is loaded\n{out}");
    assert!(out.contains("? help"), "the hint is still shown\n{out}");
}

#[test]
fn selecting_a_directory_shows_empty_state_guidance_not_a_blank_pane() {
    // a directory selection shows "Directory — select a file to view" in the content
    // pane, instead of a blank void. Built directly in the Presenter (the controller's
    // clear_content sets this copy).
    let mut state = sample_state();
    // Select the directory row (index 0 = /r/src).
    state.selected = 0;
    state.content = to_text("Directory — select a file to view");
    state.notices.clear();
    // a directory selection has no displayed file content, so the title falls back to
    // the selected node's name (the directory). Mirrors the controller's `clear_content`.
    state.content_title = None;
    state.content_rendering = false;
    let out = render(&state, 100, 24);
    assert!(
        out.contains("Directory — select a file to view"),
        "a directory selection shows guidance, not a blank pane\n{out}"
    );
}

#[test]
fn an_empty_tree_shows_empty_state_guidance_not_a_blank_pane() {
    // an empty / zero-match tree (no files, or a filter matched nothing) shows "No
    // files" in the content pane instead of leaving it blank.
    let mut state = sample_state();
    state.nodes = vec![]; // empty tree
    state.selected = 0;
    state.content = to_text("No files");
    state.notices.clear();
    // no file content displayed → title falls back to "Content" (no selected node).
    state.content_title = None;
    state.content_rendering = false;
    let out = render(&state, 100, 24);
    assert!(
        out.contains("No files"),
        "an empty tree shows guidance, not a blank pane\n{out}"
    );
}

#[test]
fn empty_state_directory_snapshot() {
    // Snapshot the directory-selected empty state so the layout + the guidance are locked.
    let mut state = sample_state();
    state.selected = 0; // the /r/src directory
    state.content = to_text("Directory — select a file to view");
    state.notices.clear();
    // directory selected → no file content → title falls back to the directory's name.
    state.content_title = None;
    state.content_rendering = false;
    insta::assert_snapshot!("presenter_empty_directory", render(&state, 100, 24));
}

#[test]
fn empty_state_no_files_snapshot() {
    // Snapshot the empty-tree empty state.
    let mut state = sample_state();
    state.nodes = vec![];
    state.selected = 0;
    state.content = to_text("No files");
    state.notices.clear();
    // no file content displayed → title falls back to "Content" (no selected node).
    state.content_title = None;
    state.content_rendering = false;
    insta::assert_snapshot!("presenter_empty_no_files", render(&state, 100, 24));
}

#[test]
fn loading_state_snapshot_while_a_render_is_in_flight() {
    // while an off-thread render for a newly-selected file is in flight, the content
    // pane shows a loading placeholder body and a neutral "Content" title (NOT the still-loading
    // selection's name). The controller's `dispatch_render` sets `content_rendering = true` and
    // `content_title = None` (no content has landed yet — launch / re-root), so the Presenter
    // falls back to "Content" instead of picking up the new selection's name. This snapshot locks
    // that visual: the body is the placeholder and the border title is "Content".
    let mut state = sample_state();
    // The cursor has moved to README.md (index 4) but its render hasn't landed — the body is the
    // placeholder and `content_title` is `None` (no content has landed at all yet, e.g. launch).
    state.selected = 4; // README.md
    state.content = to_text("Rendering\u{2026}");
    state.notices.clear();
    state.content_title = None;
    state.content_rendering = true;
    insta::assert_snapshot!("presenter_loading", render(&state, 100, 24));
}
