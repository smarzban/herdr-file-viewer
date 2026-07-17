# Usage guide

A tour of what the viewer does, feature by feature. For the exact keys and mouse gestures see the
[keys reference](keys.md); to open the viewer in the first place see [summoning](summoning.md); to
customize it see [configuration](configuration.md).

- [The tree](#the-tree)
- [Finding a file fast](#finding-a-file-fast)
- [Viewing a file](#viewing-a-file)
- [Git awareness](#git-awareness)
- [Navigating within a file](#navigating-within-a-file)
- [Annotating files and ranges](#annotating-files-and-ranges)
- [Copying paths and lines](#copying-paths-and-lines)
- [Handing a file off](#handing-a-file-off)
- [Switching worktree](#switching-worktree)
- [In-app help](#in-app-help)
- [Staying up to date](#staying-up-to-date)
- [Using the mouse](#using-the-mouse)

## The tree

The left column is a recursive, expandable directory tree, **rooted at the worktree root** when you
launch inside a git repo, otherwise at the launch directory. It honors `.gitignore` (press `i` to
reveal ignored files), and a separate toggle (`.`) hides dot-prefixed "hidden" files and folders
when a directory is full of them. The tree's **top border names the root** directory and its
**bottom border shows the current branch**, so you always know *where* and *on what branch* you're
looking.

Move the cursor with `↑`/`↓` (or `k`/`j`), expand/collapse a directory with `→`/`←` (or `l`/`h`) or
`Enter`. The tree scrolls to keep the selection in view, and sideways for long or deeply-nested
names — reachable by keyboard with `H` / `L` when the tree is focused. A scrollbar appears whenever
there's more than fits. Narrow or widen the tree column with `<` / `>`, or drag the divider; the
starting split, the tree's side, and a column cap are all [configurable](configuration.md).

## Finding a file fast

Press `f` to open a **fuzzy finder** over every file in the tree (`.gitignore`-aware). Type to
filter, `↑`/`↓` to move, `Enter` to open, `Esc` to cancel — far faster than scrolling the tree in a
large repo.

## Viewing a file

The content pane shows **the right view for each file, automatically**: a changed file shows its
**diff**, a markdown file **renders**, a binary **document** (`.docx`, `.odt`, `.pdf`, `.pptx`,
`.xlsx`) is **converted and rendered**, and anything else is **syntax-highlighted** content with
line numbers. No mode-switching, no commands.

- **Cycle the view** with `v` to override the automatic choice (e.g. see a changed markdown file's
  raw source instead of its diff).
- A changed file can also show a **full-file diff**: the whole file with line numbers and the diff
  shown inline.
- **Scroll** the content in all four directions once it's focused (`Tab` to it, then the arrows or
  `h`/`j`/`k`/`l`). Prose (markdown / plain text) wraps; diffs and code keep their original lines so
  columns stay aligned. Press `w` to toggle wrapping, or for rendered markdown to switch between the
  fit-to-pane view (wide tables sized to fit, over-long cells shown as `…`) and a wide view that
  renders tables at full width and scrolls sideways.
- **Zoom** with `z` to hide the tree and read the file across the full pane; press again (or
  `q`/`Esc`) to restore the split.
- **Full-screen** with `Z` (Shift+`z`) to open the file *and* zoom the viewer's herdr pane to fill
  the whole terminal — the file takes over the entire screen, not just the split. `Z` again (or
  `Esc`/`q`/`z`) returns to the split.

Rendering is **delegated** to `glow` (markdown), `delta` (diffs), and `bat` (syntax); when a
renderer isn't installed the viewer falls back to plain text with a short notice. See
[external renderers](renderers.md).

## Git awareness

Git status is woven straight into the tree, not a separate mode:

- **Status markers**: each file carries its git-status letter — `M` modified, `A` added, `D`
  deleted, `?` untracked — and a directory containing any change carries a `●`. They're **colored**
  so changes read at a glance (changed files and dirty folders red, new files green), with the glyph
  as a non-color cue so status survives a colorblind palette or a non-default terminal theme.
- **Changed-files-only filter**: press `c` to restrict the tree to files git reports as changed.
- **Diff baseline**: press `b` to flip what "changed" and the diff compare against — the merge-base
  of your branch (review your whole branch) versus `HEAD` (just your uncommitted work).
- **Refresh**: the viewer re-reads git status automatically when the pane regains focus, so a merge,
  pull, or commit you make elsewhere shows up on its own; `r` forces a full refresh on demand.

Git is read through the system `git` CLI (read-only subcommands only). Without git on `PATH` the
viewer still opens, but the status markers, filter, baseline, and diffs are degraded — see
[install](install.md).

## Navigating within a file

- **Go to a line**: press `:` and type a line number to jump the content pane straight there. In a
  rendered-markdown or diff view it switches to the line-numbered content view to make the jump;
  out-of-range clamps to the last line.
- **Search in the file**: press `/` to search the open file's content. Every match highlights as you
  type, `Enter` commits, and `n` / `N` cycle through matches (wrapping at the ends). Smartcase — a
  lowercase query matches any case; add a capital to go case-sensitive — and it works in every view
  (code, markdown, or diff). `Esc` clears it and restores your scroll.

## Annotating files and ranges

Annotations are read-only notes for the **current viewer session and root**. They start empty on
launch, stay in memory only, and never modify files or git state.

Annotated files show `@` in the tree's reserved prefix column (alongside any git marker) and before
the applied content title. Unselected annotated filenames use a subtle background; line/range
targets use the same background on extant lines in the source/content view, including a one-cell cue
for a blank line. Rendered Markdown and diff views keep the file/title `@`, but do not color numeric
line targets because transformed output has no trustworthy source-line mapping. Active line-select,
mouse selection, and search highlighting take precedence: cyan replaces the persistent background,
while the current line-select marker or current search match retains it with reversed bold emphasis.
Closing the active state reveals the persistent annotation background again.

- **Add to a file**: select a file and press `a`, type the note, then press `Enter` to save or `Esc`
  to cancel. Directories cannot be annotated.
- **Add to lines**: focus the content pane, press `L`, select a line/range, then press the
  line-select-local `a`. The target is captured as a root-relative file plus the normalized
  inclusive line/range; canceling the editor restores the exact selection.
- **Edit, delete, or clear**: press `A` for the annotation overview. Move with `↑`/`↓` or `j`/`k`,
  edit with `Enter`/`e`, delete one with `d`, or press uppercase `D` once to clear all immediately.
  `Esc`/`q` closes the overview.
- **Copy all**: press `y` in a non-empty overview. The deterministic, path/range-ordered export goes
  through OSC 52 and the overview closes; copying does not remove annotations.

Saving normalizes every run of Unicode whitespace or control characters to one ASCII space and
trims both ends. If that leaves the note empty, the editor stays open and shows a validation error;
the annotation is not added or changed.

A worktree switch (re-root) also clears all annotations, because their targets belong to the old
root, so it raises the same confirm quitting does (`y` copies them and switches, `Enter` switches
and discards, `Esc` cancels the switch and stays put). A failed switch or a same-root no-op changes
nothing and never confirms, since neither would lose anything. Closing and relaunching the viewer
always starts with an empty annotation store.

Because annotations live only for the session, anything that would discard them confirms first
rather than losing them to a stray key: quitting (`q`) and switching worktree (`W`) both raise it.
The dialog lists what would be lost, in the same rows the overview uses (the first eight, then
`+N more`, so it stays glanceable on a short terminal):

- **`y` copies them and continues**, which is usually where you were headed anyway: it writes the
  same `<file-annotations>` block the overview's `y` does, so you land ready to paste. If the
  clipboard write fails, the dialog stays open with the error rather than continuing and destroying
  what `y` promised to save.
- **The action's own key continues and discards them**: `q` when quitting, `Enter` when switching
  worktree (matching the picker's own confirm key).
- **`Esc` cancels**, returning to the viewer with the annotations intact. On a switch this cancels
  the switch itself, not just the discard.

The confirm only appears when the store is non-empty, so it never interrupts a session that did not
use annotations. Backing out of zoom with `q` is not a quit and raises no confirm. Set
`confirm_discard = false` in the config to skip it and discard immediately.

The exact concise copy format is:

```text
<file-annotations>
- README.md -> Clarify the fallback.
- src/app.rs:42 -> Explain the ignored result.
- src/controller/mod.rs:42-47 -> Why is this guarded twice?
</file-annotations>
```

File-level entries omit the line field, so ` -> ` (not `:`) separates the reference from the note:
the reference keeps its greppable `path:line` shape, and because `>` is escaped in both paths and
notes, the arrow is unambiguous even when a note contains a colon. Notes and paths escape `&`, `<`,
and `>` so the single outer wrapper cannot be spoofed; the copied block has no heading, blank lines,
root path, or trailing newline.

## Copying paths and lines

- **Copy a path**: `y` copies the selected file's **repo-relative** path (e.g. `src/app.rs`); `Y`
  copies its **absolute** path — handy for pasting into a prompt, a command, or an agent.
- **Copy a line reference or content**: with the content pane focused (or zoomed), `L` enters
  **line-select mode**. `Enter` copies a repo-relative reference like `src/app.rs:42` or
  `src/app.rs:42-58`; `y`/`Y` copy the selected line content itself. A mouse click-drag selects text
  character-by-character.

Both use the terminal's **OSC 52** clipboard escape, so the copy travels through herdr (and SSH) to
your real clipboard with no extra tooling. Full mechanics — extending a selection, wrapped-view
behavior, the OSC 52 caveat — are in the [keys reference](keys.md#copy-a-line-reference-or-line-content-l).

## Handing a file off

The viewer is read-only; to *act* on a file it hands off to another tool:

- **Edit** (`e`): open the selected file in the editor you set as `editor` in
  [config.toml](configuration.md) (or, with none set, your `$EDITOR`). The viewer suspends, runs the
  editor, and resumes when it exits. See [opening in an editor](keys.md#opening-in-an-editor).
- **Open with default app** (`O`): hand the file or directory to the OS default application (an
  image opens in the system viewer, and so on). Non-blocking — the viewer keeps running.
- **Reveal in file manager** (`R`): open Finder / Explorer / a Linux file manager with the entry
  highlighted where supported, so you can drag it out (e.g. into Slack).

All three are read-only hand-offs; the viewer never modifies a file itself. The `open` / `reveal`
commands are [configurable](configuration.md).

## Switching worktree

Press `W` to re-root the viewer at **another git worktree** of the repo without relaunching. It
opens a picker that marks the current worktree and pre-selects the one a herdr agent is working in,
so you can jump straight to an agent's checkout. `↑`/`↓` move, `Enter` switches, `Esc` cancels.
Read-only: it changes only *what you're viewing*, never the branch or any files.

## In-app help

Press `?` to open a view-only **help overlay** with sections for **Keybindings** (every action's
config-var name, effective keys, and description, marking your customizations), **What's New** (the
latest changelog, rendered as markdown), **Settings** (your effective configuration), and **About**
(version, repo, license, and update status). Keyboard and mouse; `Esc` or `q` closes it. A `? help`
hint rides the content pane's bottom border so the overlay is discoverable without already knowing
the key.

## Staying up to date

The viewer checks for a new release at most once a day (off the UI thread, over a read-only
`git ls-remote`) and, when you're behind, shows an "update available" banner naming the new version
and the update command. Press `u` to dismiss it for the session. The check and banner can be turned
off — see [install & updating](install.md#updating) and the `update_check`
[config key](configuration.md).

## Using the mouse

The mouse is additive and on by default: click a tree row to select it, double-click to
open/expand, use the wheel to scroll, drag a scrollbar or the divider, and drag over content text to
select-and-copy without any mode. The full gesture table is in the [keys reference](keys.md#mouse).
`Shift`+drag is deliberately left to your terminal's own native selection.
