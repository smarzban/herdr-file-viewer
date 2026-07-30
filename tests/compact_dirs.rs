//! Compact directory chains (`compact_dirs`): a run of single-child directories drawn as ONE row,
//! in both the full tree and the changed-only view. Off by default, so the uncompacted shape is
//! pinned here too. Read-only: compaction only groups and labels rows (AC-N1).
//!
//! Two invariants beyond the shape itself get their own sections below: that folding never
//! reorders siblings — which is what lets the `]` / `[` changed-file jump agree with a compacted
//! tree without knowing compaction exists — and the walk discipline, that drawing a frame does no
//! more filesystem walking than the uncompacted tree does.

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

// ---- compaction and the `]` / `[` changed-file jump ------------------------------------------

/// The file rows top-to-bottom, root-relative — the order the jump has to agree with.
fn file_rows(model: &TreeModel) -> Vec<PathBuf> {
    model
        .visible_nodes()
        .iter()
        .filter(|n| n.kind == NodeKind::File)
        .map(|n| n.path.clone())
        .collect()
}

/// The file name the cursor sits on.
fn selected(model: &TreeModel) -> String {
    model
        .selected()
        .map(|n| n.path.file_name().unwrap().to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Two foldable chains plus a root-level file, all three files changed. The changed-set's keys
/// sort `Top.txt`, `a_pkg/…/A.txt`, `b_pkg/…/B.txt` (uppercase first), while the rows render
/// `A.txt`, `B.txt`, `Top.txt` (directories before files) — so key order and row order genuinely
/// differ and a jump that used the wrong one would be caught.
fn jump_fixture() -> (TempDir, BTreeMap<PathBuf, Status>) {
    let dir = TempDir::new();
    for (sub, name) in [("a_pkg/one", "A.txt"), ("b_pkg/two", "B.txt")] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
        fs::write(dir.path().join(sub).join(name), "x").unwrap();
    }
    fs::write(dir.path().join("Top.txt"), "x").unwrap();
    let changed = ["Top.txt", "a_pkg/one/A.txt", "b_pkg/two/B.txt"]
        .iter()
        .map(|p| (PathBuf::from(p), Status::Modified))
        .collect();
    (dir, changed)
}

#[test]
fn compaction_never_reorders_the_rows_the_jump_walks() {
    // The load-bearing invariant behind `]` / `[` agreeing with a compacted tree: folding removes
    // intermediate DIRECTORY rows and relabels one, it never moves a row among its siblings. So the
    // file rows — the only rows the jump can land on — are identical either way, and the jump's
    // comparator needs to know nothing about compaction.
    let (dir, set) = jump_fixture();

    let mut plain = TreeModel::new(dir.path());
    for sub in ["a_pkg", "a_pkg/one", "b_pkg", "b_pkg/two"] {
        plain.expand(&dir.path().join(sub));
    }
    assert_eq!(
        rows(&plain),
        vec!["a_pkg", "one", "A.txt", "b_pkg", "two", "B.txt", "Top.txt"],
        "precondition: uncompacted, one row per segment"
    );

    let mut folded = TreeModel::new(dir.path());
    folded.set_compact_dirs(true);
    folded.expand(&dir.path().join("a_pkg/one"));
    folded.expand(&dir.path().join("b_pkg/two"));
    assert_eq!(
        rows(&folded),
        vec!["a_pkg/one", "A.txt", "b_pkg/two", "B.txt", "Top.txt"],
        "precondition: each chain is one row"
    );

    assert_eq!(
        file_rows(&folded),
        file_rows(&plain),
        "same files, same order — only the directory rows differ"
    );

    // And `]` walks exactly that order, forward and back, in the compacted tree.
    folded.set_status(&set);
    folded.set_cursor(0); // the first chain row, above every candidate
    let mut forward = Vec::new();
    for _ in 0..3 {
        folded.select_changed(true, &set);
        forward.push(selected(&folded));
    }
    assert_eq!(
        forward,
        ["A.txt", "B.txt", "Top.txt"],
        "top-to-bottom as rendered, not the key order that puts Top.txt first"
    );

    let mut backward = Vec::new();
    for _ in 0..3 {
        folded.select_changed(false, &set);
        backward.push(selected(&folded));
    }
    assert_eq!(
        backward,
        ["B.txt", "A.txt", "Top.txt"],
        "the same walk in reverse, wrapping off the top row onto the bottom one"
    );
}

#[test]
fn the_jump_reaches_a_changed_file_under_a_collapsed_compacted_chain() {
    // A folded row is keyed on the DEEPEST directory of its chain, while the jump expands every
    // ancestor of its target. The two have to meet: expanding `a_pkg` and `a_pkg/one` must open the
    // one row that stands for both.
    let (dir, set) = jump_fixture();
    let mut model = TreeModel::new(dir.path());
    model.set_compact_dirs(true);
    model.set_status(&set);
    assert_eq!(
        rows(&model),
        vec!["a_pkg/one", "b_pkg/two", "Top.txt"],
        "precondition: both chains are folded shut"
    );

    model.select_changed(true, &set);
    assert_eq!(selected(&model), "A.txt");
    assert_eq!(
        rows(&model),
        vec!["a_pkg/one", "A.txt", "b_pkg/two", "Top.txt"],
        "the folded row opened, rather than the chain un-folding into its segments"
    );
}

#[test]
fn the_jump_agrees_with_the_compacted_changed_only_tree() {
    // The changed-only view folds over the synthesized set instead of the filesystem — a second
    // render path, and the jump has to agree with it too.
    let (dir, set) = jump_fixture();
    let mut model = TreeModel::new(dir.path());
    model.set_status(&set);
    model.set_changed_only(true, &set);
    model.set_compact_dirs(true);
    assert_eq!(
        rows(&model),
        vec!["a_pkg/one", "A.txt", "b_pkg/two", "B.txt", "Top.txt"],
        "precondition: every directory expanded, each chain one row"
    );

    model.set_cursor(0);
    let mut visited = Vec::new();
    for _ in 0..3 {
        model.select_changed(true, &set);
        visited.push(selected(&model));
    }
    assert_eq!(visited, ["A.txt", "B.txt", "Top.txt"]);
    assert!(
        model.changed_only(),
        "jumping inside the filter must not relax it"
    );
}

// ---- walk discipline ----------------------------------------------------------------------

#[test]
fn a_build_walks_each_directory_once_and_a_rebuild_walks_nothing() {
    // Compaction has to look INSIDE a directory to know whether its row folds — including a
    // COLLAPSED one, which the uncompacted tree never opens. Done naively that is one `ignore` walk
    // per visible directory row on the per-frame `visible_nodes` path: a collapsed root of twenty
    // directories costs twenty-one walks per frame instead of one. The memoized listings are what
    // bound it, and this is the meter.
    let dir = deep_repo();
    let mut model = TreeModel::new(dir.path());
    assert_eq!(
        model.listed_dirs(),
        0,
        "nothing is read before compaction is on"
    );

    // `set_compact_dirs` re-clamps the cursor, which is itself a build — so the first listings are
    // taken here rather than by the `rows` call below.
    model.set_compact_dirs(true);
    assert_eq!(rows(&model), vec!["src/main/java", "README.md"]);
    assert_eq!(
        model.listed_dirs(),
        4,
        "the root plus the three links of the chain — each read once, whatever asked for it"
    );

    for _ in 0..5 {
        let _ = model.visible_nodes();
    }
    assert_eq!(
        model.listed_dirs(),
        4,
        "and every frame after the first walks nothing at all"
    );

    // Cold again, this time with the chain EXPANDED: descending into the deepest directory wants
    // exactly the listing the fold already took, so it is still four walks and not five.
    model.expand(&dir.path().join("src/main/java"));
    model.invalidate_listings();
    assert_eq!(model.listed_dirs(), 0, "invalidation drops the lot");
    assert_eq!(rows(&model), vec!["src/main/java", "App.java", "README.md"]);
    assert_eq!(
        model.listed_dirs(),
        4,
        "the fold and the descent into it share one listing of the deepest directory"
    );
}

#[test]
fn a_compacted_tree_re_reads_the_disk_when_its_listings_are_invalidated() {
    // The other half of the bound: because a rebuild walks nothing, a compacted tree is a coherent
    // SNAPSHOT rather than a live per-frame read, and the controller re-takes it wherever it
    // re-reads git — launch, `r`, editor return, baseline toggle, focus-gain. Both halves are
    // asserted here observationally, which is the strongest statement of "the frame did no
    // filesystem work": a change on disk cannot show up in a build that never looked.
    let dir = deep_repo();
    let mut model = TreeModel::new(dir.path());
    model.set_compact_dirs(true);
    assert_eq!(rows(&model), vec!["src/main/java", "README.md"]);

    // Two changes a rebuild would have to walk to notice: a new root entry (the collect walk) and
    // a new file inside the chain, which ends the fold two segments earlier (the fold's walk, on a
    // collapsed directory).
    fs::create_dir_all(dir.path().join("vendor")).unwrap();
    fs::write(dir.path().join("src/main/Extra.java"), "x").unwrap();
    assert_eq!(
        rows(&model),
        vec!["src/main/java", "README.md"],
        "neither shows: the rebuild read no directory"
    );

    model.invalidate_listings();
    assert_eq!(
        rows(&model),
        vec!["src/main", "vendor", "README.md"],
        "and the next build sees both — the new root entry, and the chain stopping early"
    );
}

#[test]
fn with_compaction_off_the_tree_still_walks_live_on_every_build() {
    // The memo is the price of compaction and is charged to nobody else. With `compact_dirs` off
    // nothing is cached, so the default tree keeps picking up on-disk changes with no refresh —
    // exactly as it did before the feature existed.
    let dir = deep_repo();
    let model = TreeModel::new(dir.path());
    assert_eq!(rows(&model), vec!["src", "README.md"], "precondition");

    fs::create_dir_all(dir.path().join("vendor")).unwrap();
    assert_eq!(
        rows(&model),
        vec!["src", "vendor", "README.md"],
        "the new directory appears with no invalidation"
    );
    assert_eq!(model.listed_dirs(), 0, "and nothing was memoized");
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
