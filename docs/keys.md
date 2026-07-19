# Keys & mouse

The complete key and mouse reference for the viewer. For a guided tour of what each feature
*does*, see the [usage guide](usage.md); to remap any of these keys, see
[configuration → Keybindings](configuration.md#keybindings).

The viewer is **keyboard-first**: every function has a key and nothing requires a mouse. The mouse
is additive and on by default.

## Keys

| Key | Action |
| --- | --- |
| `↑` / `k`, `↓` / `j` | Move the tree cursor, or **scroll the content pane** vertically when it is focused |
| `→` / `l` | Expand the selected directory, or **scroll the content pane right** when it is focused |
| `←` / `h` | Collapse the selected directory, or **scroll the content pane left** when it is focused |
| `H` (Shift+`h`) | Scroll the **tree** pane left (long / deeply-nested rows), inert unless the tree is focused |
| `L` (Shift+`l`) | Focus-gated: with the **tree** focused, scroll it right (long / deeply-nested rows); with the **content pane** focused (or zoomed), enter **line-select mode** to select lines and copy either a `file:line` reference or the content itself (see [below](#copy-a-line-reference-or-line-content-l)) |
| _line-select mode_ | `j`/`k` (or `↑`/`↓`) move the marker, `Shift`+move (`J`/`K`, Shift+`↑`/`↓`) extends a line selection; **click-drag** with the mouse selects **text** (character-granular); `Enter` copies the `path:line` / `path:start-end` **reference**, `y`/`Y` copies the selected **content**, `Esc` exits |
| `Enter` | Activate the selection: expand/collapse a directory, or open a file in **zoom mode** (content full-screen) |
| `i` | Toggle gitignored files |
| `.` | Toggle hidden (dot-prefixed) files and folders |
| `c` | Toggle changed-files-only |
| `b` | Toggle the diff baseline (base branch ⇄ `HEAD`) |
| `D` (Shift+`d`) | Cycle diff presentation — `delta` unified → side-by-side → plain, unstyled `git diff` text → back to unified (in Diff/FullDiff views) |
| `v` | Cycle the content view mode |
| `e` | Open the selected file in `$EDITOR` (see [Opening in an editor](#opening-in-an-editor)) |
| `O` (Shift+`o`) | **Open with default app**: hand the selected file or directory to the OS default application (e.g. an image opens in the system viewer). Read-only hand-off; non-blocking (the viewer keeps running) |
| `R` (Shift+`r`) | **Reveal in file manager**: open the OS file manager (Finder / Explorer / a Linux file manager) with the selected entry highlighted where supported, so you can drag it out (e.g. into Slack). Read-only hand-off |
| `f` | **Go to file**: open a fuzzy finder over every file in the tree; type to filter, `↑` / `↓` move, `Enter` opens the selected file, `Esc` cancels (`←` / `→` scroll long paths) |
| `:` | **Go to line**: open a prompt and jump the content pane to a source line by number (`Enter` jumps, `Esc` cancels; out-of-range clamps to the last line). Works in any view; in a rendered-markdown or diff view, confirming switches to the line-numbered content view and jumps there |
| `/` | **Search in file**: open a prompt and highlight every match in the content pane as you type; `Enter` commits the search (highlights persist), `Esc` clears it and restores the scroll. Smartcase (a lowercase query is case-insensitive; a capital makes it case-sensitive). Works in any view |
| `n` / `N` (Shift+`n`) | After a committed search, jump to the **next** / **previous** match and scroll it into view, wrapping at the ends with a notice |
| `y` | Copy the selected file's **repo-relative** path to the clipboard (e.g. `src/app.rs`) |
| `Y` | Copy the selected file's **absolute** path to the clipboard |
| `Tab` | Move focus between the tree and content columns |
| `<` / `>` | Narrow / widen the tree column (move the divider) |
| `w` | Toggle line wrapping for the content pane. For rendered markdown this switches between the fit-to-pane view (wide tables sized to fit, over-long cells shown as `…`) and a wide view that renders tables at full width and scrolls horizontally (`←`/`→`) so you can read every cell |
| `z` | Zoom: hide the tree so the content pane fills the frame; press again (or `q`/`Esc`) to restore the two-column layout |
| `Z` (Shift+`z`) | **Full-screen a file** (toggle): open the selected file like `Enter` _and_ zoom the viewer's herdr pane to fill the whole terminal, so the file takes over the entire screen instead of just the split. Press `Z` again (or `Esc`/`q`, or `z`) to return to the normal two-column split; switching worktree or quitting also restores the pane. On a directory it just expands/collapses like `Enter`; falls back to the in-pane zoom when the host isn't herdr |
| `r` | Refresh git state: pick up changes made outside the viewer (a merge / pull / commit elsewhere) |
| `W` (Shift+`w`) | **Switch worktree**: open a picker of the repo's git worktrees and re-root the viewer to the one you pick (read-only; no branch checkout). Marks the current worktree and pre-selects the one with an active herdr agent; `↑`/`↓` move, `←`/`→` scroll long paths, `Enter` switches, `Esc` cancels |
| `?` (Shift+`/`) | Open the **help overlay**: What's New (latest changelog, rendered markdown) + About (version, repo, license, update status); `Esc` / `q` closes it |
| `u` | Dismiss the "update available" banner for this session |
| `q` / `Esc` | Back out of zoom if zoomed; otherwise close the viewer and return to the prior pane |

These are the **default** keys. Remap any of them with a `[keys]` table in the
[config file](configuration.md#keybindings).

`Tab` to the content pane, then the arrow keys (or `h`/`j`/`k`/`l`) scroll it in all four
directions; `Tab` back to the tree to move between files. Long lines wrap in prose (markdown /
plain text); diffs and code keep their original lines so columns stay aligned. Scroll
sideways with `←`/`→`, or press `w` to wrap them instead. Rendered markdown fits the pane by
default (wide tables sized to fit, over-long cells shown as `…`); press `w` for a wide view that
renders tables at full width and scrolls sideways so you can read every cell. The layout reflows
automatically when the pane is resized.

**Git state stays current.** The viewer re-reads git status when the pane **regains focus**, so
changes you make outside it (a merge, pull, or commit in another pane) show up automatically; `r`
forces a full refresh on demand. (Focus-refresh updates the tree's status without disturbing your
content scroll.)

Character keys act only when no control chord is held (so terminal chords like `Ctrl+C` are
never intercepted); `Shift` is permitted, for keys such as `<` and `>` (and `y`/`Y`, `W`, `N`,
`O`, `R`, `Z`, `?`, `H`/`L`, and `J`/`K` in line-select mode).

### Copy a path (`y` / `Y`)

`y` copies the selected file's repo-relative path; `Y` copies its absolute path, handy for pasting
into a prompt, a command, or an agent. The copy uses the terminal's **OSC 52** clipboard escape, so
it travels through herdr (and SSH) to your real clipboard with no extra tooling. A confirmation
appears in the notices strip. If nothing lands on your clipboard, your terminal likely needs OSC 52
/ clipboard-write enabled (e.g. in tmux, `set -g set-clipboard on`).

### Copy a line reference or line content (`L`)

With the content pane focused (or zoomed), `L` enters **line-select mode**: a marker lands on the
top visible line, `j`/`k` (or `↑`/`↓`) move it, and holding `Shift` (`J`/`K`, or Shift+`↑`/`↓`)
extends a whole-line selection. Or **click-drag with the mouse** to select **text**
character-by-character: press where the selection starts, drag to where it ends (the pane scrolls if
you drag past an edge), release; the selected characters are highlighted as you go. One selection,
two products:

- **`Enter` copies a repo-relative reference**: `src/app.rs:42` for a single line,
  `src/app.rs:42-58` for a range (a mouse selection references the lines it spans), ready to
  paste into an agent chat or an issue to point at exact lines.
- **`y` / `Y` copy the content itself**: for a line selection, the lines joined by newlines; for
  a mouse text selection, exactly the characters you dragged over. The syntax view's line-number
  gutter is stripped and indentation is preserved, so it pastes as real code.

A confirmation notice names what was copied. Both copies use the same **OSC 52** path as the
tree's `y`/`Y`. `Esc` leaves the mode.

`Shift`+mouse is deliberately left alone so your terminal's own native selection/copy still works:
most terminals reserve `Shift`+drag for exactly that. Selection works in wrapped views too (the
`w` toggle): the click maps through the same wrapping the pane draws with, so the caret lands on
the character under the cursor. Because selection only maps onto the source, entering line-select
from a rendered-markdown or diff view first switches that file to the line-numbered content view.
With the **tree** focused, `L` keeps its tree horizontal-scroll behavior instead. The mode is gated
on which pane has focus.

## Mouse

The viewer is keyboard-first; the mouse is additive and on by default:

| Gesture | Action |
| --- | --- |
| **Click** a tree row | Select it (focus the tree) |
| **Double-click** a folder | Expand / collapse it (same as `Enter`) |
| **Double-click** a file | Open it in **zoom mode**: content full-screen (same as `Enter`); the editor is the `e` key |
| **Wheel** over the content pane | Scroll it vertically; over the tree, move the selection |
| **Horizontal wheel / swipe** | Scroll the content, or the tree, sideways (terminal-dependent, see below) |
| **Drag** a scrollbar | Scroll that pane: drag ↕ on a vertical bar, ↔ on a horizontal bar; pressing the track jumps there |
| **Drag** the divider | Resize the tree / content split |
| **Drag** over the content text | **Select and copy text**: the selection highlights character-by-character as you drag (auto-scrolling past an edge) and is copied to the clipboard on release; no mode needed. Works in wrapped views (prose/markdown) too. `Esc`, a click elsewhere, or switching files clears the highlight |

**`Shift`+drag is left to your terminal**, so its native select-and-copy still works while the
viewer owns ordinary clicks: herdr reserves `Shift`+mouse for exactly this. (herdr forwards
mouse events to the pane because the viewer requests capture.)

**Horizontal mouse scroll is terminal-dependent**: it works only where your terminal emits
horizontal-scroll events (`ScrollLeft` / `ScrollRight`); many terminals send nothing for a
sideways trackpad swipe. The `←` / `→` keys always scroll the content sideways, and `H` / `L`
always scroll the tree sideways, regardless of terminal.

The mouse-wheel step is configurable — see [`scroll_lines`](configuration.md).

## Opening in an editor

`e` opens the selected file in an external editor; the viewer suspends, runs the editor, and resumes
when it exits. The viewer never edits a file itself. Choose the editor two ways — the config key is
the reliable one:

- **Recommended: set `editor` in [config.toml](configuration.md)** (e.g. `editor = "code --wait"`,
  or `"vim"`). It takes precedence over `$EDITOR` and sidesteps the server-environment gotcha below
  entirely — no shell-rc edits, no server restart.
- **Fallback: `$EDITOR`.** With no `editor` configured, `e` uses the `$EDITOR` environment variable
  (e.g. `vim`, or `"code --wait"` for editors that fork). Zero config if it's already set.

If `e` says "no editor configured," neither source is set: add `editor` to your config (simplest),
or export `$EDITOR` where the herdr server can see it (expand below).

<details>
<summary><strong>Why <code>$EDITOR</code> sometimes isn't seen (and how to fix it)</strong></summary>

The viewer reads `$EDITOR` from the **herdr server's** environment (the server spawns every pane),
*not* from the shell you happen to be attached from. So if `$EDITOR` is set in your interactive
shell but the server was started without it (common with `mosh`, `systemd`, or any login manager
that doesn't source your shell startup files), the viewer won't see it. Setting `editor` in the
[config file](configuration.md) sidesteps all of this; to make `$EDITOR` itself work:

1. **Export `$EDITOR` in the startup file your server's launch actually reads.** Pick the line(s)
   that match how herdr starts on your machine:

   ```bash
   # zsh: interactive shells read ~/.zshrc; ~/.zshenv is read by *every* zsh invocation
   echo 'export EDITOR=vim' >> ~/.zshrc

   # bash: add to both, so interactive and login shells agree
   echo 'export EDITOR=vim' >> ~/.bashrc
   echo 'export EDITOR=vim' >> ~/.profile

   # mosh / `sh -lc` / any POSIX login-shell launch (e.g. herdr started over SSH+mosh)
   echo 'export EDITOR=vim' >> ~/.profile
   ```

   If you're unsure which applies, adding it to **`~/.profile`** covers the login-shell launch
   paths; keep it in your shell's rc too for interactive use.

2. **Restart the herdr server** so it re-reads the environment: `reload-config` and `prefix+q`
   are **not** enough (the first doesn't re-read env; the second only quits the client and leaves
   the detached server running with its old environment):

   ```bash
   herdr server stop   # stops the background daemon: ends all panes, so finish in-flight work first
   herdr               # relaunch from a shell where `echo $EDITOR` already prints your editor
   ```

3. **Verify:** open any shell pane *inside* herdr and run `echo $EDITOR`. Once that prints your
   editor (it was empty before), `e` will open it.

</details>
