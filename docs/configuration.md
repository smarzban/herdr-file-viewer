# Configuration

An optional TOML config file lets you override the editor, the renderer/opener commands, a couple
of startup toggles, the tree layout, and the keybindings. **Read-only input** — the viewer never
writes this file; edit it in your own editor and relaunch to pick up changes (there is no in-app
settings editor). You can see what's currently in effect any time in the `?` help overlay's
**Settings** section.

**Quick start.** A fully-commented [`config.example.toml`](../config.example.toml) ships in the
plugin folder, documenting every setting. You never have to guess where the live file goes: under
herdr, `herdr plugin config-dir herdr-file-viewer` prints the exact directory herdr keeps it in.
Copy the example there as `config.toml` in one line:

```bash
cp "$(herdr plugin list --json | jq -r '.result.plugins[]|select(.plugin_id=="herdr-file-viewer").plugin_root')/config.example.toml" \
   "$(herdr plugin config-dir herdr-file-viewer)/config.toml"
```

No `jq`? Run `herdr plugin list` to see the plugin folder (shown in brackets) and copy from there:
`cp <plugin-folder>/config.example.toml "$(herdr plugin config-dir herdr-file-viewer)/config.toml"`.

Then uncomment the lines you want and relaunch. Copying it as-is changes nothing (every line is
commented out). However you copy it, **rename the copy to `config.toml`**: the `config.example.toml`
filename itself is never read.

## File location

When run under herdr, the config lives at `$HERDR_PLUGIN_CONFIG_DIR/config.toml` — herdr provides
that directory (on Linux it is
`~/.config/herdr/plugins/config/herdr-file-viewer/`, so the file is that path plus `config.toml`).
Run standalone (outside herdr), it
falls back to `$XDG_CONFIG_HOME/herdr-file-viewer/config.toml`, defaulting to
`~/.config/herdr-file-viewer/config.toml` when `XDG_CONFIG_HOME` isn't set. A missing file is the
normal case — every key falls back to its default.

## Precedence

A config key always wins. Only two keys also have an environment-variable fallback tier below the
config key and above the built-in default — `editor` (`$EDITOR`) and `update_check`
(`$HERDR_FILE_VIEWER_NO_UPDATE_CHECK`) — giving those two a `config > env > default` chain. Every
other key (`markdown`, `diff`, `syntax`, `open`, `reveal`, `hide_dotfiles`, `confirm_discard`,
`scroll_lines`, `tree_width`, `tree_position`, `tree_max_cols`, `preview_max_lines`,
`preview_max_kib`) has no
applicable environment variable; for those it's `config > default` only.

## Keys

```toml
# ~/.config/herdr-file-viewer/config.toml (or the herdr-provided path above)

editor = "code --wait"      # command to open a file with `e` (overrides $EDITOR)

markdown = "glow -s dark -w 0 -"   # override the markdown / diff / syntax renderers
diff = "delta"                     # (defaults: glow / delta / bat)
syntax = "bat --color=always --style=numbers --paging=never --file-name={name} -"

open = "xdg-open"           # override the `O` open-with / `R` reveal-in-file-manager commands
reveal = "nautilus"

hide_dotfiles = false       # true to hide dotfiles at startup (the `.` key still toggles)
update_check = true         # false to disable the once-a-day update check
confirm_discard = true      # false to discard annotations without confirming (on quit / worktree switch)
scroll_lines = 3            # mouse-wheel step (content/search/help), a 1 to 10 scale: 1 slow · 3 medium · 6 fast · 10 max
tree_width = 30             # tree column's share of the viewer pane, percent 20-80 (content takes the rest)
tree_max_cols = 30          # HARD CAP in columns; the SMALLER of this and tree_width% wins (raise both to widen)
tree_position = "left"      # which side the directory tree sits on: "left" (default) or "right"

preview_max_lines = 10000   # show at most this many lines before a truncated preview (100–100000)
preview_max_kib = 1024      # ...or this size before truncating, in KiB (1024 = 1 MB; 64–65536)
```

`tree_width` and `tree_max_cols` **together** decide the tree's startup width, and the **smaller of
the two wins**: the tree is drawn at `min(tree_width% of the pane, tree_max_cols)`. So if you set
`tree_width = 50` and nothing changes, `tree_max_cols` (default 30 columns) is capping it: raise
`tree_max_cols` too, or set it high to switch the cap off. The cap is a **column** count, not a
percent; it exists so a full-terminal tab or a wide monitor gives the extra room to the content pane
instead of a mostly-blank tree (it only bites past ~100 columns). `tree_position` puts the tree on
the `left` (default) or `right`. All three set the **startup** split inside the viewer's own pane
(not the herdr pane, which the host decides); you can still resize live with the grow/shrink keys or
by dragging the divider, and an explicit resize lifts the cap.

`preview_max_lines` and `preview_max_kib` cap how much of a file the content pane shows: a file is
displayed in full until it exceeds **either** cap, then the pane shows a truncated preview with a
`⚠ Truncated preview` notice (the same bound also applies to a large diff). Truncation fires on
whichever cap is hit **first**. For typical source code the **line** cap bites first; the **size**
cap (in KiB, `1024` = 1 MB) mainly guards minified or generated files (bundles, big JSON, logs) and
also bounds how much is ever read from disk, so a giant or hostile file is never slurped whole. Raise
either to view bigger files (`preview_max_lines` up to `100000`, `preview_max_kib` up to `65536` =
64 MB); both clamp into range, and a very large value can make the pane slower to render.

One caveat for **diffs**: a diff is additionally bounded at ~4 MB by the git-capture step, independent
of `preview_max_kib`. So raising `preview_max_kib` above ~4 MB widens how much *file content* is shown
but not how much of a very large *diff* is (a diff past that bound is shown up to ~4 MB).

`confirm_discard` guards the one piece of state the viewer can lose. Annotations (`a` / `A`) are
session-only, so both quitting (`q`) and switching worktree (`W`) discard them. By default either
raises a confirm listing what would be lost: `y` copies them to the clipboard and continues, the
action's own key continues and discards (`q` to quit, `Enter` to switch), and `Esc` cancels. Set it
to `false` to skip the confirm and discard immediately. It only appears when annotations are
actually held, so leaving it on costs nothing in a session that never uses them. See
[annotating files and ranges](usage.md#annotating-files-and-ranges).

## Command values

Command values (`editor`, `markdown`, `diff`, `syntax`, `open`, `reveal`) are **split into
arguments** the way a shell would for simple cases — whitespace splits, double-quotes group a
path with spaces — but **no shell is invoked**. `editor` / `open` / `reveal` get the target
**path** appended as the final argument; the **renderers** (`markdown` / `diff` / `syntax`)
instead get the file **content on stdin** and your value **replaces** the whole default command
(flags aren't merged), so a custom renderer must read stdin (glow and bat need a trailing `-`)
and set its own flags — the token `{name}` is substituted with the file name.

**Known limitation:** the full-file-diff view derives its line-numbered gutter from the `diff`
command by appending delta's `--line-numbers` flag; if you point `diff` at a tool that rejects
that flag (nonzero exit or spawn failure), the full-file-diff view falls all the way back to
plain, unrendered diff text (with a notice), not just a missing gutter. Renderer timeouts and
other limits aren't configurable.

## Keybindings

A `[keys]` table remaps the viewer's global keys, keyed by **intent name** (the stable snake_case
id of an action, e.g. `refresh`, `nav_up`, `switch_worktree`). Each value is a **key spec**: a
single string, or an array of strings, naming the key(s) that action should answer to. An entry
**replaces** that action's default key set, so list every key you want it to keep.

```toml
[keys]
refresh = "g"               # a single key: `g` refreshes, and the default `r` no longer does
nav_up = ["w", "Up"]        # an array: bind several keys at once (the default `k` is dropped)
switch_worktree = "F2"      # a named key
```

### Every remappable action

These are all the actions you can put in `[keys]`, keyed by intent name, with their default key(s).
The `?` help overlay's **Keybindings** section shows the same list live (and marks the ones you have
customized).

| Group | Intent name (`[keys]` id) | Default key(s) | Action |
| --- | --- | --- | --- |
| **Navigation** | `nav_up` | `Up`, `k` | Move the tree cursor up one row |
| | `nav_down` | `Down`, `j` | Move the tree cursor down one row |
| | `expand` | `Right`, `l` | Expand the selected directory |
| | `collapse` | `Left`, `h` | Collapse the selected directory |
| | `activate` | `Enter` | Activate the selection: expand/collapse a directory, or open a file |
| **View & layout** | `open_fullscreen` | `Z` | Toggle full-screen reading of the selected file |
| | `cycle_view` | `v` | Cycle the content pane's view mode |
| | `toggle_focus` | `Tab` | Move focus between the tree and content columns |
| | `shrink_tree` | `<` | Narrow the tree column |
| | `grow_tree` | `>` | Widen the tree column |
| | `toggle_wrap` | `w` | Force content-line wrapping on or off |
| | `toggle_zoom` | `z` | Hide the tree so content fills the frame, or restore the split |
| | `tree_scroll_left` | `H` | Scroll the tree pane left |
| | `tree_scroll_right` | `L` | Scroll the tree pane right |
| **Git & filters** | `toggle_ignore` | `i` | Reveal or hide gitignored files |
| | `toggle_hidden` | `.` | Hide or reveal dot-prefixed (hidden) files and folders |
| | `toggle_changed_only` | `c` | Restrict the tree to changed files, or restore the full tree |
| | `toggle_baseline` | `b` | Switch the diff baseline between base-branch and `HEAD` |
| | `refresh` | `r` | Re-read git state and re-render |
| **Open & copy** | `open_in_editor` | `e` | Hand the selected file off to an external editor |
| | `open_with_app` | `O` | Open the selected entry with the OS default application |
| | `reveal_in_file_manager` | `R` | Reveal the selected entry in the OS file manager |
| | `copy_repo_path` | `y` | Copy the selected node's repo-relative path to the clipboard |
| | `copy_abs_path` | `Y` | Copy the selected node's absolute path to the clipboard |
| **Annotations** | `add_annotation` | `a` | Add an in-memory annotation for the selected file |
| | `show_annotations` | `A` | Open the session annotation overview |
| **Search & jump** | `open_finder` | `f` | Open the go-to-file fuzzy finder |
| | `open_go_to_line` | `:` | Open the go-to-line prompt |
| | `open_search` | `/` | Open the in-file search prompt |
| | `next_match` | `n` | Jump to the next search match (wraps) |
| | `prev_match` | `N` | Jump to the previous search match (wraps) |
| **Session** | `dismiss_update` | `u` | Dismiss the update-available banner for this session |
| | `switch_worktree` | `W` | Open the worktree picker to re-root at another git worktree |
| | `show_help` | `?` | Open the in-app help overlay (What's New and About) |
| | `close` | `q`, `Esc` | Close the viewer and return to the prior pane |

`Esc` always closes the viewer even if you rebind `close` — that floor can't be rebound away (see
below). Keys handled inside a modal are fixed and not remappable. That includes line-select `a`
(add an annotation for the selected line/range), annotation-editor `←`/`→`/`Home`/`End`/`Enter`/`Esc`,
and annotation-overview `j`/`k`/arrows, `Enter`/`e`, `d`, uppercase `D`, `y`, `Esc`/`q`, as well as
the finder and `:` / `/` prompts. Remapping a global action never changes these local modal keys.

**Bindable keys** are the modifier-free surface the viewer already uses: any printable or shifted
character (`g`, `<`, `?`, and capitals such as `A`, `D`, and `W` are each their own key), plus the named keys
`Tab`, `Enter`, `Esc`, the four arrows, `Home`, `End`, `PageUp`, `PageDown`, `Space`, `Backspace`,
`Delete`, `Insert`, and `F1` through `F12` (named keys are matched case-insensitively). There are
**no `Ctrl` / `Alt` chords**: a chord never fires a viewer action, so terminal combinations like
`Ctrl+C` always pass straight through.

**Precedence is `config > default`:** a `[keys]` value replaces the action's built-in keys, and any
action you don't list keeps its defaults unless an explicit binding claims one of those default
keys. For example, `refresh = "a"` keeps `a` for Refresh and leaves `add_annotation` unbound;
`show_help = "A"` similarly leaves `show_annotations` unbound. The Keybindings help section shows
such displaced actions as `(unbound)` rather than stealing the user's configured key. The load is
defensive and never crashes the viewer: an
unknown intent name, an unbindable key, or two actions claiming the same key is ignored for those
entries only (their defaults are kept). Invalid TOML in the config (a syntax error, or a wrong-typed
value such as `refresh = 42`) is the same whole-file fallback the rest of the config uses: the viewer
ignores the entire file and falls back to built-in defaults, and the `?` overlay flags that the
config was malformed. Whatever you configure, **`Esc` always closes** the viewer: that floor cannot be
rebound away, so you can never strand yourself (you may still move the `q` Close key or any other
action). Only the global keys are remappable; keys handled inside a modal (including line-select
and the annotation editor/overview) keep their fixed keys.

See your bindings in effect any time in the `?` help overlay's **Keybindings** section. It groups
the actions into sections and shows, for each, its config-var name (the `[keys]` id you type to
remap it), its effective key(s), and its description, marking the ones you have customized. The full
default key list lives in the [keys reference](keys.md).
