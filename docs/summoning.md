# Summoning the viewer

How the viewer gets opened: the open actions, the idempotent launcher, the four layouts (split,
tab, overlay, popup), and the `--remote` caveat. For a quick "install then bind a key," see the
[Quick start](../README.md#quick-start);
once it's open, see the [usage guide](usage.md) and [keys reference](keys.md).

The viewer opens **only** in response to an explicit action. There are no event hooks and no
automatic invocation. The manifest declares one `[[panes]]` entry (the viewer itself) and an
`[[actions]]` per layout whose command opens it:

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

## Open as an overlay

A third action, `open-file-viewer-overlay`, opens the viewer as a **temporary overlay** over the
active pane (`scripts/open-file-viewer-overlay.sh`, `--placement overlay`). herdr builds the
overlay as a 50/50 split whose new half is tab-zoomed, so it covers what you were doing; when it
closes, herdr restores the previous focus **and** the previous zoom, putting you back exactly where
you were. Good for "look at a file, then get straight back to what I had".

It shares the split launcher's *launch-or-focus-or-toggle* decision, scoped to the current tab, so
there is **one viewer per tab whichever way you summoned it**. Two consequences worth knowing:

- With a viewer already open in this tab, the overlay key **focuses** it (or closes it if it is
  already focused) rather than opening a second one — and if that viewer is a split, it stays a
  split.
- Pressing the **split** key while an *unfocused overlay* viewer is open converts that overlay into
  an ordinary split. The split launcher pulls focus by zoom-cycling the pane, which drops the
  overlay's covering zoom; the viewer keeps working, it is just tiled from then on.

Bind it to its own key:

```toml
[[keys.command]]
key = "prefix+alt+f"
type = "plugin_action"
command = "herdr-file-viewer.open-file-viewer-overlay"
description = "open file viewer as an overlay"
```

**Linux and macOS only** in this release, and it needs **herdr 0.7.4+** (the plugin's declared
minimum). There is no native-Windows overlay action — see [Windows](windows.md).

## Open in a popup

A fourth action, `open-file-viewer-popup`, opens the viewer in a **floating, session-modal popup**
(`scripts/open-file-viewer-popup.sh`, `--placement popup`). A popup is not a herdr pane: it has no
pane id, it never shows up in `pane list`, the layout, or the session snapshot, and nothing can
address it after it opens.

**Closing it — read this first.** The popup receives all terminal input and no other surface is
focusable while it is up, so the only way out is the viewer's own close key:

- **`Esc` always works.** It is the un-remappable floor: even if you rebind `close` away from `q`
  in `[keys]`, `Esc` still reaches it (see [configuration](configuration.md#keybindings)). `q` is
  the default binding of that same action.
- **Each press peels one layer.** A live line selection, then a flash/notice, then a committed
  search, then zoom — and only then the viewer itself. With a search committed you press a few
  times.
- **With annotations held** (the default `confirm_discard = true`), closing raises the discard
  confirm first. Its keys are **fixed**, not remappable: `y` copies them and quits, `q` quits and
  discards, `Esc` returns to the viewer. So `Esc` on its own loops back into the viewer forever —
  press `Esc` **to** the confirm, then `q` or `y` **through** it.

**Open-only: there is no toggle.** herdr ships no CLI verb that can close a popup (checked against
0.8.0), and the open call hands back nothing addressable, so the launcher cannot focus or close one
the way the split and tab launchers do. Pressing the key again while a popup is open is rejected by herdr
itself with `popup already open` (exit 1), and the live popup is left untouched.

**Size: 90% × 85% of the terminal**, fixed in the launcher. An unsized popup is half-size, which
measured 80 columns on a 167-column terminal — sitting exactly on the viewer's narrow-layout
boundary, with no margin at all before it collapses to a single column, and collapsing outright on
any narrower terminal. 90% gives 147 columns there: comfortably two-column, with a frame of context
around it.

```toml
[[keys.command]]
key = "prefix+alt+p"
type = "plugin_action"
command = "herdr-file-viewer.open-file-viewer-popup"
description = "open file viewer in a popup"
```

**Want a different size?** There is no config key for it — bind a raw `type = "shell"` command
carrying your own argv instead of the action. A size is either a cell count or a percentage from
`1%` to `100%`, and the `--env` marker is what tells the viewer it is in a popup (so `Z`
full-screens in-pane instead of zooming the pane underneath):

```toml
[[keys.command]]
key = "prefix+alt+p"
type = "shell"
command = "herdr plugin pane open --plugin herdr-file-viewer --entrypoint file-viewer --placement popup --width 70% --height 70% --env HERDR_FILE_VIEWER_PLACEMENT=popup"
description = "open file viewer in a smaller popup"
```

**Linux and macOS only** in this release, and it needs **herdr 0.7.4+** — session-modal popups for
plugins, with cell/percentage sizing, arrived in that release, which is why it is the plugin's
declared minimum. There is no native-Windows popup action — see [Windows](windows.md).

## Limitation over `herdr --remote`

`--remote` attaches with **local** keybindings by default, but herdr does not send local custom
command bindings, including `plugin_action`, to the remote host. To drive the viewer on the remote,
put the binding in the remote server's `config.toml` and attach with
**`herdr --remote <host> --remote-keybindings server`**. The qualified id then resolves against the
plugin installed on that server.

This is a herdr keybinding/remote limitation, not the plugin's. The action and launcher work the
same locally and remotely; only which config supplies the binding differs.

On Windows the action ids and keybinding requirements differ slightly — see [Windows](windows.md).
