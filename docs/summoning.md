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

## Open in a tab instead of a split

A second action, `open-file-viewer-tab`, opens the viewer in its **own tab**
(`scripts/open-file-viewer-tab.sh`, `--placement tab`). Its launcher is idempotent *across the tabs
of the current workspace*, *open-or-switch-or-toggle*:

- no viewer in this workspace → open it in a new tab (focused)
- a viewer in another tab of this workspace → **switch to that tab** (never a duplicate)
- a viewer in the current tab, not focused → focus it in place
- the viewer already focused → close it (herdr auto-closes the emptied tab)

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

## Limitation over `herdr --remote`

`--remote` attaches with **local** keybindings by default, but herdr does not send local custom
command bindings, including `plugin_action`, to the remote host. To drive the viewer on the remote,
put the binding in the remote server's `config.toml` and attach with
**`herdr --remote <host> --remote-keybindings server`**. The qualified id then resolves against the
plugin installed on that server.

This is a herdr keybinding/remote limitation, not the plugin's. The action and launcher work the
same locally and remotely; only which config supplies the binding differs.

On Windows the action ids and keybinding requirements differ slightly — see [Windows](windows.md).
