//! Apply git state to the tree — set the working-tree status markers + changed-set, keep the
//! changed-only filter in sync, and the focus-gained refresh. Part of the Session Controller
//! (split out of `controller/mod.rs`, M6). The re-root orchestration that drives these stays in
//! mod.rs.

use super::*;

impl Controller {
    /// The pane regained focus (the run loop forwards herdr's focus events): re-read git state
    /// so external changes show in the tree. No-op without a repo (AC-26) — so an external
    /// change to a non-git directory costs nothing. In **changed-only** mode the refresh
    /// re-filters the visible list, which can move the cursor to a different file; if the
    /// selection actually changed, re-render so the content pane matches the highlighted row —
    /// otherwise the content (and its scroll) is left untouched, the common case.
    pub fn handle_focus_gained(&mut self) -> Effects {
        if !self.is_git_repo {
            return Effects::noop();
        }
        let before = self.tree.selected().map(|n| n.path);
        self.refresh_git_state();
        if self.tree.selected().map(|n| n.path) != before {
            self.dispatch_render();
        }
        Effects::redraw()
    }

    /// Store a new baseline-dependent changed-set. Re-applies the tree filter only when
    /// baseline-aware `c` is on — status mode (`d`) filters from [`Self::git_status`] instead.
    pub(super) fn set_changed(&mut self, changed: BTreeMap<PathBuf, Status>) {
        self.changed = changed;
        if self.changed_only && !self.status_mode {
            self.tree.set_changed_only(true, &self.changed);
        }
    }

    /// Push a freshly-queried git status + changed-set onto the tree together: per-node status
    /// markers (AC-7), the cached working-tree status for status mode (`d`), and the
    /// baseline-dependent changed-set for `c` (AC-16). Callers source `changed` differently —
    /// synchronously (`refresh_git_state`) or from the off-thread re-root fetch (`poll`).
    pub(super) fn apply_git_state(
        &mut self,
        status: &BTreeMap<PathBuf, Status>,
        changed: BTreeMap<PathBuf, Status>,
    ) {
        self.git_status = status.clone();
        self.tree.set_status(status);
        self.set_changed(changed);
        // Status mode filters from working-tree status; re-apply after a refresh so external
        // edits (focus-gain / `r`) update the filtered tree without leaving the mode.
        if self.status_mode {
            self.tree.set_changed_only(true, &self.git_status);
        }
    }

    /// Re-query git for the working-tree status (tree markers, AC-7) and the changed-set
    /// against the active baseline (AC-16), updating the tree caches. No-op without a repo
    /// (AC-26). Runs on the calling thread, but only on deliberate, infrequent actions —
    /// launch, editor return, baseline toggle, the `r` refresh key, and focus-gain — never the
    /// hot navigation path, where the diff is fetched off-thread (AC-23).
    pub(super) fn refresh_git_state(&mut self) {
        if !self.is_git_repo {
            return;
        }
        let status = self.git.status();
        let changed = self.git.changed_set(self.baseline);
        self.apply_git_state(&status, changed);
        // Refresh the cached branch too: `refresh_git_state` runs on `r`,
        // editor-return, and focus-gain to pick up EXTERNAL git changes — so an external
        // `git checkout` must update the tree's bottom-border branch, not just status/changed-set.
        // Without this the label went stale. `git rev-parse` from the tree root resolves the repo
        // even when the root is a subdir; `None` on a detached HEAD (border omits the branch).
        self.current_branch = crate::git::current_branch(&self.root);
        // Drop any pending re-root async status fetch: this sync
        // refresh has just produced the authoritative status/changed-set, so an older in-flight
        // async result must not later clobber it in `poll`. Invariant: every synchronous
        // git-state recompute invalidates a pending re-root async fetch.
        self.drop_pending_status();
    }

    /// Drop any pending re-root async status/changed-set fetch so a stale in-flight result
    /// cannot later overwrite a freshly-recomputed synchronous git state in [`poll`]. Must be
    /// called after every synchronous git-state recompute.
    pub(super) fn drop_pending_status(&mut self) {
        self.status_rx = None;
    }
}
