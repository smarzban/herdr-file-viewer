//! Pinned and active preview projections share one drawing path.

use herdr_file_viewer::presenter::{
    AnnotationIndicatorsView, CharSelView, ContentSearch, Focus, PreviewProjection, ViewState,
    draw, geometry,
};
use herdr_file_viewer::preview::{BranchState, PreviewOrigin};
use herdr_file_viewer::render::to_text;
use herdr_file_viewer::search::Match;
use ratatui::{Terminal, backend::TestBackend};
use std::path::PathBuf;

fn projection(title: &str, content: &str) -> PreviewProjection {
    PreviewProjection::new(title, to_text(content))
}

fn state(pinned: PreviewProjection) -> ViewState {
    let mut active = projection("active.rs", "ACTIVE needle\nactive second row\n");
    active.notices = vec!["active notice".into()];
    active.search = Some(ContentSearch {
        matches: vec![Match {
            line: 1,
            start: 7,
            end: 13,
        }],
        current: 0,
    });
    active.selection = Some(CharSelView {
        start_line: 2,
        start_col: 0,
        end_line: 2,
        end_col: 6,
        gutter: 0,
    });
    ViewState {
        nodes: Vec::new(),
        selected: 0,
        active,
        pinned: Some(pinned),
        focus: Focus::Content,
        width: 150,
        tree_scroll: 0,
        tree_hscroll: 0,
        preview_split_pct: 50,
        split_pct: 20,
        tree_position: herdr_file_viewer::config::TreePosition::Left,
        tree_max_cols: 20,
        tree_icons: herdr_file_viewer::config::TreeIcons::Off,
        split_manual: false,
        zoomed: false,
        remote_notice_status: None,
        picker: None,
        finder: None,
        annotation_count: 0,
        annotation_overview: None,
        annotation_editor: None,
        discard_confirm: None,
        annotation_indicators: AnnotationIndicatorsView::default(),
        root_name: "r".into(),
        branch: None,
        pinned_foreign_root: None,
        prompt: None,
        help: None,
    }
}

/// The same fixture with the pin marked as captured in ANOTHER worktree, which is the only case
/// that earns the worktree label in the title (AC-12).
fn state_from_foreign_worktree(pinned: PreviewProjection, worktree: &str) -> ViewState {
    ViewState {
        pinned_foreign_root: Some(worktree.to_string()),
        ..state(pinned)
    }
}

fn render(
    state: &ViewState,
    width: u16,
    height: u16,
) -> (String, herdr_file_viewer::presenter::PreviewViewports) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut viewports = Default::default();
    terminal
        .draw(|frame| {
            viewports = draw(frame, state);
        })
        .unwrap();
    (format!("{}", terminal.backend()), viewports)
}

fn origin(root: &str, branch: BranchState, path: &str) -> PreviewOrigin {
    PreviewOrigin::new(
        PathBuf::from(root),
        branch,
        PathBuf::from(root).join(path),
        PathBuf::from(path),
    )
}

#[test]
fn wide_three_region_snapshot_and_independent_viewports() {
    let mut pinned = projection("ignored by origin", "PINNED needle\npinned second row\n");
    pinned.notices = vec!["pinned notice".into()];
    pinned.wrap = true;
    pinned.rows = 4;
    pinned.search = Some(ContentSearch {
        matches: vec![Match {
            line: 1,
            start: 7,
            end: 13,
        }],
        current: 0,
    });
    pinned.origin = Some(origin(
        "/worktrees/review",
        BranchState::Named("review".into()),
        "src/pinned.rs",
    ));
    let mut state = state(pinned);
    state.preview_split_pct = 40;
    let (output, viewports) = render(&state, 150, 16);

    assert!(
        output.contains("PINNED") && output.contains("ACTIVE"),
        "{output}"
    );
    assert!(
        output.contains("pinned notice") && output.contains("active notice"),
        "{output}"
    );
    assert!(
        viewports.pinned.is_some(),
        "pinned viewport feedback is named"
    );
    assert_ne!(
        viewports.active,
        viewports.pinned.unwrap(),
        "each projection measures its own viewport"
    );
    insta::assert_snapshot!("wide_three_region", output);
}

#[test]
fn tree_hidden_keeps_both_preview_projections() {
    let mut pinned = projection("pinned.rs", "PINNED\n");
    pinned.origin = Some(origin(
        "/repo",
        BranchState::Named("main".into()),
        "pinned.rs",
    ));
    let mut state = state(pinned);
    state.zoomed = true;
    let (output, viewports) = render(&state, 100, 12);

    assert!(
        output.contains("PINNED") && output.contains("ACTIVE"),
        "{output}"
    );
    assert!(viewports.pinned.is_some() && viewports.active.0 > 0);
    insta::assert_snapshot!("tree_hidden_two_preview", output);
}

#[test]
fn narrow_pin_keeps_active_projection_and_surfaces_widen_notice() {
    let mut pinned = projection("pinned.rs", "PINNED NARROW\n");
    pinned.origin = Some(origin(
        "/worktrees/review",
        BranchState::Named("review".into()),
        "pinned.rs",
    ));
    let mut state = state(pinned);
    state.focus = Focus::Pinned;
    let (output, viewports) = render(&state, 60, 12);

    assert!(!output.contains("PINNED NARROW"), "{output}");
    assert!(output.contains("ACTIVE needle"), "{output}");
    assert!(
        output.contains("Pinned: pinned.rs — widen to view"),
        "{output}"
    );
    assert!(viewports.pinned.is_none());
    assert_ne!(viewports.active, (0, 0));
}

#[test]
fn narrow_pin_focus_snapshots() {
    for (name, focus) in [
        ("narrow_tree", Focus::Tree),
        ("narrow_pinned", Focus::Pinned),
        ("narrow_active", Focus::Content),
    ] {
        let mut pinned = projection("pinned.rs", "PINNED\n");
        pinned.origin = Some(origin(
            "/worktrees/review",
            BranchState::Named("review".into()),
            "pinned.rs",
        ));
        let mut state = state(pinned);
        state.focus = focus;
        let (output, _) = render(&state, 60, 12);
        insta::assert_snapshot!(name, output);
    }
}

#[test]
fn pinned_origin_identity_is_visible_and_neutralized() {
    // The third element is the foreign worktree label: `None` means the pin came from the viewed
    // worktree, where the branch alone is the origin and no worktree is named.
    let cases = [
        (
            "origin_named_branch",
            origin(
                "/worktrees/feature",
                BranchState::Named("feat/one".into()),
                "src/lib.rs",
            ),
            None,
        ),
        (
            "origin_detached",
            origin("/worktrees/detached", BranchState::Detached, "README.md"),
            None,
        ),
        (
            "origin_cross_worktree",
            origin(
                "/worktrees/other\u{1b}[31m",
                BranchState::Named("topic\u{7}".into()),
                "src/other.rs",
            ),
            Some("other\u{1b}[31m"),
        ),
    ];
    for (name, origin, foreign) in cases {
        let mut pinned = projection("ignored", "PINNED\n");
        pinned.origin = Some(origin);
        let state = match foreign {
            Some(worktree) => state_from_foreign_worktree(pinned, worktree),
            None => state(pinned),
        };
        let (output, _) = render(&state, 150, 10);
        assert!(
            !output.contains('\u{1b}') && !output.contains('\u{7}'),
            "{output}"
        );
        assert!(
            !output.contains("src/lib.rs")
                && !output.contains("README.md")
                && !output.contains("src/other.rs"),
            "AC-12: the pinned title contains origin only, not its path\n{output}"
        );
        // A same-worktree pin must not carry a worktree at all: repeating the viewed worktree's
        // own name is the width that clipped the origin away before this amendment.
        assert_eq!(
            output.contains("Pinned: [feat/one]") || output.contains("Pinned: [detached]"),
            foreign.is_none(),
            "AC-12: only a cross-worktree pin names its worktree\n{output}"
        );
        insta::assert_snapshot!(name, output);
    }
}

#[test]
fn prompt_and_remote_status_stay_full_width_and_unique_with_a_pin() {
    let mut pinned = projection("pinned.rs", "PINNED\n");
    pinned.origin = Some(origin(
        "/repo",
        BranchState::Named("main".into()),
        "pinned.rs",
    ));
    let mut state = state(pinned);
    state.prompt = Some("Search: needle".into());
    state.remote_notice_status = Some("Update available".into());
    let (output, _) = render(&state, 150, 12);
    assert_eq!(output.matches("Search: needle").count(), 1, "{output}");
    assert_eq!(output.matches("Update available").count(), 1, "{output}");
    let layout = geometry(ratatui::layout::Rect::new(0, 0, 150, 12), &state);
    assert_eq!(layout.area_width, 150);
}
