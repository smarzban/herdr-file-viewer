# CONTEXT: glossary

Canonical vocabulary for this repo. Glossary only: no implementation detail, no specs.

## Project terms

- **viewer**: the plugin itself: the git-aware, read-only file viewer that runs as a
  herdr TUI pane. (Working name: herdr-file-viewer.)
- **tree**: the left column: a recursive, expandable directory tree of the current
  root, decorated with git status markers.
- **content pane**: the right column: shows the selected file as rendered markdown,
  a diff, or syntax-highlighted content, depending on the active view mode.
- **view mode**: which rendering the content pane is showing (rendered markdown /
  diff / content). Auto-selected per file, cyclable by the user.
- **diff baseline**: what a diff (and the meaning of "changed") is compared against:
  the base branch, or HEAD. Chosen by a context-smart default, toggleable.
- **base branch**: the branch a feature branch / worktree forked from (e.g. main);
  the baseline for reviewing the full body of work in a worktree.
- **changed-only filter**: a toggle that restricts the tree to files git reports as
  changed against the active diff baseline.
- **root**: the directory the tree is rooted at: the worktree root, or the pane's cwd
  when not in a worktree. The viewer does not browse above it.
- **re-root**: change the **root** at runtime: re-resolve it and rebuild the tree and
  git view from the new root, in place. Distinct from launch-time root resolution;
  read-only and session-only (a re-root never persists past relaunch).
- **worktree switch**: a **re-root** to another git worktree of the current repo,
  chosen from the **worktree picker**. The viewer's only re-root trigger today.
- **worktree picker**: the keyboard-summoned overlay listing the current repo's
  worktrees; selecting one performs a **worktree switch**.
- **active worktree**: the worktree the agent is working in, identified from herdr
  metadata when available and pre-selected in the **worktree picker**.
- **size cap**: the file-size limit (≥ 1 MB or ≥ 5,000 lines) above which the content
  pane shows a bounded, truncated preview instead of the whole file.
- **truncation notice**: the visible indicator shown when a file is previewed only up
  to the size cap.
- **renderer fallback**: the plain, unstyled text the content pane shows when a
  delegated renderer is unavailable, alongside a non-fatal notice.
- **focus-toggle**: the control that, in a narrow split (< 80 columns), gives the full
  pane width to either the tree or the content pane.
- **file finder**: the keyboard-summoned text-input overlay that fuzzy-matches a typed
  query against the **flat file index** and jumps to the chosen file (**reveal in tree**
  + render). Read-only; type-to-narrow, distinct from browsing the **tree** by hand.
- **flat file index**: the recursive, gitignore-aware list of every file under the
  **root** (not just the expanded **tree** nodes) that the **file finder** matches
  against; built fresh each time the finder opens, never persisted.
- **fuzzy match**: a case-insensitive subsequence match (query characters in order,
  gaps allowed) against a file's root-relative path, weighted toward basename hits; how
  the **file finder** ranks files.
- **reveal in tree**: expand a path's ancestor folders, set the **tree** cursor to it,
  and scroll it into view; how the **file finder** jumps to a chosen file.
- **search scope**: which files the **file finder** indexes: gitignore-respecting by
  default (skips ignored files and `.git/`), a single knob a future setting can widen to
  include ignored / hidden files.
- **search in file**: find text in the open file's **displayed content** via the `/`
  **prompt line**; `n`/`N` cycle matches. Read-only, within the open file, distinct from
  the **file finder**, which finds files by name.
- **go to line**: jump the **content pane** to a source line via the `:` **prompt line**;
  available only in the source-mapped (syntax/content) **view mode**, where a display row
  maps to a source line.
- **prompt line**: the one-line input shown at the bottom of the viewer for **search in
  file** (`/term`) and **go to line** (`:42`); distinct from the centered **file finder**
  and **worktree picker** overlays, so it does not cover the content being positioned.
- **current match**: the one **search in file** match the viewer is positioned on, visually
  distinguished from the other highlighted matches; `n`/`N` move it.
- **smartcase**: case-insensitive matching unless the query contains an uppercase
  character, in which case it is case-sensitive; how **search in file** matches.
- **line reference**: a clipboard string naming a location in a file as
  `<repo-relative-path>:<line>` (single) or `<repo-relative-path>:<start>-<end>` (range);
  what **line-select mode** copies, for pasting into an agent chat. The location companion
  to the **copy-path** keys (`y`/`Y`), which copy only a file path.
- **line-select mode**: an ephemeral **content pane** modal in which a movable **line
  marker** picks a line (or, with `Shift`, a contiguous range) to copy as a **line
  reference**; entered with `L` from content focus, exited with `Esc`. Available in the
  source-mapped **view mode** (auto-switches to it from rendered markdown / diff).
- **line marker**: the visible indicator of the currently selected line inside
  **line-select mode**; moved by `j`/`k`, arrows, or a mouse click.
- **opener**: an injected external command that hands the selected entry off to the
  host OS; the read-only external-effect seam shared by **open with default app** and
  **reveal in file manager**, modeled on the editor launcher. Non-blocking (fire-and-
  forget), unlike the terminal-handover editor hand-off (`e`).
- **open with default app**: the `O` hand-off: launch the selected entry (file or
  directory) in the OS default application (e.g. an image in the system image viewer).
- **reveal in file manager**: the `R` hand-off: surface the selected entry in the OS
  file manager (Finder / Explorer / a Linux file manager), highlighted where the
  platform supports it, so it can be dragged out of the viewer.
- **config file**: the viewer's read-only TOML settings file, loaded once at startup
  from the herdr-provided `$HERDR_PLUGIN_CONFIG_DIR` (XDG fallback when absent). It is
  **input, not state**: the app reads it and never writes it.
- **effective setting**: the value a setting actually resolves to after precedence is
  applied: the **config file** key if present, else the environment variable, else the
  built-in default (config > env > default).
- **command override**: a **config file** value that replaces a built-in external
  command (the **opener**s, the renderers, or the editor). Written as a string and split
  into argv by the same quote-aware tokenizer used for `$EDITOR`; no shell is invoked.
- **Settings section**: the display-only section of the help overlay that lists the
  **effective setting**s; it shows configuration, it does not edit it.
- **scroll step**: how many content lines (or **file finder** list items, or help-overlay
  lines) the mouse wheel advances per wheel event. Set by the `scroll_lines` **config file**
  key (config > default; default 3, clamped to the range 1 to 10). The directory tree is
  unaffected: it always advances one row per wheel event.
- **tree width**: the directory tree column's share of the viewer pane, as a percent (the
  content pane takes the rest). Set by the `tree_width` **config file** key (config > default;
  default 30, clamped to the range 20 to 80). Seeds the startup split; the live grow/shrink keys
  and the divider drag still adjust it within a session. This is the split INSIDE the viewer's
  pane, distinct from the size of the herdr **pane** itself (the host decides that).
- **tree position**: which side of the content pane the directory tree is drawn on — `left`
  (default, today's layout) or `right`. Set by the `tree_position` **config file** key
  (config > default). An unrecognized value falls back to `left`.
- **tree column cap**: the maximum tree width in character columns (not a percent). The tree is
  drawn at `min(`**tree width**`% of the pane, tree_max_cols)`, so a wide pane (a full tab, a big
  monitor) keeps the tree compact instead of over-allocating blank space. Set by the `tree_max_cols`
  **config file** key (config > default; default 30, clamped to the range 10 to 1000; a large value
  effectively disables the cap). A default ceiling only: a hand resize (divider drag or the
  grow/shrink keys) lifts it for the session.
- **keybinding registry**: the single data-driven table binding each global viewer action
  to its default key(s) and a human description; the source of truth the input dispatcher
  decodes from and the help overlay and README both derive from.
- **intent name**: the stable snake_case identifier of a global action, used as the key of
  a `[keys]` **config file** entry (e.g. `refresh`, `nav_up`).
- **key spec**: the string, or array of strings, a user writes to name the key(s) an action
  binds to (e.g. `"g"`, `["w", "Up"]`); limited to the modifier-free key surface (no
  `Ctrl`/`Alt`).
- **custom binding**: an **effective setting** style binding that came from a user's
  `[keys]` entry rather than the default **keybinding registry**; marked as such in the
  **Keybindings section**.
- **Keybindings section**: the display-only section of the help overlay listing every
  action with its effective key(s) and description (a sibling of the **Settings section**).

## herdr terms (host platform)

- **worktree**: a herdr-managed git worktree, typically where an agent does its work.
- **action**: a herdr plugin command bound to a keybinding, run in a workspace
  context; how the user summons the viewer.
- **pane**: a herdr terminal surface a plugin can own (overlay / split / tab /
  zoomed). The viewer runs in a split pane.
- **platform**: an OS the plugin declares support for in its manifest `platforms`
  set (linux / macos / windows).
- **platform filter**: herdr skipping a manifest `[[build]]` whose declared
  **platform** doesn't match the host, and returning `platform_unsupported` for an
  unsupported action or pane; what lets unix and Windows build/launcher entries
  coexist in one manifest.
- **preview channel**: herdr's pre-release update channel. Native Windows herdr
  binaries ship only here, so a Windows user of the **viewer** must be on it.
