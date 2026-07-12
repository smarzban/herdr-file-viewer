# Configuration

An optional TOML config file lets you override the editor, the renderer/opener commands, a couple
of startup toggles, the tree layout, and the keybindings. **Read-only input** — the viewer never
writes this file; edit it in your own editor and relaunch to pick up changes (there is no in-app
settings editor). You can see what's currently in effect any time in the `?` help overlay's
**Settings** section.

**Quick start:** a fully-commented [`config.example.toml`](../config.example.toml) ships in the
plugin folder documenting every setting. Copy it to the config path below, **rename it to
`config.toml`**, uncomment the lines you want, and relaunch — copying it as-is changes nothing
(every line is commented out).

## File location

When run under herdr, the config lives at `$HERDR_PLUGIN_CONFIG_DIR/config.toml` — herdr provides
that directory (on Linux it resolves to
`~/.config/herdr/plugins/config/herdr-file-viewer/config.toml`). Run standalone (outside herdr), it
falls back to `$XDG_CONFIG_HOME/herdr-file-viewer/config.toml`, defaulting to
`~/.config/herdr-file-viewer/config.toml` when `XDG_CONFIG_HOME` isn't set. A missing file is the
normal case — every key falls back to its default.

## Precedence

A config key always wins. Only two keys also have an environment-variable fallback tier below the
config key and above the built-in default — `editor` (`$EDITOR`) and `update_check`
(`$HERDR_FILE_VIEWER_NO_UPDATE_CHECK`) — giving those two a `config > env > default` chain. Every
other key (`markdown`, `diff`, `syntax`, `open`, `reveal`, `hide_dotfiles`, `scroll_lines`,
`tree_width`, `tree_position`, `tree_max_cols`) has no applicable environment variable; for those
it's `config > default` only.

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
scroll_lines = 3            # mouse-wheel step (content/search/help), a 1 to 10 scale: 1 slow · 3 medium · 6 fast · 10 max
tree_width = 30             # tree column's share of the viewer pane, percent 20-80 (content takes the rest)
tree_position = "left"      # which side the directory tree sits on: "left" (default) or "right"
tree_max_cols = 30          # cap the tree at this many columns so it stays compact on a wide pane
```

`tree_width` / `tree_position` set the **startup** tree/content split inside the viewer's own pane
(not the size of the herdr pane, which the host decides). You can still resize the split live with
the grow/shrink keys or by dragging the divider; the config just seeds the initial value.
`tree_max_cols` is a **column** ceiling (not a percent): the tree is drawn at
`min(tree_width% of the pane, tree_max_cols)`, so a full-terminal tab gives the extra width to the
content pane instead of a mostly-blank tree. It only bites past ~100 columns, and dragging the
divider or the grow/shrink keys lifts it (an explicit resize wins); set it high to disable.

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

**Bindable keys** are the modifier-free surface the viewer already uses: any printable or shifted
character (`g`, `<`, `?`, and capitals such as `W` are each their own key), plus the named keys
`Tab`, `Enter`, `Esc`, the four arrows, `Home`, `End`, `PageUp`, `PageDown`, `Space`, `Backspace`,
`Delete`, `Insert`, and `F1` through `F12` (named keys are matched case-insensitively). There are
**no `Ctrl` / `Alt` chords**: a chord never fires a viewer action, so terminal combinations like
`Ctrl+C` always pass straight through.

**Precedence is `config > default`:** a `[keys]` value replaces the action's built-in keys, and any
action you don't list keeps its defaults. The load is defensive and never crashes the viewer: an
unknown intent name, an unbindable key, or two actions claiming the same key is ignored for those
entries only (their defaults are kept). Invalid TOML in the config (a syntax error, or a wrong-typed
value such as `refresh = 42`) is the same whole-file fallback the rest of the config uses: the viewer
ignores the entire file and falls back to built-in defaults, and the `?` overlay flags that the
config was malformed. Whatever you configure, **`Esc` always closes** the viewer: that floor cannot be
rebound away, so you can never strand yourself (you may still move the `q` Close key or any other
action). Only the global keys are remappable; keys handled inside a modal (the finder query, the
`:` / `/` prompt, line-select mode) keep their own keys.

See your bindings in effect any time in the `?` help overlay's **Keybindings** section. It groups
the actions into sections and shows, for each, its config-var name (the `[keys]` id you type to
remap it), its effective key(s), and its description, marking the ones you have customized. The full
default key list lives in the [keys reference](keys.md).
