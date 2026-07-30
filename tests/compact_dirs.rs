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
fn a_collapsed_row_costs_a_two_entry_probe_not_a_full_listing() {
    // The shape of the whole design. Compaction asks "does this fold?" of every visible directory
    // row, including COLLAPSED ones the uncompacted tree never opens — but the answer needs at most
    // two entries, so a collapsed row must never materialize its children. Only the directories the
    // tree actually descends into get a full listing, which is exactly the set the uncompacted tree
    // reads on every frame.
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("alpha/one")).unwrap(); // folds: a lone subdirectory
    fs::create_dir_all(dir.path().join("beta")).unwrap(); // empty — no fold
    fs::create_dir_all(dir.path().join("gamma")).unwrap();
    fs::write(dir.path().join("gamma/x.txt"), "x").unwrap(); // holds a file — no fold
    fs::write(dir.path().join("top.txt"), "x").unwrap();

    let mut model = TreeModel::new(dir.path());
    // `set_compact_dirs` re-clamps the cursor, which is one build — so these are that build's
    // reads, taken before anything else asks for the rows.
    model.set_compact_dirs(true);
    assert_eq!(
        model.walk_counts(),
        (1, 4),
        "one build: ONE full listing (the root, the only directory descended into) and a two-entry \
         probe each for alpha, alpha/one, beta and gamma — no collapsed row materialized"
    );

    assert_eq!(rows(&model), vec!["alpha/one", "beta", "gamma", "top.txt"]);
    assert_eq!(
        model.walk_counts(),
        (2, 4),
        "the second build lists the root again — live, as always — and re-probes nothing"
    );
}

#[test]
fn a_rebuild_re_probes_nothing_and_lists_only_what_the_uncompacted_tree_lists() {
    // The per-frame invariant: drawing a compacted frame does no more filesystem work than drawing
    // an uncompacted one. The fold shapes are cached, so a rebuild re-probes nothing; the listings
    // are not, so a rebuild reads the same directories an uncompacted rebuild would — and the
    // contents a compacted tree draws are never stale.
    let dir = deep_repo();
    let mut model = TreeModel::new(dir.path());
    model.set_compact_dirs(true);
    model.expand(&dir.path().join("src/main/java"));
    assert_eq!(rows(&model), vec!["src/main/java", "App.java", "README.md"]);

    let (full, probes) = model.walk_counts();
    for _ in 0..5 {
        let _ = model.visible_nodes();
    }
    let (full_after, probes_after) = model.walk_counts();
    assert_eq!(
        probes_after - probes,
        0,
        "five more frames, not one re-probe"
    );
    assert_eq!(
        (full_after - full) / 5,
        2,
        "each frame lists the root and the expanded chain end, and nothing else"
    );

    // The same five frames on the same tree with compaction off — the bound the invariant is
    // stated against. Uncompacted, all three chain segments are expanded to show the same file.
    let mut plain = TreeModel::new(dir.path());
    for sub in ["src", "src/main", "src/main/java"] {
        plain.expand(&dir.path().join(sub));
    }
    let (plain_full, _) = plain.walk_counts();
    for _ in 0..5 {
        let _ = plain.visible_nodes();
    }
    let (plain_full_after, plain_probes) = plain.walk_counts();
    assert_eq!(plain_probes, 0, "compaction off probes nothing, ever");
    assert!(
        full_after - full <= plain_full_after - plain_full,
        "a compacted frame reads no more than an uncompacted one ({} vs {})",
        full_after - full,
        plain_full_after - plain_full
    );
}

#[test]
fn re_probing_after_an_invalidation_picks_up_a_changed_fold_shape() {
    // What the cache can lag, and what clears it. Only the fold SHAPE is cached, so a file added
    // inside a folded chain does not shorten the chain until something re-probes — which the
    // controller does wherever it re-reads git (launch, `r`, editor return, baseline toggle,
    // focus-gain). The row's contents were never cached, so nothing else is stale.
    let dir = deep_repo();
    let mut model = TreeModel::new(dir.path());
    model.set_compact_dirs(true);
    assert_eq!(rows(&model), vec!["src/main/java", "README.md"]);

    fs::write(dir.path().join("src/main/Extra.java"), "x").unwrap();
    assert_eq!(
        rows(&model),
        vec!["src/main/java", "README.md"],
        "the cached shape still folds past the new file"
    );

    model.invalidate_compaction();
    assert_eq!(
        rows(&model),
        vec!["src/main", "README.md"],
        "re-probed: the chain now stops where the new file lives"
    );
}

#[test]
fn a_new_root_entry_shows_without_any_invalidation_even_under_compaction() {
    // Contents are read live under compaction too — only the fold shape is cached. A directory
    // added beside the chain appears on the very next frame, no refresh needed.
    let dir = deep_repo();
    let mut model = TreeModel::new(dir.path());
    model.set_compact_dirs(true);
    assert_eq!(rows(&model), vec!["src/main/java", "README.md"]);

    fs::create_dir_all(dir.path().join("vendor")).unwrap();
    assert_eq!(
        rows(&model),
        vec!["src/main/java", "vendor", "README.md"],
        "the root is listed live on every build"
    );
}

#[test]
fn with_compaction_off_nothing_is_probed_or_cached() {
    // The cache is the price of compaction and is charged to nobody else.
    let dir = deep_repo();
    let model = TreeModel::new(dir.path());
    assert_eq!(rows(&model), vec!["src", "README.md"], "precondition");

    fs::create_dir_all(dir.path().join("vendor")).unwrap();
    assert_eq!(
        rows(&model),
        vec!["src", "vendor", "README.md"],
        "the new directory appears with no invalidation"
    );
    assert_eq!(model.walk_counts().1, 0, "and not one probe was taken");
}

#[test]
fn reveal_decides_against_a_live_read_not_a_stale_fold() {
    // A relaxation `reveal` performs is PERMANENT, so it must never be triggered by a cached fold
    // shape that predates the target. Here the new file ends the chain two segments early — but
    // under the stale shape it has no row, which would look exactly like "a filter is hiding it"
    // and flip `show_ignored` on for the rest of the session. `reveal` re-probes first.
    let dir = deep_repo();
    let mut model = TreeModel::new(dir.path());
    model.set_compact_dirs(true);
    assert_eq!(rows(&model), vec!["src/main/java", "README.md"]);

    let added = dir.path().join("src/main/Extra.java");
    fs::write(&added, "x").unwrap();
    assert!(
        !model.visible_nodes().iter().any(|n| n.path == added),
        "precondition: the cached shape still hides the new file"
    );

    assert!(model.reveal(&added), "reveal lands on it");
    assert!(
        !model.show_ignored(),
        "a visible file must not switch the gitignore filter on"
    );
    assert!(!model.hide_hidden(), "nor flip hide-hidden");
    assert!(!model.changed_only(), "nor changed-only");
    assert_eq!(
        model.selected().unwrap().path,
        added,
        "and the cursor is on the target"
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
