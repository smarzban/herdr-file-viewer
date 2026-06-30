//! Tree filters: show-ignored toggle (AC-5) + changed-only filter (AC-6),
//! plus status markers on nodes (AC-7, tree side).

mod common;

use common::TempDir;
use herdr_file_viewer::git::Status;
use herdr_file_viewer::tree::TreeModel;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

fn names(model: &TreeModel) -> Vec<String> {
    model
        .visible_nodes()
        .iter()
        .map(|n| n.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn show_ignored_toggle_reveals_then_hides_ignored_files() {
    let dir = TempDir::new();
    fs::write(dir.path().join(".gitignore"), "secret.log\n").unwrap();
    fs::write(dir.path().join("keep.txt"), "k").unwrap();
    fs::write(dir.path().join("secret.log"), "s").unwrap();

    let mut model = TreeModel::new(dir.path());
    assert!(!names(&model).contains(&"secret.log".to_string()));
    model.set_show_ignored(true);
    assert!(
        names(&model).contains(&"secret.log".to_string()),
        "AC-5: revealed"
    );
    model.set_show_ignored(false);
    assert!(
        !names(&model).contains(&"secret.log".to_string()),
        "AC-5: restored"
    );
}

#[test]
fn hide_hidden_toggle_hides_then_reveals_dotfiles() {
    // #46: a toggle to hide dot-prefixed files/folders (like opening $HOME, flooded with them),
    // mirroring the show-ignored toggle. By default dotfiles SHOW (so .gitignore / .github stay
    // browsable); toggling hide_hidden drops every `.`-prefixed entry, regular files remain.
    let dir = TempDir::new();
    fs::write(dir.path().join(".gitignore"), "\n").unwrap(); // empty: ignores nothing, shows itself
    fs::write(dir.path().join(".env"), "x").unwrap();
    fs::create_dir_all(dir.path().join(".config")).unwrap();
    fs::write(dir.path().join("keep.txt"), "k").unwrap();

    let mut model = TreeModel::new(dir.path());
    // Default: dotfiles are visible (current behavior, preserved).
    let n = names(&model);
    assert!(
        n.contains(&".gitignore".to_string()),
        "dotfiles show by default"
    );
    assert!(n.contains(&".env".to_string()));
    assert!(n.contains(&".config".to_string()));
    assert!(n.contains(&"keep.txt".to_string()));

    // Toggle ON: dot-prefixed files AND folders are hidden; regular files remain.
    model.set_hide_hidden(true);
    let n = names(&model);
    assert!(
        !n.contains(&".gitignore".to_string()),
        "#46: dotfile hidden"
    );
    assert!(!n.contains(&".env".to_string()), "#46: dotfile hidden");
    assert!(
        !n.contains(&".config".to_string()),
        "#46: dot-folder hidden"
    );
    assert!(
        n.contains(&"keep.txt".to_string()),
        "regular files still shown"
    );

    // Toggle OFF: restored.
    model.set_hide_hidden(false);
    assert!(
        names(&model).contains(&".gitignore".to_string()),
        "#46: dotfiles restored when toggled back off"
    );
}

#[test]
fn changed_only_restricts_then_restores_full_tree() {
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("README.md"), "r").unwrap();
    fs::write(dir.path().join("src/changed.rs"), "c").unwrap();
    fs::write(dir.path().join("src/unchanged.rs"), "u").unwrap();
    let mut changed = BTreeMap::new();
    changed.insert(PathBuf::from("src/changed.rs"), Status::Modified);

    let mut model = TreeModel::new(dir.path());
    model.set_changed_only(true, &changed);
    let n = names(&model);
    assert!(
        n.contains(&"src".to_string()),
        "ancestor dir of a change is shown"
    );
    assert!(
        n.contains(&"changed.rs".to_string()),
        "AC-6: changed file shown"
    );
    assert!(
        !n.contains(&"unchanged.rs".to_string()),
        "AC-6: unchanged sibling hidden"
    );
    assert!(
        !n.contains(&"README.md".to_string()),
        "AC-6: unchanged top-level file hidden"
    );

    model.set_changed_only(false, &changed);
    let restored = names(&model);
    assert!(
        restored.contains(&"README.md".to_string()),
        "AC-6: full tree restored"
    );
    assert!(restored.contains(&"src".to_string()));
}

#[test]
fn changed_only_shows_deleted_files_with_their_marker() {
    // A deleted file is no longer on disk; it must still appear in changed-only mode so
    // the deletion can be reviewed (AC-6) with a deleted marker (AC-7).
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/present.rs"), "x").unwrap();
    // src/gone.rs is intentionally NOT created.
    let mut changed = BTreeMap::new();
    changed.insert(PathBuf::from("src/gone.rs"), Status::Deleted);
    changed.insert(PathBuf::from("src/present.rs"), Status::Modified);

    let mut model = TreeModel::new(dir.path());
    model.set_status(&changed);
    model.set_changed_only(true, &changed);

    let gone = model
        .visible_nodes()
        .into_iter()
        .find(|n| n.path.ends_with("gone.rs"));
    let gone = gone.expect("AC-6: deleted file appears in changed-only mode");
    assert_eq!(gone.status, Some(Status::Deleted)); // AC-7
}

#[test]
fn changed_only_shows_files_under_a_deleted_directory() {
    // A whole directory was deleted: none of its files (nor the dir) are on disk, but the
    // changed-set still references them — they must be reviewable.
    let dir = TempDir::new();
    let mut changed = BTreeMap::new();
    changed.insert(PathBuf::from("old/a.rs"), Status::Deleted);
    changed.insert(PathBuf::from("old/sub/b.rs"), Status::Deleted);

    let mut model = TreeModel::new(dir.path());
    model.set_changed_only(true, &changed);

    let names: Vec<String> = model
        .visible_nodes()
        .iter()
        .map(|n| n.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.contains(&"old".to_string()),
        "the deleted directory is synthesized"
    );
    assert!(names.contains(&"a.rs".to_string()));
    assert!(
        names.contains(&"b.rs".to_string()),
        "files under a deleted dir are shown"
    );
}

#[test]
fn cursor_moves_and_stays_in_bounds_when_filters_shrink_the_list() {
    let dir = TempDir::new();
    fs::write(dir.path().join("a.txt"), "a").unwrap();
    fs::write(dir.path().join("b.txt"), "b").unwrap();
    fs::write(dir.path().join("c.txt"), "c").unwrap();
    let mut model = TreeModel::new(dir.path());

    model.move_cursor(2); // to the 3rd node
    assert_eq!(model.cursor(), 2);
    model.move_cursor(100); // clamped to last
    assert_eq!(model.cursor(), 2);
    model.move_cursor(-100); // clamped to first
    assert_eq!(model.cursor(), 0);

    model.move_cursor(2);
    // Filter down to a single changed file → cursor must clamp into range.
    let mut changed = BTreeMap::new();
    changed.insert(PathBuf::from("a.txt"), Status::Modified);
    model.set_changed_only(true, &changed);
    assert!(
        model.cursor() < model.visible_nodes().len(),
        "cursor clamped after filtering"
    );
    assert!(model.selected().is_some());
}

#[test]
fn status_markers_attach_to_nodes() {
    let dir = TempDir::new();
    fs::write(dir.path().join("m.txt"), "m").unwrap();
    let mut map = BTreeMap::new();
    map.insert(PathBuf::from("m.txt"), Status::Modified);

    let mut model = TreeModel::new(dir.path());
    model.set_status(&map);
    let node = model
        .visible_nodes()
        .into_iter()
        .find(|n| n.path.ends_with("m.txt"))
        .unwrap();
    assert_eq!(node.status, Some(Status::Modified)); // AC-7 (tree side)
}
