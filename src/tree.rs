//! Tree Model — the rooted, gitignore-aware file tree, its expansion state, and cursor.
//!
//! Enumerates lazily (immediate children on expand) via the `ignore` crate so launch is
//! fast on large repos (AC-22). Hides gitignored entries by default (AC-4), is bounded by
//! its root — no node ever escapes it (AC-N5) — and reads only, never writes (AC-N1).

use crate::git::Status;
use ignore::WalkBuilder;
use std::collections::{BTreeMap, HashSet};
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
}

/// The browsable file tree rooted at `root`.
pub struct TreeModel {
    root: PathBuf,
    expanded: HashSet<PathBuf>,
    cursor: usize,
    show_ignored: bool,
    changed_only: bool,
    /// The changed-set against the active baseline: drives status markers (always shown,
    /// AC-7) and the changed-only filter (AC-6). Keyed by root-relative path.
    status_map: BTreeMap<PathBuf, Status>,
}

impl TreeModel {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            expanded: HashSet::new(),
            cursor: 0,
            show_ignored: false,
            changed_only: false,
            status_map: BTreeMap::new(),
        }
    }

    /// Reveal gitignored/all files (AC-5).
    pub fn set_show_ignored(&mut self, on: bool) {
        self.show_ignored = on;
    }

    /// Restrict the tree to changed files only (AC-6); `changed` is the changed-set
    /// against the active baseline, which also feeds status markers.
    pub fn set_changed_only(&mut self, on: bool, changed: &BTreeMap<PathBuf, Status>) {
        self.changed_only = on;
        self.status_map = changed.clone();
    }

    /// Set the per-file status used for tree markers (AC-7), independent of the filter.
    pub fn set_status(&mut self, status: &BTreeMap<PathBuf, Status>) {
        self.status_map = status.clone();
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The ordered list of currently-visible nodes (root's children, plus the children of
    /// every expanded directory, depth-first).
    pub fn visible_nodes(&self) -> Vec<Node> {
        let mut out = Vec::new();
        self.collect(&self.root, 0, &mut out);
        out
    }

    fn collect(&self, dir: &Path, depth: usize, out: &mut Vec<Node>) {
        for (path, kind) in self.entries(dir) {
            if self.changed_only && !self.leads_to_change(&path, kind) {
                continue;
            }
            // In changed-only mode, auto-descend into directories so the (only) changed
            // files inside are reachable without manual expansion.
            let expanded = kind == NodeKind::Dir
                && (self.changed_only || self.expanded.contains(&path));
            out.push(Node {
                path: path.clone(),
                kind,
                depth,
                expanded,
                status: self.status_for(&path),
            });
            if expanded {
                self.collect(&path, depth + 1, out);
            }
        }
    }

    /// The node's git status (AC-7), looked up by root-relative path.
    fn status_for(&self, path: &Path) -> Option<Status> {
        path.strip_prefix(&self.root)
            .ok()
            .and_then(|rel| self.status_map.get(rel).copied())
    }

    /// In changed-only mode: a file kept iff it is itself changed; a directory kept iff it
    /// (transitively) contains a changed file.
    fn leads_to_change(&self, path: &Path, kind: NodeKind) -> bool {
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return false;
        };
        match kind {
            NodeKind::File => self.status_map.contains_key(rel),
            NodeKind::Dir => self
                .status_map
                .keys()
                .any(|changed| changed != rel && changed.starts_with(rel)),
        }
    }

    /// Immediate children of `dir`: gitignore-filtered (unless `show_ignored`), `.git`
    /// hidden, directories before files, each group alphabetical. Read-only.
    fn entries(&self, dir: &Path) -> Vec<(PathBuf, NodeKind)> {
        let mut builder = WalkBuilder::new(dir);
        builder
            .max_depth(Some(1))
            .hidden(false) // show dotfiles (e.g. .gitignore, .github)
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

        // Directories first, then files; alphabetical within each group.
        entries.sort_by(|a, b| {
            (b.1 == NodeKind::Dir)
                .cmp(&(a.1 == NodeKind::Dir))
                .then_with(|| a.0.file_name().cmp(&b.0.file_name()))
        });
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
    }

    /// Toggle a directory's expansion.
    pub fn toggle(&mut self, path: &Path) {
        if self.expanded.contains(path) {
            self.collapse(path);
        } else {
            self.expand(path);
        }
    }
}
