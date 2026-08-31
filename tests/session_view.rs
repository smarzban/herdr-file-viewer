//! Session view — tree-level behavior: the two-section synthesis (in-root tree + separator +
//! outside-root section), category glyph decoration, cursor rules around the separator, the
//! session file jump, and reveal's relaxation. Controller-level behavior (toggle, pickers,
//! live-follow) lives in `session_controller.rs`; transcript parsing in `session_transcript.rs`.

mod common;

use common::TempDir;
use herdr_file_viewer::session::{Category, SessionSet};
use herdr_file_viewer::tree::{NodeKind, TreeModel};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// A session set from `(relative-path, category)` in-root members and absolute outside members.
fn set(in_root: &[(&str, Category)], outside: &[(&str, Category)]) -> SessionSet {
    SessionSet {
        in_root: in_root
            .iter()
            .map(|(p, c)| (PathBuf::from(p), *c))
            .collect::<BTreeMap<_, _>>(),
        outside: outside
            .iter()
            .map(|(p, c)| (PathBuf::from(p), *c))
            .collect::<BTreeMap<_, _>>(),
    }
}

#[test]
fn session_view_synthesizes_in_root_tree_then_separator_then_outside_groups() {
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/new.rs"), "x").unwrap();
    fs::write(dir.path().join("README.md"), "x").unwrap();

    let mut tree = TreeModel::new(dir.path());
    tree.set_session_view(
        true,
        &set(
            &[
                ("src/new.rs", Category::Created),
                ("README.md", Category::Mentioned),
            ],
            &[("/tmp/scratch.txt", Category::Updated)],
        ),
        None,
    );
    assert!(tree.session_view());

    let rows = tree.visible_nodes();
    let kinds: Vec<(String, NodeKind)> = rows
        .iter()
        .map(|n| (n.path.to_string_lossy().into_owned(), n.kind))
        .collect();
    // In-root synthesis: dirs before files, every dir expanded, then the divider, then the
    // outside group (its /tmp header row is a labelled Dir) and its file.
    let names: Vec<&str> = kinds
        .iter()
        .map(|(p, k)| {
            if *k == NodeKind::Separator {
                "<sep>"
            } else {
                p.rsplit('/').next().unwrap()
            }
        })
        .collect();
    assert_eq!(
        names,
        ["src", "new.rs", "README.md", "<sep>", "tmp", "scratch.txt"],
        "rows: {kinds:?}"
    );

    // Categories decorate exactly the member FILE rows; dirs and the separator carry none.
    assert_eq!(rows[1].session, Some(Category::Created));
    assert_eq!(rows[2].session, Some(Category::Mentioned));
    assert_eq!(rows[0].session, None);
    assert_eq!(rows[3].session, None);
    assert_eq!(rows[5].session, Some(Category::Updated));
    // Outside rows never carry git decoration.
    assert_eq!(rows[4].status, None);
    assert!(!rows[4].dir_dirty);
    // The outside group header is labelled with its absolute path.
    assert_eq!(rows[4].label.as_deref(), Some("/tmp"));
}

#[test]
fn outside_section_folds_chains_and_abbreviates_home() {
    let dir = TempDir::new();
    let mut tree = TreeModel::new(dir.path());
    tree.set_session_view(
        true,
        &set(
            &[],
            &[
                ("/home/u/.claude/CLAUDE.md", Category::Updated),
                ("/home/u/.claude/skills/x/SKILL.md", Category::Mentioned),
            ],
        ),
        Some(PathBuf::from("/home/u")),
    );
    let rows = tree.visible_nodes();
    // Rows: separator, the folded `~/.claude` group header, then its children — the
    // `skills/x` chain folds into one row (label `skills/x`) above its file, beside the file
    // member directly under `.claude`.
    assert_eq!(rows[0].kind, NodeKind::Separator);
    assert_eq!(rows[1].label.as_deref(), Some("~/.claude"));
    assert_eq!(rows[1].kind, NodeKind::Dir);
    let labels: Vec<Option<&str>> = rows.iter().map(|n| n.label.as_deref()).collect();
    assert!(
        labels.contains(&Some("skills/x")),
        "the single-child chain must fold: {labels:?}"
    );
    let files: Vec<&str> = rows
        .iter()
        .filter(|n| n.kind == NodeKind::File)
        .map(|n| n.path.file_name().unwrap().to_str().unwrap())
        .collect();
    assert_eq!(files, ["SKILL.md", "CLAUDE.md"]);
}

#[test]
fn separator_is_never_selectable() {
    let dir = TempDir::new();
    fs::write(dir.path().join("a.rs"), "x").unwrap();
    let mut tree = TreeModel::new(dir.path());
    tree.set_session_view(
        true,
        &set(
            &[("a.rs", Category::Mentioned)],
            &[("/tmp/out.txt", Category::Created)],
        ),
        None,
    );
    let rows = tree.visible_nodes();
    let sep = rows
        .iter()
        .position(|n| n.kind == NodeKind::Separator)
        .expect("a separator row");

    // A click on the separator snaps to the next selectable row below.
    tree.set_cursor(sep);
    assert_ne!(tree.selected().unwrap().kind, NodeKind::Separator);
    assert_eq!(tree.cursor(), sep + 1);

    // Keyboard movement steps over it in the direction of travel.
    tree.set_cursor(sep - 1);
    tree.move_cursor(1);
    assert_eq!(tree.cursor(), sep + 1);
    tree.move_cursor(-1);
    assert_eq!(tree.cursor(), sep - 1);
}

#[test]
fn select_next_file_row_cycles_members_across_both_sections() {
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/a.rs"), "x").unwrap();
    let mut tree = TreeModel::new(dir.path());
    tree.set_session_view(
        true,
        &set(
            &[("src/a.rs", Category::Updated)],
            &[("/tmp/out.txt", Category::Created)],
        ),
        None,
    );
    // Rows: src, a.rs, <sep>, tmp, out.txt — the file jump visits a.rs and out.txt only.
    tree.set_cursor(0);
    assert_eq!(tree.select_next_file_row(true), Some(false));
    assert_eq!(tree.selected().unwrap().path.file_name().unwrap(), "a.rs");
    assert_eq!(tree.select_next_file_row(true), Some(false));
    assert_eq!(
        tree.selected().unwrap().path.file_name().unwrap(),
        "out.txt"
    );
    // Past the last member it wraps and reports it.
    assert_eq!(tree.select_next_file_row(true), Some(true));
    assert_eq!(tree.selected().unwrap().path.file_name().unwrap(), "a.rs");
    // …and backwards, wrapping the other way.
    assert_eq!(tree.select_next_file_row(false), Some(true));
    assert_eq!(
        tree.selected().unwrap().path.file_name().unwrap(),
        "out.txt"
    );
}

#[test]
fn reveal_relaxes_the_session_view_for_a_non_member() {
    let dir = TempDir::new();
    fs::write(dir.path().join("member.rs"), "x").unwrap();
    fs::write(dir.path().join("other.rs"), "x").unwrap();
    let mut tree = TreeModel::new(dir.path());
    tree.set_session_view(true, &set(&[("member.rs", Category::Updated)], &[]), None);

    // A member reveals without touching the view…
    assert!(tree.reveal(&dir.path().join("member.rs")));
    assert!(tree.session_view());
    // …an explicit non-member target relaxes it, exactly like the changed-only filter.
    assert!(tree.reveal(&dir.path().join("other.rs")));
    assert!(!tree.session_view());
    assert_eq!(
        tree.selected().unwrap().path.file_name().unwrap(),
        "other.rs"
    );
}

#[test]
fn deleted_members_stay_listed_with_the_missing_cue() {
    let dir = TempDir::new();
    fs::write(dir.path().join("kept.rs"), "x").unwrap();
    let mut tree = TreeModel::new(dir.path());
    tree.set_session_view(
        true,
        &set(
            &[
                ("kept.rs", Category::Updated),
                ("gone.rs", Category::Created),
            ],
            &[],
        ),
        None,
    );
    let rows = tree.visible_nodes();
    let row = |name: &str| {
        rows.iter()
            .find(|n| n.path.file_name().is_some_and(|f| f == name))
            .unwrap()
    };
    assert!(!row("kept.rs").session_missing);
    assert!(
        row("gone.rs").session_missing,
        "a member deleted since the session touched it keeps its row, cued as missing"
    );
}

#[test]
fn session_decoration_never_leaks_outside_the_session_view() {
    let dir = TempDir::new();
    fs::write(dir.path().join("a.rs"), "x").unwrap();
    let mut tree = TreeModel::new(dir.path());
    tree.set_session_view(true, &set(&[("a.rs", Category::Created)], &[]), None);
    tree.set_session_view(false, &SessionSet::default(), None);

    let rows = tree.visible_nodes();
    assert!(rows.iter().all(|n| n.kind != NodeKind::Separator));
    assert!(rows.iter().all(|n| n.session.is_none()));
    assert!(rows.iter().all(|n| !n.session_missing));

    // The changed-only synthesis shares the emitter; it must stay undecorated too.
    let changed: BTreeMap<PathBuf, herdr_file_viewer::git::Status> = [(
        PathBuf::from("a.rs"),
        herdr_file_viewer::git::Status::Modified,
    )]
    .into_iter()
    .collect();
    tree.set_changed_only(true, &changed);
    assert!(tree.visible_nodes().iter().all(|n| n.session.is_none()));
}

#[test]
fn empty_set_shows_an_empty_session_view() {
    let dir = TempDir::new();
    fs::write(dir.path().join("a.rs"), "x").unwrap();
    let mut tree = TreeModel::new(dir.path());
    tree.set_session_view(true, &SessionSet::default(), None);
    // No members at all: no rows, and in particular no dangling separator.
    assert!(tree.visible_nodes().is_empty());
}

#[test]
fn a_member_directly_in_home_gets_the_bare_tilde_header() {
    let dir = TempDir::new();
    let mut tree = TreeModel::new(dir.path());
    tree.set_session_view(
        true,
        &set(&[], &[("/home/u/.zshrc", Category::Updated)]),
        Some(PathBuf::from("/home/u")),
    );
    let rows = tree.visible_nodes();
    // The chain folds down to home itself; the group header reads `~`, not `/home/u`.
    assert_eq!(rows[1].label.as_deref(), Some("~"));
    assert_eq!(
        rows[2].path.file_name().unwrap().to_str().unwrap(),
        ".zshrc"
    );
}

#[test]
fn directories_toggle_collapsed_and_expanded_in_the_session_view() {
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/a.rs"), "x").unwrap();
    let mut tree = TreeModel::new(dir.path());
    let members = set(
        &[("src/a.rs", Category::Updated)],
        &[("/tmp/out.txt", Category::Created)],
    );
    tree.set_session_view(true, &members, None);

    let files = |tree: &TreeModel| -> Vec<String> {
        tree.visible_nodes()
            .iter()
            .filter(|n| n.kind == NodeKind::File)
            .map(|n| n.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    // Everything starts expanded — glanceability is the view's point.
    assert_eq!(files(&tree), ["a.rs", "out.txt"]);

    // Collapse the in-root directory: its child leaves the rows and the glyph flips.
    let src = dir.path().join("src");
    tree.collapse(&src);
    assert_eq!(
        files(&tree),
        ["out.txt"],
        "a collapsed dir hides its children"
    );
    let src_row = tree
        .visible_nodes()
        .into_iter()
        .find(|n| n.path == src)
        .expect("the collapsed dir keeps its row");
    assert!(!src_row.expanded, "the row must render as collapsed (▸)");

    // A live-follow re-application of the (grown) set must NOT re-expand what the user
    // collapsed — live updates never fight the user's layout.
    tree.set_session_view(true, &members, None);
    assert_eq!(files(&tree), ["out.txt"]);

    // Expanding restores the children.
    tree.expand(&src);
    assert_eq!(files(&tree), ["a.rs", "out.txt"]);

    // The outside-root group header collapses too (display state only).
    tree.collapse(std::path::Path::new("/tmp"));
    assert_eq!(files(&tree), ["a.rs"]);

    // Leaving the view resets the collapse memory: a fresh entry starts fully expanded.
    tree.set_session_view(false, &SessionSet::default(), None);
    tree.set_session_view(true, &members, None);
    assert_eq!(files(&tree), ["a.rs", "out.txt"]);
}

#[test]
fn changed_only_synthesis_stays_always_expanded() {
    // Regression guard for the shared emitter: changed-only mode documents "every directory
    // expanded" (ARCHITECTURE.md), so the session view's collapse support must not leak in.
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/a.rs"), "x").unwrap();
    let mut tree = TreeModel::new(dir.path());
    let changed: BTreeMap<PathBuf, herdr_file_viewer::git::Status> = [(
        PathBuf::from("src/a.rs"),
        herdr_file_viewer::git::Status::Modified,
    )]
    .into_iter()
    .collect();
    tree.set_changed_only(true, &changed);
    tree.collapse(&dir.path().join("src"));
    let names: Vec<_> = tree
        .visible_nodes()
        .iter()
        .map(|n| n.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["src", "a.rs"], "changed-only stays fully expanded");
}
