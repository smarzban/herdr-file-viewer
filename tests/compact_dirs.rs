//! Compact directory chains (`compact_dirs`): a run of single-child directories drawn as ONE row,
//! in both the full tree and the changed-only view. Off by default, so the uncompacted shape is
//! pinned here too. Read-only: compaction only groups and labels rows (AC-N1).

mod common;

use common::TempDir;
use herdr_file_viewer::git::Status;
use herdr_file_viewer::tree::{NodeKind, TreeModel};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Each visible row as it is displayed: the label when the row carries one (a compacted chain),
/// else the path's final component — the same choice the Presenter makes.
fn rows(model: &TreeModel) -> Vec<String> {
    model
        .visible_nodes()
        .iter()
        .map(|n| {
            n.label
                .clone()
                .unwrap_or_else(|| n.path.file_name().unwrap().to_string_lossy().into_owned())
        })
        .collect()
}

/// A `src/main/java` chain with one file at the bottom, plus a shallow file at the root.
fn deep_repo() -> TempDir {
    let dir = TempDir::new();
    let deep = dir.path().join("src/main/java");
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("App.java"), "x").unwrap();
    fs::write(dir.path().join("README.md"), "x").unwrap();
    dir
}

#[test]
fn off_by_default_the_chain_is_one_row_per_segment() {
    let dir = deep_repo();
    let mut model = TreeModel::new(dir.path());
    model.expand(&dir.path().join("src"));
    model.expand(&dir.path().join("src/main"));
    model.expand(&dir.path().join("src/main/java"));

    assert_eq!(
        rows(&model),
        vec!["src", "main", "java", "App.java", "README.md"],
        "the default tree shape is unchanged"
    );
    assert!(
        model.visible_nodes().iter().all(|n| n.label.is_none()),
        "no row carries a label when compaction is off"
    );
}

#[test]
fn a_single_child_chain_becomes_one_row_leading_into_the_deepest_directory() {
    let dir = deep_repo();
    let mut model = TreeModel::new(dir.path());
    model.set_compact_dirs(true);

    assert_eq!(
        rows(&model),
        vec!["src/main/java", "README.md"],
        "three collapsed rows and their indentation collapse into one"
    );

    let chain = model
        .visible_nodes()
        .into_iter()
        .find(|n| n.label.is_some())
        .unwrap();
    assert_eq!(chain.kind, NodeKind::Dir);
    assert_eq!(
        chain.path,
        dir.path().join("src/main/java"),
        "the row's path is the DEEPEST directory, so expand/collapse acts on it"
    );
    assert_eq!(chain.depth, 0, "the folded row keeps the chain's own depth");
}

#[test]
fn expanding_a_compacted_row_shows_the_deepest_directorys_children() {
    let dir = deep_repo();
    let mut model = TreeModel::new(dir.path());
    model.set_compact_dirs(true);

    let chain = model
        .visible_nodes()
        .into_iter()
        .find(|n| n.label.is_some())
        .unwrap();
    model.expand(&chain.path);

    assert_eq!(
        rows(&model),
        vec!["src/main/java", "App.java", "README.md"],
        "the file under the deepest directory appears one level in"
    );
}

#[test]
fn a_chain_stops_at_a_directory_that_holds_a_file() {
    // `src` holds `main` AND a file, so there is nothing to fold at the top; `main/java` still
    // folds below it.
    let dir = TempDir::new();
    let deep = dir.path().join("src/main/java");
    fs::create_dir_all(&deep).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "x").unwrap();
    fs::write(deep.join("App.java"), "x").unwrap();

    let mut model = TreeModel::new(dir.path());
    model.set_compact_dirs(true);
    model.expand(&dir.path().join("src"));

    assert_eq!(rows(&model), vec!["src", "main/java", "lib.rs"]);
}

#[test]
fn a_chain_stops_at_a_directory_with_two_subdirectories() {
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("src/main/java")).unwrap();
    fs::create_dir_all(dir.path().join("src/test")).unwrap();

    let mut model = TreeModel::new(dir.path());
    model.set_compact_dirs(true);
    model.expand(&dir.path().join("src"));

    assert_eq!(
        rows(&model),
        vec!["src", "main/java", "test"],
        "a branch point ends the chain; each branch folds on its own"
    );
}

#[test]
fn a_chain_folds_over_entries_the_tree_is_not_showing() {
    // Compaction follows what is VISIBLE: `src`'s only shown entry is `main`, because the sibling
    // is gitignored — so it folds like the single-child directory it appears to be. Revealing
    // ignored files with `i` un-folds it again.
    let dir = TempDir::new();
    fs::write(dir.path().join(".gitignore"), "src/target\n").unwrap();
    fs::create_dir_all(dir.path().join("src/main")).unwrap();
    fs::create_dir_all(dir.path().join("src/target")).unwrap();

    let mut model = TreeModel::new(dir.path());
    model.set_compact_dirs(true);
    assert!(
        rows(&model).contains(&"src/main".to_string()),
        "folds past the ignored sibling: {:?}",
        rows(&model)
    );

    model.set_show_ignored(true);
    model.expand(&dir.path().join("src"));
    let shown = rows(&model);
    assert!(
        shown.contains(&"src".to_string()) && shown.contains(&"target".to_string()),
        "revealing the ignored sibling ends the chain: {shown:?}"
    );
}

#[test]
fn changed_only_folds_the_chain_too() {
    // The case the feature exists for: a changed-set tree is all path, so an uncompacted
    // `src/main/java/br/com/App.java` costs six rows before the file name.
    let dir = TempDir::new();
    let deep = dir.path().join("src/main/java/br/com");
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("App.java"), "x").unwrap();

    let mut changed = BTreeMap::new();
    changed.insert(
        PathBuf::from("src/main/java/br/com/App.java"),
        Status::Modified,
    );
    let mut model = TreeModel::new(dir.path());
    model.set_status(&changed);
    model.set_changed_only(true, &changed);
    model.set_compact_dirs(true);

    assert_eq!(
        rows(&model),
        vec!["src/main/java/br/com", "App.java"],
        "one row of path, then the file"
    );

    model.set_compact_dirs(false);
    assert_eq!(
        rows(&model),
        vec!["src", "main", "java", "br", "com", "App.java"],
        "and the uncompacted view is unchanged"
    );
}

#[test]
fn changed_only_chain_stops_at_a_directory_holding_a_changed_file() {
    // `src/main` holds a changed file of its own, so folding must stop there — otherwise the row
    // would lead past a file the user needs to see.
    let dir = TempDir::new();
    let deep = dir.path().join("src/main/java");
    fs::create_dir_all(&deep).unwrap();
    fs::write(dir.path().join("src/main/Local.java"), "x").unwrap();
    fs::write(deep.join("App.java"), "x").unwrap();

    let mut changed = BTreeMap::new();
    changed.insert(PathBuf::from("src/main/Local.java"), Status::Modified);
    changed.insert(PathBuf::from("src/main/java/App.java"), Status::Modified);
    let mut model = TreeModel::new(dir.path());
    model.set_status(&changed);
    model.set_changed_only(true, &changed);
    model.set_compact_dirs(true);

    assert_eq!(
        rows(&model),
        vec!["src/main", "java", "App.java", "Local.java"],
        "the chain ends where a changed file lives"
    );
}
