# Summoning the viewer

How the viewer gets opened: the open actions, the idempotent launcher, split vs. tab, and the
`--remote` caveat. For a quick "install then bind a key," see the [Quick start](../README.md#quick-start);
once it's open, see the [usage guide](usage.md) and [keys reference](keys.md).

The viewer opens **only** in response to an explicit action. There are no event hooks and no
automatic invocation. The manifest declares a `[[panes]]` entry (the split-pane viewer) and an
`[[actions]]` whose command opens it:

```toml
[[panes]]
id = "file-viewer"
placement = "split"
command = ["./target/release/herdr-file-viewer"]

[[actions]]
id = "open-file-viewer"
title = "Open file viewer"
command = ["bash", "scripts/open-file-viewer.sh"]   # opens the pane via the herdr CLI
```

Summon it by invoking the action:

```bash
herdr plugin action invoke open-file-viewer --plugin herdr-file-viewer
```

It opens the viewer in a **split** pane beside your current work. The launcher
(`scripts/open-file-viewer.sh`, used by both the action and any keybinding) is **idempotent**,
scoped to the current tab, so invoking it repeatedly is *launch-or-focus-or-toggle*:

- no viewer pane open in this tab → open a split (focused)
- a viewer pane open but not focused → focus it
- the viewer pane already focused → close it (herdr has no hide-without-close; reopening just
  re-walks the tree)

**One-press access: bind a key.** herdr's `config.toml` binds keys to commands; point a
`plugin_action` binding at the installed plugin's qualified action id. herdr invokes the action
directly, so no detached shell or hard-coded path is involved:

```toml
[[keys.command]]
key = "prefix+f"   # any herdr key syntax, e.g. ctrl+b then f
type = "plugin_action"
command = "herdr-file-viewer.open-file-viewer"
description = "open file viewer in split"
```

Reload with `herdr server reload-config`. Pressing the key then opens / focuses / hides the
viewer via the same idempotent launcher.

## Split beside or below

By default the split opens to the **right** of the pane you pressed the key in. Set
[`open_direction`](configuration.md) to put it **below** instead, so your terminal keeps the top
half and the viewer takes the bottom:

```toml
# <plugin config dir>/config.toml   (`herdr plugin config-dir herdr-file-viewer`)
open_direction = "down"
```

No reload is needed — the launcher reads it on each summon, so the next `prefix+f` opens
underneath. It applies to the split actions only (`open-file-viewer` and `open-file-viewer-at`): a
tab has no direction, so the tab actions ignore it.

## Open in a tab instead of a split

A second action, `open-file-viewer-tab`, opens the viewer in its **own tab**
(`scripts/open-file-viewer-tab.sh`, `--placement tab`). Its launcher is idempotent *across the tabs
of the current workspace*, *open-or-switch-or-toggle*:

- no viewer for this repo in this workspace → open it in a new tab (focused)
- a viewer **showing this repo** in another tab of this workspace → **switch to that tab** (never
  a duplicate)
- a viewer in the current tab, not focused → focus it in place
- the viewer already focused → close it (herdr auto-closes the emptied tab)

The switch is **root-aware**: "this repo" is the worktree (or, outside git, the directory) of the
pane you pressed the key in, so a viewer you opened on another directory with
[Open at another directory](#open-at-another-directory) is not mistaken for it. A running viewer
keeps its process working directory on the root it shows, which is how the launcher tells them
apart.

The idempotency is scoped to the **current workspace**: a viewer already open in a *different*
workspace is left where it is, and a fresh one opens here. The action reaches this workspace's
viewer, it never pulls you across workspaces.

Bind it to its own key, e.g. `prefix+shift+f` alongside `prefix+f` for the split:

```toml
[[keys.command]]
key = "prefix+shift+f"
type = "plugin_action"
command = "herdr-file-viewer.open-file-viewer-tab"
description = "open file viewer in tab"
```

## Open at another directory

Two more actions ask **where** to open. Each pops up a small herdr popup pre-filled with `~/`; type
a directory (or a file) and the viewer opens there instead of the directory you are in:

- `open-file-viewer-at` opens it in a **split** beside your pane, in your
  [`open_direction`](#split-beside-or-below), like `prefix+f`.
- `open-file-viewer-at-tab` opens it in a **new tab**, named `Files`.

Both always open a fresh viewer. Needs herdr 0.7.5+ (popups); Linux and macOS only for now. The
popup's title says which one you pressed.

| In the popup | Does |
|---|---|
| `Enter` | open the viewer at the typed directory (`~/` alone opens your home); for a file, open its directory with that file shown |
| `Tab` | complete a directory or file name, ignoring case (`work` → `Workspace/`); directories end in `/`; press again to cycle when several match |
| `↑` / `↓` (or `Shift-Tab`) | move through the list of matches; the highlighted one fills the line, so `Enter` opens it and `Tab` completes inside it |
| `Esc` / `Ctrl-C` | cancel: close the popup, open nothing |
| `Ctrl-U` | clear the line |
| `←` `→` `Home` `End` `Backspace` `Delete` | edit |

Paths resolve from your home directory first: `Workspace/app` means `~/Workspace/app`. If that does
not exist, the same path is tried from `/`, so `private/tmp/x` (or `~/private/tmp/x`) finds
`/private/tmp/x` without a leading slash; `Tab` rewrites the line to the absolute path it found. An
absolute path is used as typed, and one typed or pasted straight after the `~/` prefill
(`~//private/tmp`) counts as absolute too. The tree then roots exactly as a normal summon would, at that directory's
worktree top level inside git, else the directory itself. Picking a file roots the viewer at the
file's directory the same way and opens the file in the content pane. A path that does not exist
shows an error in the popup, so you can fix it or cancel.

```toml
[[keys.command]]
key = "prefix+d"
type = "plugin_action"
command = "herdr-file-viewer.open-file-viewer-at"
description = "open file viewer at… (split)"

[[keys.command]]
key = "prefix+alt+d"
type = "plugin_action"
command = "herdr-file-viewer.open-file-viewer-at-tab"
description = "open file viewer at… (tab)"
```

Avoid `prefix+g` and `prefix+shift+d`: herdr's built-in `goto` and `close_workspace` use them by
default.

Under the hood the popup runs `herdr plugin pane open … --placement split|tab --env
HERDR_FILE_VIEWER_ROOT=<dir>` (plus `--env HERDR_FILE_VIEWER_OPEN=<file>` for a file): the directory reaches the viewer as an environment variable, never
as `--cwd`. An agent or script can open a viewer on a given directory the same way.

## Limitation over `herdr --remote`

`--remote` attaches with **local** keybindings by default, but herdr does not send local custom
command bindings, including `plugin_action`, to the remote host. To drive the viewer on the remote,
put the binding in the remote server's `config.toml` and attach with
**`herdr --remote <host> --remote-keybindings server`**. The qualified id then resolves against the
plugin installed on that server.

This is a herdr keybinding/remote limitation, not the plugin's. The action and launcher work the
same locally and remotely; only which config supplies the binding differs.

On Windows the action ids and keybinding requirements differ slightly — see [Windows](windows.md).
