//! File Index — a recursive, gitignore-aware walk that returns every file under `root`
//! as a root-relative path string.
//!
//! Used by the Go-to-file feature (AC-12…AC-15, AC-18, AC-19, AC-N1, AC-N2, AC-N5).
//! This is a separate walk from the Tree Model (ADR-0005): no depth limit, files only,
//! and the entire `.git` subtree is pruned via `filter_entry`.

use ignore::WalkBuilder;
use std::path::Path;

/// The shared base for the crate's two gitignore-aware walks — this File Index and the Tree
/// Model (`tree.rs`). Sets the hermetic policy both share so it lives in one place: honor an
/// ancestor `.gitignore`, ignore the user's global gitignore and generic `.ignore` files, and
/// apply `.gitignore` even outside a git repo. The caller sets what differs between the two
/// walks — depth, dotfile hiding, and whether `.gitignore`/`.git/info/exclude` are honored.
///
/// `is_git_repo` bounds how far the `parents(true)` ancestor search is allowed to climb.
/// `ignore::WalkBuilder`'s ancestor search always walks every parent directory up to the
/// filesystem root looking for `.gitignore` files (`parents(true)` has no "stop at the repo
/// root" option of its own) — `require_git` is the only knob that bounds it, by gating the
/// search on finding a `.git`/`.jj` directory. When `root` is itself a git repository, passing
/// `require_git(true)` makes that search stop exactly at `root`'s own `.git` (matching real
/// git's behavior: only ancestors *inside* the repository can ever affect it), so an unrelated
/// enclosing directory or repository above it — a dotfiles checkout, `$HOME`, anything with its
/// own `.gitignore` — can no longer reach in and hide the whole tree. Filed upstream after a
/// git repo nested under `~/stow/documents-ruben` (itself a git repo, with `~/.gitignore`
/// containing a bare `*`) rendered completely empty: `require_git(false)` let the ancestor climb
/// walk straight past the repo boundary into that unrelated checkout and up to `$HOME`.
/// When `root` is NOT a git repo, `require_git(false)` is kept (AC-19: a plain folder's own
/// `.gitignore` is still honored even with no `.git` anywhere in its ancestry).
pub(crate) fn walk_builder(root: &Path, is_git_repo: bool) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .parents(true) // honor ancestor .gitignore for correct nested semantics
        .git_global(false) // hermetic: ignore the user's global gitignore
        .ignore(false) // only git ignore sources, not generic .ignore files
        // Bound the ancestor search at `root`'s own repo boundary when it has one; otherwise
        // fall back to honoring `.gitignore` even outside a repo (AC-13, AC-19, AC-4, AC-26).
        .require_git(is_git_repo);
    builder
}

/// Return every file under `root` as a root-relative `String`, respecting `.gitignore`.
/// Equivalent to [`build_scoped`] with `is_git_repo = false` — kept for callers (and the
/// existing test suite) that don't have a resolved git-repo flag to pass.
///
/// - Recursive (no depth limit) — AC-12.
/// - `.gitignore`-d files are excluded — AC-13.
/// - The `.git` subtree is pruned entirely — AC-14.
/// - Directories are not included, only files — AC-15.
/// - Every returned path is relative to `root` (no leading `/`, no `..`) — AC-N5.
/// - Each call performs a fresh walk; no cache — AC-18.
/// - Works in non-git directories without error (`require_git(false)`) — AC-19.
/// - Read-only: no filesystem or git mutations — AC-N1, AC-N2.
pub fn build(root: &Path) -> Vec<String> {
    build_scoped(root, false)
}

/// Like [`build`], but bounds the ancestor `.gitignore` search at `root`'s own repository
/// boundary when `is_git_repo` is true (see [`walk_builder`]) instead of letting it climb past
/// an unrelated enclosing directory/repository above `root`.
pub fn build_scoped(root: &Path, is_git_repo: bool) -> Vec<String> {
    build_cancellable(root, is_git_repo, || false, |_| {}).unwrap_or_default()
}

/// Same visibility rules as the synchronous index, with cooperative cancellation and a
/// progress callback. Neither callback changes the walk's scope or truncates its results.
pub(crate) fn build_cancellable(
    root: &Path,
    is_git_repo: bool,
    cancelled: impl Fn() -> bool,
    progress: impl Fn(usize),
) -> Option<Vec<String>> {
    let mut builder = walk_builder(root, is_git_repo);
    builder
        .hidden(false) // include dotfiles (AC-17 depends on the index NOT hiding dotfiles)
        .git_ignore(true)
        .git_exclude(true)
        .filter_entry(|e| e.file_name() != ".git"); // prune entire .git subtree — AC-14

    let mut paths = Vec::new();
    for entry in builder.build() {
        if cancelled() {
            return None;
        }
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_some_and(|t| t.is_file())
            && let Ok(rel) = entry.path().strip_prefix(root)
        {
            paths.push(rel_to_slash(rel));
            if paths.len() % 128 == 0 {
                progress(paths.len());
            }
        }
    }
    progress(paths.len());
    Some(paths)
}

/// Render a root-relative path as a forward-slash string on every platform. The rest of the app
/// (git status/diff/worktree paths, the tree, the content title) speaks git's forward-slash
/// convention; on Windows the native separator is `\`, so a raw stringification would make the
/// finder's listing inconsistent with the rest of the UI. Joining the path's `Normal` components
/// with `/` is identical to today's output on unix (the separator already is `/`) and converts
/// `a\b` → `a/b` on Windows. It also enforces AC-N5 (root-relative, no `..`/absolute leak) by
/// construction.
fn rel_to_slash(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn root_with_files(tag: &str, n: usize) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hfv-index-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..n {
            std::fs::write(root.join(format!("f{i:03}.rs")), "x").unwrap();
        }
        root
    }

    #[test]
    fn cancellable_walk_reports_progress_every_128_files_and_at_the_end() {
        let root = root_with_files("progress", 300);
        let seen = RefCell::new(Vec::new());
        let paths = build_cancellable(&root, false, || false, |n| seen.borrow_mut().push(n));
        assert_eq!(paths.map(|p| p.len()), Some(300));
        assert_eq!(*seen.borrow(), [128, 256, 300]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancellable_walk_stops_partway_when_cancelled() {
        let root = root_with_files("cancel", 300);
        let checks = Cell::new(0);
        let last_progress = Cell::new(0);
        let paths = build_cancellable(
            &root,
            false,
            || {
                checks.set(checks.get() + 1);
                checks.get() > 200
            },
            |n| last_progress.set(n),
        );
        assert!(
            paths.is_none(),
            "cancellation is not reported as a complete index"
        );
        assert_eq!(
            checks.get(),
            201,
            "the walk stops at the first check that sees it"
        );
        assert!(last_progress.get() < 300, "the walk did not run to the end");
        let _ = std::fs::remove_dir_all(&root);
    }
}
