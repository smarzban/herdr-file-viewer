//! T-6 — Tree Model: gitignore-aware recursive enumeration + root boundary
//! (AC-3, AC-4, AC-N5, AC-N1).

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
        nodes.iter().find(|n| n.path.file_name().unwrap() == name).expect("node present")
    };
    assert!(find("changed").dir_dirty, "a directory with a modified descendant is dirty");
    assert!(!find("clean").dir_dirty, "a directory with no changes is not dirty");
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
    assert!(!top.contains(&"b.rs".to_string()), "nested files hidden until expand");

    model.expand(&dir.path().join("src")); // AC-3
    let after = names(&model);
    assert!(after.contains(&"b.rs".to_string()));
    assert!(!after.contains(&"c.rs".to_string()), "deeper level still collapsed");

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
    assert!(!n.contains(&"ignored.txt".to_string()), "AC-4: gitignored file absent");
    assert!(!n.contains(&"target".to_string()), "AC-4: gitignored dir absent");
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
        model.visible_nodes().iter().all(|n| n.path.starts_with(&root)),
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
