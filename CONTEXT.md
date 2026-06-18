# CONTEXT — glossary

Canonical vocabulary for this repo. Glossary only: no implementation detail, no specs.

## Project terms

- **viewer** — the plugin itself: the git-aware, read-only file viewer that runs as a
  herdr TUI pane. (Working name: herdr-file-viewer.)
- **tree** — the left column: a recursive, expandable directory tree of the current
  root, decorated with git status markers.
- **content pane** — the right column: shows the selected file as rendered markdown,
  a diff, or syntax-highlighted content, depending on the active view mode.
- **view mode** — which rendering the content pane is showing (rendered markdown /
  diff / content). Auto-selected per file, cyclable by the user.
- **diff baseline** — what a diff (and the meaning of "changed") is compared against:
  the base branch, or HEAD. Chosen by a context-smart default, toggleable.
- **base branch** — the branch a feature branch / worktree forked from (e.g. main);
  the baseline for reviewing the full body of work in a worktree.
- **changed-only filter** — a toggle that restricts the tree to files git reports as
  changed against the active diff baseline.
- **root** — the directory the tree is rooted at: the worktree root, or the pane's cwd
  when not in a worktree. The viewer does not browse above it.
- **size cap** — the file-size limit (≥ 1 MB or ≥ 5,000 lines) above which the content
  pane shows a bounded, truncated preview instead of the whole file.
- **truncation notice** — the visible indicator shown when a file is previewed only up
  to the size cap.
- **renderer fallback** — the plain, unstyled text the content pane shows when a
  delegated renderer is unavailable, alongside a non-fatal notice.
- **focus-toggle** — the control that, in a narrow split (< 80 columns), gives the full
  pane width to either the tree or the content pane.

## herdr terms (host platform)

- **worktree** — a herdr-managed git worktree, typically where an agent does its work.
- **action** — a herdr plugin command bound to a keybinding, run in a workspace
  context; how the user summons the viewer.
- **pane** — a herdr terminal surface a plugin can own (overlay / split / tab /
  zoomed). The viewer runs in a split pane.
