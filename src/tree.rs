//! Tree Model — the rooted, gitignore-aware file tree, its expansion state, and cursor.
//!
//! Enumerates lazily (immediate children on expand) via the `ignore` crate so launch is
//! fast on large repos (AC-22). Hides gitignored entries by default (AC-4), is bounded by
//! its root — no node ever escapes it (AC-N5) — and reads only, never writes (AC-N1).

use crate::git::Status;
use crate::index::walk_builder;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Whether a tree node is a directory or a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Dir,
    File,
}

/// One visible row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub path: PathBuf,
    pub kind: NodeKind,
    pub depth: usize,
    pub expanded: bool,
    pub status: Option<Status>,
    /// For a directory: whether any file under it has a git status (so the Presenter can
    /// color a folder that contains changes). Always `false` for files.
    pub dir_dirty: bool,
}

/// The tree's sibling order, and the **single source of truth** for it: directories before
/// files, alphabetical within each group.
///
/// Every ordering in this module routes through here — the filesystem rows ([`sort_entries`]),
/// the synthesized changed-only rows ([`TreeModel::emit_synthetic`]), and the `]` / `[` jump's
/// traversal ([`cmp_visual`]) — so a change to how siblings sort can never leave one of them
/// behind.
fn cmp_sibling(a: (&OsStr, NodeKind), b: (&OsStr, NodeKind)) -> Ordering {
    (b.1 == NodeKind::Dir)
        .cmp(&(a.1 == NodeKind::Dir))
        .then_with(|| a.0.cmp(b.0))
}

/// Compare two root-relative paths by the order their rows appear in the tree, given the
/// [`NodeKind`] of each path's own last component (interior components are directories by
/// construction).
///
/// Walks components from the common prefix and, at the first divergence, defers to
/// [`cmp_sibling`] — so a path that continues (a directory at that level) orders before one that
/// ends there, and same-kind siblings use the tree's name ordering. When one path is an ancestor
/// of the other the ancestor's row comes first, matching the depth-first render.
///
/// This is why `Cargo.toml` is the *last* row next to `docs/` and `src/` even though it is the
/// *first* key in a `BTreeMap` keyed by path: lexicographic key order is not row order.
fn cmp_visual(a: (&Path, NodeKind), b: (&Path, NodeKind)) -> Ordering {
    let (mut ac, mut bc) = (a.0.components(), b.0.components());
    loop {
        match (ac.next(), bc.next()) {
            (Some(x), Some(y)) => {
                // A component is a directory when another follows it; the last one carries the
                // caller-supplied kind (a cursor may sit on a directory row).
                let xk = if ac.clone().next().is_some() {
                    NodeKind::Dir
                } else {
                    a.1
                };
                let yk = if bc.clone().next().is_some() {
                    NodeKind::Dir
                } else {
                    b.1
                };
                match cmp_sibling((x.as_os_str(), xk), (y.as_os_str(), yk)) {
                    Ordering::Equal => continue, // same node at this level — descend
                    ord => return ord,
                }
            }
            (Some(_), None) => return Ordering::Greater, // `b` is an ancestor: its row is first
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
    }
}

/// The row order of a root-relative path, for a caller outside this module (the jump's tests).
/// Both paths are treated as files, which is what a changed-set holds.
pub fn cmp_file_rows(a: &Path, b: &Path) -> Ordering {
    cmp_visual((a, NodeKind::File), (b, NodeKind::File))
}

/// The visible-row index of the **file** node at `path`, if it has one.
///
/// Kind-checked rather than path-only: a file replaced by a directory of the same name puts one
/// path into both halves of the changed-set's synthesized tree — the file list and the ancestor
/// directory set — so changed-only mode emits a `Dir` row AND a `File` row for it, the directory
/// first. A path-only lookup would land the jump on the directory row and never show the file's
/// diff.
fn file_row(rows: &[Node], path: &Path) -> Option<usize> {
    rows.iter()
        .position(|n| n.path == path && n.kind == NodeKind::File)
}

/// Order tree entries: directories first, then files; alphabetical within each group.
fn sort_entries(entries: &mut [(PathBuf, NodeKind)]) {
    entries.sort_by(|a, b| {
        cmp_sibling(
            (a.0.file_name().unwrap_or_default(), a.1),
            (b.0.file_name().unwrap_or_default(), b.1),
        )
    });
}

/// The browsable file tree rooted at `root`.
pub struct TreeModel {
    root: PathBuf,
    expanded: HashSet<PathBuf>,
    cursor: usize,
    show_ignored: bool,
    hide_hidden: bool,
    changed_only: bool,
    /// Per-file status for tree markers (AC-7), keyed by root-relative path. Set
    /// independently of the filter (`set_status`) so the two can never overwrite each
    /// other.
    markers: BTreeMap<PathBuf, Status>,
    /// The changed-set driving the changed-only filter (AC-6), set by `set_changed_only`.
    changed_filter: BTreeMap<PathBuf, Status>,
}

impl TreeModel {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            expanded: HashSet::new(),
            cursor: 0,
            show_ignored: false,
            hide_hidden: false,
            changed_only: false,
            markers: BTreeMap::new(),
            changed_filter: BTreeMap::new(),
        }
    }

    /// Reveal gitignored/all files (AC-5).
    pub fn set_show_ignored(&mut self, on: bool) {
        self.show_ignored = on;
        self.clamp_cursor();
    }

    /// Hide dot-prefixed files and folders (#46) — independent of the gitignore toggle. Off by
    /// default, so dotfiles (`.gitignore`, `.github`) stay browsable until the user asks to hide
    /// them (e.g. when opening a `$HOME` flooded with dotfiles).
    pub fn set_hide_hidden(&mut self, on: bool) {
        self.hide_hidden = on;
        self.clamp_cursor();
    }

    /// Restrict the tree to changed files only (AC-6); `changed` is the changed-set
    /// against the active baseline.
    pub fn set_changed_only(&mut self, on: bool, changed: &BTreeMap<PathBuf, Status>) {
        self.changed_only = on;
        self.changed_filter = changed.clone();
        self.clamp_cursor();
    }

    /// Set the per-file status used for tree markers (AC-7), independent of the filter.
    pub fn set_status(&mut self, status: &BTreeMap<PathBuf, Status>) {
        self.markers = status.clone();
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether the changed-only filter is currently active on the tree. Exposed so the
    /// controller can re-sync its mirror field after `reveal` may have relaxed this flag.
    pub fn changed_only(&self) -> bool {
        self.changed_only
    }

    /// Whether the hide-hidden filter is currently active on the tree. Exposed so the
    /// controller can re-sync its mirror field after `reveal` may have relaxed this flag.
    pub fn hide_hidden(&self) -> bool {
        self.hide_hidden
    }

    /// Whether gitignored entries are currently shown. Exposed so the controller can re-sync
    /// its mirror after `reveal` may have relaxed this flag for an explicit launch target.
    pub fn show_ignored(&self) -> bool {
        self.show_ignored
    }

    /// The ordered list of currently-visible nodes. In the full tree these are root's
    /// children plus the children of every expanded directory, depth-first. In changed-only
    /// mode the tree is built from the changed-set itself (so deleted files — and files
    /// under a deleted directory — still appear, AC-6/AC-7), with every directory expanded.
    pub fn visible_nodes(&self) -> Vec<Node> {
        if self.changed_only {
            return self.changed_only_nodes();
        }
        let mut out = Vec::new();
        self.collect(&self.root, 0, &mut out);
        out
    }

    fn collect(&self, dir: &Path, depth: usize, out: &mut Vec<Node>) {
        for (path, kind) in self.entries(dir) {
            let expanded = kind == NodeKind::Dir && self.expanded.contains(&path);
            let dir_dirty = kind == NodeKind::Dir && self.dir_contains_change(&path);
            out.push(Node {
                path: path.clone(),
                kind,
                depth,
                expanded,
                status: self.status_for(&path),
                dir_dirty,
            });
            if expanded {
                self.collect(&path, depth + 1, out);
            }
        }
    }

    /// Build the changed-only tree from the changed-set's paths (not the filesystem), so
    /// deletions — including whole deleted directories — are reviewable.
    fn changed_only_nodes(&self) -> Vec<Node> {
        let files: BTreeSet<PathBuf> = self.changed_filter.keys().cloned().collect();
        let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
        for rel in &files {
            let mut ancestor = rel.parent();
            while let Some(p) = ancestor {
                if p.as_os_str().is_empty() {
                    break;
                }
                dirs.insert(p.to_path_buf());
                ancestor = p.parent();
            }
        }
        let mut out = Vec::new();
        self.emit_synthetic(Path::new(""), 0, &dirs, &files, &mut out);
        out
    }

    fn emit_synthetic(
        &self,
        parent_rel: &Path,
        depth: usize,
        dirs: &BTreeSet<PathBuf>,
        files: &BTreeSet<PathBuf>,
        out: &mut Vec<Node>,
    ) {
        let is_child = |rel: &Path| rel.parent().unwrap_or(Path::new("")) == parent_rel;
        let mut children: Vec<(&PathBuf, NodeKind)> = dirs
            .iter()
            .filter(|d| is_child(d))
            .map(|d| (d, NodeKind::Dir))
            .chain(
                files
                    .iter()
                    .filter(|f| is_child(f))
                    .map(|f| (f, NodeKind::File)),
            )
            .collect();
        children.sort_by(|a, b| {
            cmp_sibling(
                (a.0.file_name().unwrap_or_default(), a.1),
                (b.0.file_name().unwrap_or_default(), b.1),
            )
        });

        for (rel, kind) in children {
            let abs = self.root.join(rel);
            out.push(Node {
                path: abs.clone(),
                kind,
                depth,
                expanded: kind == NodeKind::Dir,
                status: self.status_for(&abs),
                dir_dirty: kind == NodeKind::Dir && self.dir_contains_change(&abs),
            });
            if kind == NodeKind::Dir {
                self.emit_synthetic(rel, depth + 1, dirs, files, out);
            }
        }
    }

    /// The node's git status (AC-7): the dedicated marker map, falling back to the
    /// changed-set so synthesized deleted nodes still carry their marker.
    fn status_for(&self, path: &Path) -> Option<Status> {
        path.strip_prefix(&self.root).ok().and_then(|rel| {
            self.markers
                .get(rel)
                .or_else(|| self.changed_filter.get(rel))
                .copied()
        })
    }

    /// Whether any tracked change lives under directory `path` — used to color a folder that
    /// contains changes (AC-7). Component-wise prefix match, so `src` is not matched by
    /// `src2/…`; excludes the directory's own path.
    fn dir_contains_change(&self, path: &Path) -> bool {
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return false;
        };
        self.markers
            .keys()
            .chain(self.changed_filter.keys())
            .any(|k| k != rel && k.starts_with(rel))
    }

    /// Immediate children of `dir`: gitignore-filtered (unless `show_ignored`), dot-prefixed
    /// entries dropped when `hide_hidden` (#46), `.git` always hidden, directories before files,
    /// each group alphabetical. Read-only.
    fn entries(&self, dir: &Path) -> Vec<(PathBuf, NodeKind)> {
        let mut builder = walk_builder(dir);
        builder
            .max_depth(Some(1))
            // Dotfiles (e.g. .gitignore, .github) show by default; the hide-hidden toggle (#46)
            // turns on `ignore`'s hidden filter to drop every `.`-prefixed entry.
            .hidden(self.hide_hidden)
            .git_ignore(!self.show_ignored)
            .git_exclude(!self.show_ignored);

        let mut entries: Vec<(PathBuf, NodeKind)> = builder
            .build()
            .filter_map(Result::ok)
            .filter(|e| e.depth() == 1) // children only, not `dir` itself
            .filter(|e| e.file_name().to_str() != Some(".git")) // never browse into .git
            .map(|e| {
                let kind = if e.file_type().is_some_and(|t| t.is_dir()) {
                    NodeKind::Dir
                } else {
                    NodeKind::File
                };
                (e.into_path(), kind)
            })
            .collect();

        sort_entries(&mut entries);
        entries
    }

    /// Expand a directory (no-op for a path outside the root — AC-N5).
    pub fn expand(&mut self, path: &Path) {
        if path.starts_with(&self.root) {
            self.expanded.insert(path.to_path_buf());
        }
    }

    /// Collapse a directory.
    pub fn collapse(&mut self, path: &Path) {
        self.expanded.remove(path);
        self.clamp_cursor();
    }

    /// Set the cursor to an absolute visible-row index, clamped to the visible range (used by
    /// a mouse click that selects the row it landed on).
    pub fn set_cursor(&mut self, idx: usize) {
        let len = self.visible_nodes().len();
        self.cursor = if len == 0 { 0 } else { idx.min(len - 1) };
    }

    /// Move the cursor by `delta` rows, clamped to the visible range.
    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.visible_nodes().len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        let max = (len - 1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
    }

    /// The currently-selected node, if any.
    pub fn selected(&self) -> Option<Node> {
        self.visible_nodes().into_iter().nth(self.cursor)
    }

    /// Expand every ancestor directory of `path`, from its parent up to and including the root, so
    /// a target buried in collapsed directories can get a row.
    ///
    /// Expansion state only — it never touches a display filter. That split is what lets the
    /// `]` / `[` jump reuse this while still honoring its promise to change nothing but the cursor
    /// and expansion state; [`reveal`](Self::reveal) layers the filter relaxation on top.
    ///
    /// Returns the directories it **newly** expanded, so a caller that is only probing can tell
    /// whether it changed anything and undo exactly what it did. An empty result means the tree
    /// already looked like this — which is what lets the `]` / `[` jump skip a redundant walk.
    ///
    /// The root is deliberately not expanded: its children always render, so its membership in
    /// `expanded` is never read (only a *child* directory's is), and inserting it would make every
    /// root-level target look like a state change to that probe.
    fn expand_ancestors(&mut self, path: &Path) -> Vec<PathBuf> {
        let mut newly = Vec::new();
        let mut dir = path.parent();
        while let Some(d) = dir {
            if d == self.root || !d.starts_with(&self.root) {
                break;
            }
            if !self.expanded.contains(d) {
                self.expand(d);
                newly.push(d.to_path_buf());
            }
            dir = d.parent();
        }
        newly
    }

    /// Reveal `path` in the tree: expand every collapsed ancestor, relax display filters
    /// (`changed_only`, `hide_hidden`, `show_ignored`) if they would hide the target, then move the
    /// cursor to the target's visible-row index. Returns `false` **without moving the cursor** when
    /// `path` is not a file under `root` or does not exist on disk — these guards run before any
    /// mutation, so a missing target leaves the selection untouched (AC-10, AC-20, AC-N5).
    ///
    /// An explicit caller-supplied path (launch open target, or a finder confirm for a path that
    /// still exists on disk) is intent: filters relax only when the target remains hidden after
    /// expansion, same condition for all three. The finder's index is still gitignore-respecting, so
    /// ignored files are not *listed* there; `reveal` itself can surface them when given a concrete
    /// path (e.g. `--open` on a gitignored build artifact).
    pub fn reveal(&mut self, path: &Path) -> bool {
        if !path.starts_with(&self.root) {
            return false; // above root — AC-N5
        }
        if !path.is_file() {
            return false; // missing or not a regular file — AC-20
        }
        self.expand_ancestors(path);
        // Relax a filter only if it still hides the target after expansion.
        if self.changed_only && !self.visible_nodes().iter().any(|n| n.path == path) {
            self.changed_only = false;
        }
        if self.hide_hidden && !self.visible_nodes().iter().any(|n| n.path == path) {
            self.hide_hidden = false;
        }
        // Explicit path intent beats the default gitignore hide (launch open target / known path).
        if !self.show_ignored && !self.visible_nodes().iter().any(|n| n.path == path) {
            self.show_ignored = true;
        }
        // Move the cursor to the target's visible row.
        match self.visible_nodes().iter().position(|n| n.path == path) {
            Some(idx) => {
                self.cursor = idx;
                true
            }
            None => false,
        }
    }

    /// Move the cursor to the next (`forward`) or previous **changed file** in `changed`, and
    /// report whether the move wrapped around the ends. Returns `None` — leaving the cursor
    /// untouched — when `changed` is empty or no candidate could be selected.
    ///
    /// Traversal order is the order the candidates' **rows** appear in the tree ([`cmp_visual`]),
    /// not the changed-set's lexicographic key order — the tree renders directories before files
    /// at each level, so `Cargo.toml` beside `docs/` and `src/` is the last row while sorting
    /// first as a key. That is what makes the reported wrap mean exactly "moved past the last
    /// candidate row to the first" (and the reverse). Candidates come from the changed-set rather
    /// than the visible rows, so a changed file inside a **collapsed** directory is still reachable:
    /// each candidate is first looked up among the visible rows and, failing that, has its
    /// ancestors expanded ([`expand_ancestors`](Self::expand_ancestors)) and is looked up again.
    ///
    /// A candidate that still has no row is **skipped** — a deleted file has none on disk outside
    /// changed-only mode, and neither does one hidden by `hide_hidden`, `show_ignored`, or
    /// `changed_only`. Unlike [`reveal`](Self::reveal), the jump never relaxes a display filter to
    /// force a target into view: `]` is navigation, not an explicit request for one path, so it
    /// stays inside the tree the user has filtered to and keeps its promise below. A skipped
    /// candidate leaves no trace — any expansion done while probing it is rolled back.
    ///
    /// Read-only navigation: it moves the cursor and expansion state only (AC-N1, AC-N3).
    pub fn select_changed(
        &mut self,
        forward: bool,
        changed: &BTreeMap<PathBuf, Status>,
    ) -> Option<bool> {
        // Sorted once per jump — never per frame; the changed-set is a map, so its keys arrive in
        // lexicographic order and have to be put into row order here.
        let mut candidates: Vec<&PathBuf> = changed.keys().collect();
        candidates.sort_by(|a, b| cmp_visual((a, NodeKind::File), (b, NodeKind::File)));
        let len = candidates.len();
        if len == 0 {
            return None;
        }
        // Where the cursor sits within that order. A cursor on a directory, on an unchanged file,
        // or on nothing still has a well-defined neighbour: `partition_point` finds the insertion
        // point, so the jump goes to the next / previous changed file either way. The cursor's own
        // kind is passed through, so a directory row compares as the parent of its children rather
        // than as a file sharing their name.
        // The visible rows, walked ONCE for the whole jump: in the full tree this is a filesystem
        // enumeration, so calling it per candidate turned a jump over a run of deleted files into a
        // run of full walks on the input thread. Every branch below either returns or rolls its
        // mutation back, so this snapshot stays accurate for the entire loop.
        let rows = self.visible_nodes();
        let current = rows.get(self.cursor).and_then(|n| {
            n.path
                .strip_prefix(&self.root)
                .map(|rel| (rel.to_path_buf(), n.kind))
                .ok()
        });
        let at = |c: &Path, rel: &Path, kind| cmp_visual((c, NodeKind::File), (rel, kind));
        let (start, start_wrapped) = match &current {
            Some((rel, kind)) if forward => {
                let i = candidates.partition_point(|c| at(c, rel, *kind) != Ordering::Greater);
                if i < len { (i, false) } else { (0, true) }
            }
            Some((rel, kind)) => {
                let i = candidates.partition_point(|c| at(c, rel, *kind) == Ordering::Less);
                if i > 0 {
                    (i - 1, false)
                } else {
                    (len - 1, true)
                }
            }
            None if forward => (0, false),
            None => (len - 1, false),
        };
        let mut idx = start;
        let mut wrapped = start_wrapped;
        for step in 0..len {
            if step > 0 {
                // Step past a candidate that could not be selected, tracking the wrap.
                if forward {
                    idx += 1;
                    if idx == len {
                        idx = 0;
                        wrapped = true;
                    }
                } else if idx == 0 {
                    idx = len - 1;
                    wrapped = true;
                } else {
                    idx -= 1;
                }
            }
            let abs = self.root.join(candidates[idx]);
            // Already on screen: no mutation, and no second walk.
            if let Some(pos) = file_row(&rows, &abs) {
                self.cursor = pos;
                return Some(wrapped);
            }
            // No file on disk (a deletion, or a path now taken by a directory) can gain a row
            // outside changed-only mode, which the lookup above already covers. Skipping on a
            // cheap `stat` keeps a run of deleted candidates off the walk path entirely.
            if !abs.is_file() {
                continue;
            }
            // Buried under collapsed directories: expanding is the one mutation the jump is
            // allowed, so it is worth a fresh walk. If nothing was NEWLY expanded, the tree is
            // exactly what `rows` was taken from — the invariant holds because every earlier
            // iteration either returned or rolled its expansion back, and the jump never touches a
            // filter — so a fresh walk could only reproduce `rows`, which the lookup above already
            // searched. The candidate is hidden by a filter, not by a collapsed ancestor: skip it
            // without walking, or a run of filtered-out changed files costs a walk apiece.
            let newly_expanded = self.expand_ancestors(&abs);
            if newly_expanded.is_empty() {
                continue;
            }
            let expanded_rows = self.visible_nodes();
            if let Some(pos) = file_row(&expanded_rows, &abs) {
                self.cursor = pos;
                return Some(wrapped);
            }
            // Still hidden — by `hide_hidden`, `show_ignored`, or `changed_only`. The jump does
            // not relax filters to reach it, so treat it as unselectable and undo exactly what the
            // probe expanded. Not `collapse`, which re-clamps the cursor and costs another walk.
            for d in &newly_expanded {
                self.expanded.remove(d);
            }
        }
        None
    }

    /// Keep the cursor within the (possibly shrunken) visible list after a structural or
    /// filter change, so indexing by `cursor` can never run past the end.
    fn clamp_cursor(&mut self) {
        let len = self.visible_nodes().len();
        self.cursor = self.cursor.min(len.saturating_sub(1));
    }
}
