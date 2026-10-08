//! Tree Model: gitignore-aware recursive enumeration + root boundary
//! (AC-3, AC-4, AC-N5, AC-N1).
//!
//! Tree Model `reveal(path)` (AC-10, AC-20, AC-N5).

mod common;

use common::TempDir;
use herdr_file_viewer::git::Status;
use herdr_file_viewer::tree::TreeModel;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[test]
fn a_directory_with_a_changed_descendant_is_flagged_dirty() {
    // Folder coloring needs an aggregate: a directory is "dirty" when any file under it has a
    // git status (so the Presenter can color it), while a clean directory is not.
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("changed")).unwrap();
    fs::create_dir_all(dir.path().join("clean")).unwrap();
    fs::write(dir.path().join("changed/a.rs"), "a").unwrap();
    fs::write(dir.path().join("clean/b.rs"), "b").unwrap();

    let mut model = TreeModel::new(dir.path());
    let mut status = BTreeMap::new();
    status.insert(PathBuf::from("changed/a.rs"), Status::Modified);
    model.set_status(&status);

    let nodes = model.visible_nodes();
    let find = |name: &str| {
        nodes
            .iter()
            .find(|n| n.path.file_name().unwrap() == name)
            .expect("node present")
    };
    assert!(
        find("changed").dir_dirty,
        "a directory with a modified descendant is dirty"
    );
    assert!(
        !find("clean").dir_dirty,
        "a directory with no changes is not dirty"
    );
}

fn names(model: &TreeModel) -> Vec<String> {
    model
        .visible_nodes()
        .iter()
        .map(|n| n.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn enumerates_immediate_children_and_expands_recursively() {
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("src/inner")).unwrap();
    fs::write(dir.path().join("a.txt"), "a").unwrap();
    fs::write(dir.path().join("src/b.rs"), "b").unwrap();
    fs::write(dir.path().join("src/inner/c.rs"), "c").unwrap();

    let mut model = TreeModel::new(dir.path());
    let top = names(&model);
    assert!(top.contains(&"src".to_string()));
    assert!(top.contains(&"a.txt".to_string()));
    assert!(
        !top.contains(&"b.rs".to_string()),
        "nested files hidden until expand"
    );

    model.expand(&dir.path().join("src")); // AC-3
    let after = names(&model);
    assert!(after.contains(&"b.rs".to_string()));
    assert!(
        !after.contains(&"c.rs".to_string()),
        "deeper level still collapsed"
    );

    model.expand(&dir.path().join("src/inner"));
    assert!(names(&model).contains(&"c.rs".to_string()));
}

#[test]
fn gitignored_entries_are_absent_by_default() {
    let dir = TempDir::new();
    fs::write(dir.path().join(".gitignore"), "ignored.txt\ntarget/\n").unwrap();
    fs::write(dir.path().join("kept.txt"), "k").unwrap();
    fs::write(dir.path().join("ignored.txt"), "i").unwrap();
    fs::create_dir_all(dir.path().join("target")).unwrap();
    fs::write(dir.path().join("target/x.o"), "o").unwrap();

    let n = names(&TreeModel::new(dir.path()));
    assert!(n.contains(&"kept.txt".to_string()));
    assert!(
        !n.contains(&"ignored.txt".to_string()),
        "AC-4: gitignored file absent"
    );
    assert!(
        !n.contains(&"target".to_string()),
        "AC-4: gitignored dir absent"
    );
}

#[test]
fn never_lists_paths_outside_the_root() {
    // AC-N5: bounded by the root; no node escapes it.
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("sub/f.txt"), "f").unwrap();
    let root = dir.path().join("sub");
    let mut model = TreeModel::new(&root);
    model.expand(&root);
    assert!(
        model
            .visible_nodes()
            .iter()
            .all(|n| n.path.starts_with(&root)),
        "a node escaped the root"
    );
}

#[test]
fn enumeration_does_not_mutate_the_filesystem() {
    // AC-N1: read-only.
    let dir = TempDir::new();
    fs::write(dir.path().join("f.txt"), "x").unwrap();
    let before = fs::read_dir(dir.path()).unwrap().count();
    let _ = TreeModel::new(dir.path()).visible_nodes();
    assert_eq!(before, fs::read_dir(dir.path()).unwrap().count());
    assert_eq!(fs::read_to_string(dir.path().join("f.txt")).unwrap(), "x");
}

#[test]
fn dotfiles_are_browsable_but_dot_git_is_hidden() {
    let dir = TempDir::new();
    fs::write(dir.path().join(".env"), "x").unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    fs::write(dir.path().join(".git/HEAD"), "ref").unwrap();
    let n = names(&TreeModel::new(dir.path()));
    assert!(n.contains(&".env".to_string()), "dotfiles are browsable");
    assert!(!n.contains(&".git".to_string()), ".git is hidden");
}

// ── reveal(path) ────────────────────────────────────────────────────────

/// (a) Collapse all, reveal a deep file → ancestors expanded and target selected (AC-10).
#[test]
fn reveal_expands_ancestors_and_selects_target() {
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("src/inner")).unwrap();
    fs::write(dir.path().join("src/inner/deep.rs"), "x").unwrap();
    fs::write(dir.path().join("src/top.rs"), "y").unwrap();

    let mut model = TreeModel::new(dir.path());
    // Sanity: deep.rs is NOT visible before reveal (all collapsed).
    let before_names = names(&model);
    assert!(
        !before_names.contains(&"deep.rs".to_string()),
        "deep.rs should be hidden before reveal"
    );

    let target = dir.path().join("src/inner/deep.rs");
    let ok = model.reveal(&target);
    assert!(ok, "reveal returned false unexpectedly");

    let selected = model
        .selected()
        .expect("a node must be selected after reveal");
    assert_eq!(
        selected.path, target,
        "selected node must be the revealed file"
    );

    let visible: Vec<_> = model
        .visible_nodes()
        .into_iter()
        .map(|n| n.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(
        visible.contains(&"inner".to_string()),
        "inner/ must be visible after reveal"
    );
    assert!(
        visible.contains(&"deep.rs".to_string()),
        "deep.rs must be visible after reveal"
    );
}

/// (b) `changed_only` ON, target NOT in changed set → `reveal` clears `changed_only` and selects
/// the file (filter-relax, AC-10).
#[test]
fn reveal_clears_changed_only_when_target_hidden_by_filter() {
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("sub/target.rs"), "t").unwrap();
    fs::write(dir.path().join("sub/changed.rs"), "c").unwrap();

    let mut model = TreeModel::new(dir.path());
    let mut status = BTreeMap::new();
    // Only changed.rs is in the changed-set; target.rs is not.
    status.insert(PathBuf::from("sub/changed.rs"), Status::Modified);
    model.set_changed_only(true, &status);

    let target = dir.path().join("sub/target.rs");
    let ok = model.reveal(&target);
    assert!(ok, "reveal returned false unexpectedly");

    let selected = model
        .selected()
        .expect("a node must be selected after reveal");
    assert_eq!(
        selected.path, target,
        "selected node must be the revealed file"
    );

    // The filter must have been relaxed.
    let visible: Vec<_> = model
        .visible_nodes()
        .into_iter()
        .map(|n| n.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(
        visible.contains(&"target.rs".to_string()),
        "target.rs must be visible after filter relaxation"
    );
}

/// (c2) `show_ignored` OFF, gitignored target → `reveal` turns show_ignored on and selects it
/// (explicit path intent, same as hide_hidden / changed_only relaxation).
#[test]
fn reveal_enables_show_ignored_when_target_is_gitignored() {
    let dir = TempDir::new();
    // Minimal git repo so the ignore crate honors .gitignore.
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    fs::write(dir.path().join(".gitignore"), "secret.log\n").unwrap();
    fs::write(dir.path().join("secret.log"), "ignored\n").unwrap();
    fs::write(dir.path().join("visible.rs"), "v\n").unwrap();

    let mut model = TreeModel::new(dir.path());
    assert!(
        !names(&model).contains(&"secret.log".to_string()),
        "gitignored file hidden by default"
    );

    let target = dir.path().join("secret.log");
    let ok = model.reveal(&target);
    assert!(ok, "reveal of gitignored file must succeed");
    assert!(
        model.show_ignored(),
        "show_ignored must relax for explicit path"
    );
    let selected = model.selected().expect("selected after reveal");
    assert_eq!(selected.path, target);
}

/// (c) `hide_hidden` ON, dot-prefixed target → `reveal` clears `hide_hidden` and selects it.
#[test]
fn reveal_clears_hide_hidden_when_target_is_dotfile() {
    let dir = TempDir::new();
    fs::write(dir.path().join(".secret"), "s").unwrap();
    fs::write(dir.path().join("visible.rs"), "v").unwrap();

    let mut model = TreeModel::new(dir.path());
    model.set_hide_hidden(true);

    // Sanity: .secret is invisible.
    let before = names(&model);
    assert!(
        !before.contains(&".secret".to_string()),
        ".secret should be hidden before reveal"
    );

    let target = dir.path().join(".secret");
    let ok = model.reveal(&target);
    assert!(ok, "reveal returned false unexpectedly");

    let selected = model
        .selected()
        .expect("a node must be selected after reveal");
    assert_eq!(
        selected.path, target,
        "selected node must be the dot-prefixed file"
    );
}

/// (d) `reveal` of a nonexistent / above-root path returns `false` and leaves cursor unchanged
/// (AC-20, AC-N5).
#[test]
fn reveal_returns_false_for_missing_or_above_root_path() {
    let dir = TempDir::new();
    fs::write(dir.path().join("a.txt"), "a").unwrap();
    fs::write(dir.path().join("b.txt"), "b").unwrap();

    let mut model = TreeModel::new(dir.path());
    // Move cursor to index 1 (b.txt, since a.txt sorts first).
    model.move_cursor(1);
    let cursor_before = model.cursor();

    // Nonexistent path under root.
    let missing = dir.path().join("no_such_file.txt");
    let ok = model.reveal(&missing);
    assert!(!ok, "reveal of nonexistent path must return false");
    assert_eq!(
        model.cursor(),
        cursor_before,
        "cursor must be unchanged after failed reveal"
    );

    // Path above root (parent of the root).
    let above_root = dir.path().parent().unwrap().join("escaped.txt");
    let ok2 = model.reveal(&above_root);
    assert!(!ok2, "reveal of above-root path must return false");
    assert_eq!(
        model.cursor(),
        cursor_before,
        "cursor must be unchanged after above-root reveal"
    );
}

fn selected_name(model: &TreeModel) -> String {
    model
        .selected()
        .expect("a selection")
        .path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn a_file_created_above_the_selection_keeps_it_selected() {
    let dir = TempDir::new();
    fs::write(dir.path().join("b.txt"), "b").unwrap();
    fs::write(dir.path().join("c.txt"), "c").unwrap();
    let mut model = TreeModel::new(dir.path());
    model.move_cursor(1);
    assert_eq!(selected_name(&model), "c.txt");

    fs::write(dir.path().join("a.txt"), "a").unwrap(); // sorts above both
    assert_eq!(
        selected_name(&model),
        "c.txt",
        "the selection follows the file, not the row"
    );
    assert_eq!(model.cursor(), 2, "the row is re-derived from the path");
}

#[test]
fn a_deleted_selection_falls_back_to_the_row_it_was_on() {
    let dir = TempDir::new();
    for f in ["a.txt", "b.txt", "c.txt"] {
        fs::write(dir.path().join(f), f).unwrap();
    }
    let mut model = TreeModel::new(dir.path());
    model.move_cursor(1);
    assert_eq!(selected_name(&model), "b.txt");

    fs::remove_file(dir.path().join("b.txt")).unwrap();
    assert_eq!(selected_name(&model), "c.txt", "row 1 is now c.txt");
}

#[test]
fn a_deleted_last_row_clamps_and_the_neighbor_becomes_the_selection() {
    let dir = TempDir::new();
    for f in ["a.txt", "b.txt", "c.txt"] {
        fs::write(dir.path().join(f), f).unwrap();
    }
    let mut model = TreeModel::new(dir.path());
    model.move_cursor(2);
    fs::remove_file(dir.path().join("c.txt")).unwrap();
    assert_eq!(selected_name(&model), "b.txt", "clamped to the last row");

    // The neighbour is now the selection by path, so a file appearing above keeps it.
    fs::write(dir.path().join("aa.txt"), "aa").unwrap();
    assert_eq!(selected_name(&model), "b.txt");
}

#[test]
fn an_emptied_tree_has_no_selection_and_recovers() {
    let dir = TempDir::new();
    fs::write(dir.path().join("a.txt"), "a").unwrap();
    let model = TreeModel::new(dir.path());
    assert_eq!(selected_name(&model), "a.txt");
    fs::remove_file(dir.path().join("a.txt")).unwrap();
    assert!(model.selected().is_none());
    fs::write(dir.path().join("z.txt"), "z").unwrap();
    assert_eq!(selected_name(&model), "z.txt");
}

#[test]
fn a_filter_toggle_keeps_the_selected_path() {
    let dir = TempDir::new();
    fs::write(dir.path().join(".gitignore"), "a.log\n").unwrap();
    fs::write(dir.path().join("a.log"), "x").unwrap();
    fs::write(dir.path().join("b.txt"), "b").unwrap();
    let mut model = TreeModel::new(dir.path());
    assert!(model.reveal(&dir.path().join("b.txt")));

    model.set_show_ignored(true); // a.log appears above b.txt
    assert_eq!(selected_name(&model), "b.txt");
}

// ── collapse_all() ──────────────────────────────────────────────────────

#[test]
fn collapse_all_closes_every_folder_and_selects_the_top_level_ancestor() {
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("src/inner")).unwrap();
    fs::create_dir_all(dir.path().join("docs")).unwrap();
    fs::write(dir.path().join("src/inner/deep.rs"), "x").unwrap();
    fs::write(dir.path().join("docs/a.md"), "y").unwrap();
    fs::write(dir.path().join("z.txt"), "z").unwrap();

    let mut model = TreeModel::new(dir.path());
    model.expand(&dir.path().join("docs"));
    assert!(model.reveal(&dir.path().join("src/inner/deep.rs")));

    model.collapse_all();

    assert_eq!(names(&model), ["docs", "src", "z.txt"]);
    assert_eq!(selected_name(&model), "src");
    // Re-opening a folder shows its children collapsed: nothing deeper stayed open.
    model.expand(&dir.path().join("src"));
    assert_eq!(names(&model), ["docs", "src", "inner", "z.txt"]);
}

#[test]
fn collapse_all_keeps_a_top_level_selection() {
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/a.rs"), "x").unwrap();
    fs::write(dir.path().join("z.txt"), "z").unwrap();

    let mut model = TreeModel::new(dir.path());
    model.expand(&dir.path().join("src"));
    assert!(model.reveal(&dir.path().join("z.txt")));

    model.collapse_all();

    assert_eq!(names(&model), ["src", "z.txt"]);
    assert_eq!(selected_name(&model), "z.txt");
}

#[test]
fn collapse_all_selects_a_compacted_top_level_row() {
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
    fs::write(dir.path().join("a/b/c/f.rs"), "x").unwrap();

    let mut model = TreeModel::new(dir.path());
    model.set_compact_dirs(true);
    assert!(model.reveal(&dir.path().join("a/b/c/f.rs")));

    model.collapse_all();

    assert_eq!(model.visible_nodes().len(), 1);
    assert_eq!(model.selected().unwrap().path, dir.path().join("a/b/c"));
}
