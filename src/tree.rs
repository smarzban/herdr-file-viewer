//! Tree Model — the rooted, gitignore-aware file tree, its expansion state, and cursor.
//!
//! Enumerates lazily (immediate children on expand) via the `ignore` crate so launch is
//! fast on large repos (AC-22). Hides gitignored entries by default (AC-4), is bounded by
//! its root — no node ever escapes it (AC-N5) — and reads only, never writes (AC-N1).

use crate::git::Status;
use ignore::WalkBuilder;
use std::collections::{BTreeMap, BTreeSet, HashSet};
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

/// Order tree entries: directories first, then files; alphabetical within each group.
fn sort_entries(entries: &mut [(PathBuf, NodeKind)]) {
    entries.sort_by(|a, b| {
        (b.1 == NodeKind::Dir)
            .cmp(&(a.1 == NodeKind::Dir))
            .then_with(|| a.0.file_name().cmp(&b.0.file_name()))
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
        let mut child_dirs: Vec<&PathBuf> = dirs.iter().filter(|d| is_child(d)).collect();
        let mut child_files: Vec<&PathBuf> = files.iter().filter(|f| is_child(f)).collect();
        child_dirs.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        child_files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

        for d in child_dirs {
            let abs = self.root.join(d);
            out.push(Node {
                path: abs.clone(),
                kind: NodeKind::Dir,
                depth,
                expanded: true,
                status: self.status_for(&abs),
                dir_dirty: self.dir_contains_change(&abs),
            });
            self.emit_synthetic(d, depth + 1, dirs, files, out);
        }
        for f in child_files {
            let abs = self.root.join(f);
            out.push(Node {
                path: abs.clone(),
                kind: NodeKind::File,
                depth,
                expanded: false,
                status: self.status_for(&abs),
                dir_dirty: false,
            });
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
        let mut builder = WalkBuilder::new(dir);
        builder
            .max_depth(Some(1))
            // Dotfiles (e.g. .gitignore, .github) show by default; the hide-hidden toggle (#46)
            // turns on `ignore`'s hidden filter to drop every `.`-prefixed entry.
            .hidden(self.hide_hidden)
            .parents(true) // honor ancestor .gitignore for correct nested semantics
            .git_global(false) // hermetic: ignore the user's global gitignore
            .ignore(false) // only git ignore sources, not generic .ignore files
            .require_git(false) // honor .gitignore even outside a git repo (AC-4, AC-26)
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

    /// Reveal `path` in the tree: expand every collapsed ancestor, relax `changed_only` or
    /// `hide_hidden` if they would hide the target, then move the cursor to the target's
    /// visible-row index. Returns `false` **without moving the cursor** when `path` is not a file
    /// under `root` or does not exist on disk — these guards run before any mutation, so a missing
    /// target leaves the selection untouched (AC-10, AC-20, AC-N5). (A path under `root` that the
    /// finder's gitignore-respecting index would never surface — e.g. an ignored file with
    /// `show_ignored` off — is not reachable through the finder flow.)
    pub fn reveal(&mut self, path: &Path) -> bool {
        if !path.starts_with(&self.root) {
            return false; // above root — AC-N5
        }
        if !path.is_file() {
            return false; // missing or not a regular file — AC-20
        }
        // Expand every ancestor directory from the file's parent up to and including root.
        let mut dir = path.parent();
        while let Some(d) = dir {
            if !d.starts_with(&self.root) {
                break;
            }
            self.expand(d);
            if d == self.root {
                break;
            }
            dir = d.parent();
        }
        // Relax a filter only if it still hides the target after expansion.
        if self.changed_only && !self.visible_nodes().iter().any(|n| n.path == path) {
            self.changed_only = false;
        }
        if self.hide_hidden && !self.visible_nodes().iter().any(|n| n.path == path) {
            self.hide_hidden = false;
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

    /// Keep the cursor within the (possibly shrunken) visible list after a structural or
    /// filter change, so indexing by `cursor` can never run past the end.
    fn clamp_cursor(&mut self) {
        let len = self.visible_nodes().len();
        self.cursor = self.cursor.min(len.saturating_sub(1));
    }
}
