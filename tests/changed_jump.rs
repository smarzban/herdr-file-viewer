//! Next / previous changed-file jump (`]` / `[`), Tree Model side: traversal order, wrapping,
//! reaching a changed file inside a collapsed directory, and skipping a candidate that has no
//! selectable node. Read-only throughout — the jump moves the cursor and expansion state only
//! (AC-N1, AC-N3).

mod common;

use common::TempDir;
use herdr_file_viewer::git::Status;
use herdr_file_viewer::tree::{NodeKind, TreeModel, cmp_file_rows};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// The file name the cursor currently sits on, or `None` with nothing selected.
fn selected(model: &TreeModel) -> Option<String> {
    model
        .selected()
        .map(|n| n.path.file_name().unwrap().to_string_lossy().into_owned())
}

/// The tree's currently-rendered rows, by file name — the order a user actually sees.
fn rows(model: &TreeModel) -> Vec<String> {
    model
        .visible_nodes()
        .iter()
        .map(|n| n.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

/// A changed-set over the given repo-relative paths, all modified.
fn changed(paths: &[&str]) -> BTreeMap<PathBuf, Status> {
    paths
        .iter()
        .map(|p| (PathBuf::from(p), Status::Modified))
        .collect()
}

#[test]
fn jumps_forward_through_the_changed_set_in_path_order_and_wraps() {
    let dir = TempDir::new();
    for name in ["a.rs", "b.rs", "c.rs", "clean.rs"] {
        fs::write(dir.path().join(name), "x").unwrap();
    }
    let set = changed(&["a.rs", "b.rs", "c.rs"]);
    let mut model = TreeModel::new(dir.path());
    model.set_status(&set);

    // The cursor starts on the first row (a.rs). Each jump advances one changed file — the
    // clean file in between is never selected.
    assert_eq!(selected(&model).as_deref(), Some("a.rs"), "precondition");
    assert_eq!(model.select_changed(true, &set), Some(false));
    assert_eq!(selected(&model).as_deref(), Some("b.rs"));
    assert_eq!(model.select_changed(true, &set), Some(false));
    assert_eq!(selected(&model).as_deref(), Some("c.rs"));

    // Past the last changed file it wraps to the first and reports the wrap, so the caller can
    // say so rather than leaving the key looking dead.
    assert_eq!(model.select_changed(true, &set), Some(true));
    assert_eq!(selected(&model).as_deref(), Some("a.rs"));
}

#[test]
fn jumps_backward_through_the_changed_set_and_wraps_at_the_start() {
    let dir = TempDir::new();
    for name in ["a.rs", "b.rs", "c.rs"] {
        fs::write(dir.path().join(name), "x").unwrap();
    }
    let set = changed(&["a.rs", "b.rs", "c.rs"]);
    let mut model = TreeModel::new(dir.path());
    model.set_status(&set);

    assert_eq!(selected(&model).as_deref(), Some("a.rs"), "precondition");

    // Backward from the first changed file wraps to the last.
    assert_eq!(model.select_changed(false, &set), Some(true));
    assert_eq!(selected(&model).as_deref(), Some("c.rs"));
    assert_eq!(model.select_changed(false, &set), Some(false));
    assert_eq!(selected(&model).as_deref(), Some("b.rs"));
}

#[test]
fn jump_reaches_a_changed_file_inside_a_collapsed_directory() {
    // The point of the feature in a deep repo: the changed file is several collapsed levels
    // down, so a jump that only walked the VISIBLE rows would find nothing.
    let dir = TempDir::new();
    let deep = dir.path().join("src/main/java/br");
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("Deep.java"), "x").unwrap();
    fs::write(dir.path().join("top.txt"), "x").unwrap();

    let set = changed(&["src/main/java/br/Deep.java"]);
    let mut model = TreeModel::new(dir.path());
    model.set_status(&set);
    // Nothing is expanded: the deep file has no visible row yet.
    assert!(
        !model
            .visible_nodes()
            .iter()
            .any(|n| n.path.ends_with("Deep.java")),
        "precondition: the target starts hidden under collapsed directories"
    );

    assert_eq!(model.select_changed(true, &set), Some(false));
    assert_eq!(
        selected(&model).as_deref(),
        Some("Deep.java"),
        "the jump expands the collapsed ancestors and lands on the file"
    );
}

#[test]
fn jump_works_under_the_changed_only_filter() {
    let dir = TempDir::new();
    let sub = dir.path().join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("b.rs"), "x").unwrap();
    fs::write(dir.path().join("a.rs"), "x").unwrap();

    let set = changed(&["a.rs", "sub/b.rs"]);
    let mut model = TreeModel::new(dir.path());
    model.set_status(&set);
    model.set_changed_only(true, &set);
    // The filtered tree lists directories first, so the rows read `sub/`, `sub/b.rs`, `a.rs` —
    // `a.rs` is the LAST row even though it is the first key in the changed-set.
    assert_eq!(rows(&model), ["sub", "b.rs", "a.rs"], "precondition");
    let idx = model
        .visible_nodes()
        .iter()
        .position(|n| n.path.ends_with("a.rs"))
        .unwrap();
    model.set_cursor(idx);

    // Forward from the last row reaches the first candidate, which IS a wrap.
    assert_eq!(model.select_changed(true, &set), Some(true));
    assert_eq!(selected(&model).as_deref(), Some("b.rs"));
    assert!(
        model.changed_only(),
        "jumping inside the filter must not relax it"
    );
}

#[test]
fn jump_skips_a_candidate_with_no_selectable_node() {
    // A deleted file is in the changed-set but has no node on disk and none synthesized outside
    // changed-only mode. It must be skipped rather than swallowing the keypress.
    let dir = TempDir::new();
    fs::write(dir.path().join("b.rs"), "x").unwrap();
    fs::write(dir.path().join("c.rs"), "x").unwrap();

    let set = changed(&["a_deleted.rs", "b.rs", "c.rs"]); // a_deleted.rs is not on disk
    let mut model = TreeModel::new(dir.path());
    model.set_status(&set);
    assert_eq!(selected(&model).as_deref(), Some("b.rs"), "precondition");

    // Backward from b.rs, the immediate candidate is the deleted file: skipping it wraps on to
    // the last selectable one instead of leaving the cursor stuck.
    assert_eq!(model.select_changed(false, &set), Some(true));
    assert_eq!(
        selected(&model).as_deref(),
        Some("c.rs"),
        "the deleted candidate is skipped, the next selectable one wins"
    );
}

#[test]
fn jump_is_inert_with_an_empty_changed_set() {
    let dir = TempDir::new();
    fs::write(dir.path().join("a.rs"), "x").unwrap();
    let mut model = TreeModel::new(dir.path());
    let before = model.cursor();

    assert_eq!(model.select_changed(true, &BTreeMap::new()), None);
    assert_eq!(model.select_changed(false, &BTreeMap::new()), None);
    assert_eq!(model.cursor(), before, "the cursor is left untouched");
}

/// Root-level changed files straddling two sibling directories that each hold one. The tree
/// renders directories before files, so the rows read `docs/`, `x.md`, `src/`, `y.rs`,
/// `Cargo.toml`, `zz.txt` while the changed-set's keys sort `Cargo.toml`, `docs/x.md`, `src/y.rs`,
/// `zz.txt`. Two root-level files on either side of the directory names is what makes the two a
/// genuinely different cycle rather than a rotation of each other.
fn sibling_dirs_fixture() -> (TempDir, TreeModel, BTreeMap<PathBuf, Status>) {
    let dir = TempDir::new();
    for sub in ["docs", "src"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    let files = ["Cargo.toml", "docs/x.md", "src/y.rs", "zz.txt"];
    for f in files {
        fs::write(dir.path().join(f), "x").unwrap();
    }
    let set = changed(&files);
    let mut model = TreeModel::new(dir.path());
    model.set_status(&set);
    model.expand(&dir.path().join("docs"));
    model.expand(&dir.path().join("src"));
    assert_eq!(
        rows(&model),
        ["docs", "x.md", "src", "y.rs", "Cargo.toml", "zz.txt"],
        "precondition: the root-level files render last, but `Cargo.toml` sorts first as a key"
    );
    (dir, model, set)
}

#[test]
fn jump_visits_the_changed_files_in_rendered_row_order() {
    let (_dir, mut model, set) = sibling_dirs_fixture();
    model.set_cursor(0); // the `docs` directory row, above every candidate

    let mut forward = Vec::new();
    for _ in 0..4 {
        model.select_changed(true, &set);
        forward.push(selected(&model).unwrap());
    }
    assert_eq!(
        forward,
        ["x.md", "y.rs", "Cargo.toml", "zz.txt"],
        "top-to-bottom as rendered, not the key order that puts Cargo.toml before the directories"
    );

    let mut backward = Vec::new();
    for _ in 0..4 {
        model.select_changed(false, &set);
        backward.push(selected(&model).unwrap());
    }
    assert_eq!(
        backward,
        ["Cargo.toml", "y.rs", "x.md", "zz.txt"],
        "the same walk in reverse, wrapping off the top row onto the bottom one"
    );
}

#[test]
fn wrap_is_reported_only_at_the_real_first_and_last_rows() {
    let (_dir, mut model, set) = sibling_dirs_fixture();
    model.set_cursor(0);

    assert_eq!(model.select_changed(true, &set), Some(false)); // → x.md
    assert_eq!(model.select_changed(true, &set), Some(false)); // → y.rs
    assert_eq!(
        model.select_changed(true, &set),
        Some(false),
        "Cargo.toml sorts FIRST as a key but is not the first row — no wrap here"
    );
    assert_eq!(selected(&model).as_deref(), Some("Cargo.toml"));
    assert_eq!(
        model.select_changed(true, &set),
        Some(false),
        "landing on the last rendered row is still not a wrap"
    );
    assert_eq!(selected(&model).as_deref(), Some("zz.txt"));

    assert_eq!(
        model.select_changed(true, &set),
        Some(true),
        "stepping off the last row onto the first is the one forward wrap"
    );
    assert_eq!(selected(&model).as_deref(), Some("x.md"));

    assert_eq!(
        model.select_changed(false, &set),
        Some(true),
        "and backward off the first row onto the last is its mirror"
    );
    assert_eq!(selected(&model).as_deref(), Some("zz.txt"));
    assert_eq!(model.select_changed(false, &set), Some(false));
    assert_eq!(selected(&model).as_deref(), Some("Cargo.toml"));
}

#[test]
fn jump_comparator_agrees_with_the_tree_s_rendered_order() {
    // Drift guard. The jump's traversal order and the rendered rows are both built on the tree's
    // one sibling rule; if a future change to how the tree orders rows does not reach the
    // comparator, it fails HERE instead of silently mis-ordering the jump.
    let dir = TempDir::new();
    let subs = ["Zed", "docs", "docs/deep", "src"];
    for sub in subs {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    let all = [
        "Cargo.toml",
        "readme.md",
        "Zed/z.txt",
        "docs/x.md",
        "docs/deep/d.md",
        "src/y.rs",
    ];
    for f in all {
        fs::write(dir.path().join(f), "x").unwrap();
    }
    let set = changed(&all);
    let mut model = TreeModel::new(dir.path());
    model.set_status(&set);
    for sub in subs {
        model.expand(&dir.path().join(sub));
    }

    // The file rows top-to-bottom, as the tree actually renders them.
    let rendered: Vec<PathBuf> = model
        .visible_nodes()
        .iter()
        .filter(|n| n.kind == NodeKind::File)
        .map(|n| n.path.strip_prefix(dir.path()).unwrap().to_path_buf())
        .collect();
    assert_eq!(
        rendered.len(),
        all.len(),
        "precondition: every file renders"
    );

    // Sorting those paths with the jump's comparator must reproduce that order exactly.
    let mut sorted = rendered.clone();
    sorted.reverse(); // start from the worst case, not the answer
    sorted.sort_by(|a, b| cmp_file_rows(a, b));
    assert_eq!(sorted, rendered, "full tree");

    // The changed-only filter synthesizes its rows from the changed-set instead of the
    // filesystem — a second render path, and it must order the same way.
    model.set_changed_only(true, &set);
    let filtered: Vec<PathBuf> = model
        .visible_nodes()
        .iter()
        .filter(|n| n.kind == NodeKind::File)
        .map(|n| n.path.strip_prefix(dir.path()).unwrap().to_path_buf())
        .collect();
    assert_eq!(filtered, rendered, "changed-only tree");
}

#[test]
fn jump_from_an_unchanged_file_goes_to_the_next_changed_one_in_path_order() {
    // The cursor is usually NOT on a changed file. The insertion point in the changed-set's
    // order still gives a well-defined next/previous neighbour.
    let dir = TempDir::new();
    for name in ["a.rs", "b_clean.rs", "c.rs"] {
        fs::write(dir.path().join(name), "x").unwrap();
    }
    let set = changed(&["a.rs", "c.rs"]);
    let mut model = TreeModel::new(dir.path());
    model.set_status(&set);

    // Park the cursor on the clean file that sorts between the two changed ones.
    let idx = model
        .visible_nodes()
        .iter()
        .position(|n| n.path.ends_with("b_clean.rs"))
        .unwrap();
    model.set_cursor(idx);

    assert_eq!(model.select_changed(true, &set), Some(false));
    assert_eq!(selected(&model).as_deref(), Some("c.rs"), "forward");

    model.set_cursor(idx);
    assert_eq!(model.select_changed(false, &set), Some(false));
    assert_eq!(selected(&model).as_deref(), Some("a.rs"), "backward");
}
