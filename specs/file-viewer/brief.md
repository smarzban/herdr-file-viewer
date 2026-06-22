# Brief: herdr-file-viewer (working name)

- Status: settled (idea stage) — 2026-06-16
- Next stage: acceptance-criteria

## Problem / intent

Inside a herdr workspace, reading files or seeing *what changed* — especially an
agent's work in a worktree — means leaving for an editor or dropping to separate
`git` commands. We want an in-herdr, git-aware file viewer to browse, read, and
review changes without leaving the workspace.

## What it is

A herdr plugin: a keyboard-driven TUI that opens in a split pane beside the user's
current work. A recursive directory tree on the left; file content on the right.
Git awareness runs throughout (status in the tree, diffs in the content pane), not
as a bolt-on preview mode.

## Scope (v1)

**Tree (left)**
- Recursive, expandable directory tree, rooted at the worktree root (or the pane's
  cwd when not in a worktree).
- Git status markers always shown (modified / added / deleted / untracked).
- Toggle to filter to changed files only.
- `.gitignore`-aware by default; toggle to show all files.

**Content (right)**
- Auto-selects the most useful view per file:
  - rendered markdown for markdown files,
  - diff for a changed file,
  - syntax-highlighted content otherwise.
- A key cycles modes, so the user can always force a different view — the rendered,
  syntax-highlighted, compact-diff, or full-context (whole-file, line-numbered) diff view.

**Git / diff**
- "The diff" and the meaning of "changed" compare against a context-smart baseline:
  - vs base branch when on a feature branch / in a worktree (the full body of work),
  - vs HEAD (uncommitted changes) otherwise.
- The baseline is toggleable.

**Interaction**
- Read-only. One escape hatch: open the selected file in the user's editor or a new
  herdr pane/tab.
- Keyboard-first navigation.
- Summoned via a keybinding (a herdr action); opens as a split pane.
- Narrow-split handling: collapse-tree / focus-toggle so the two columns stay usable
  in a thin split.

## Non-goals (v1)

- No file mutations (rename / delete / create / move).
- No git mutations (stage / unstage / commit / discard).
- No in-pane editing.
- No auto-launch or event hooks — the user summons it explicitly.
- Single root only (no browsing above the root, no multi-root sessions).

## Chosen approach

**Hybrid**: build the navigation/layout/git shell ourselves and delegate rendering to
mature terminal CLIs (a markdown renderer, a diff renderer, a syntax highlighter).

Why, over the alternatives considered:
- *Compose existing tools* (wrap a file manager): fastest to ship, but UX and depth of
  git integration are capped at what that tool offers — and "git-first-class,
  herdr-context-aware" is exactly the part no off-the-shelf tool delivers.
- *Build fully custom* (incl. rendering): maximal control, but reimplements markdown /
  diff / syntax rendering that already exist well — most code, most maintenance.
- *Hybrid* wins the control-to-effort trade: we own the differentiated shell; rendering
  is a solved problem we reuse. See `docs/adr/0001-hybrid-build-delegate-rendering.md`.

Specific render CLIs and the implementation language are deferred to the techstack
stage.

## Resolved key decisions

1. **Primary job** — a general file viewer that is git-first-class throughout (not a
   pure browser, not solely a change-reviewer).
2. **Build strategy** — hybrid (own the shell, delegate rendering).
3. **Read/write scope** — read-only + open-in-editor escape hatch.
4. **Diff baseline** — both vs-base-branch and vs-HEAD, with a context-smart default,
   toggleable.
5. **Placement** — opens as a split pane beside current work, summoned by a keybinding.

## Deferred (with reason)

- **Plugin name** (`herdr-file-viewer` is a working name): cosmetic, decide anytime.
- **Specific render CLIs + implementation language**: techstack stage.
- **Exact narrow-split layout mechanics** (collapse / focus-toggle behavior): design
  stage.

## Glossary terms touched

viewer, tree, content pane, view mode, diff baseline, base branch, changed-only
filter, root, worktree, action, pane. Definitions live in `/CONTEXT.md`.

## ADRs

- `docs/adr/0001-hybrid-build-delegate-rendering.md` — own the shell, delegate rendering.
