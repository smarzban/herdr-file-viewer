# Summoning & keybindings

The viewer opens **only** in response to an explicit action — there are no event hooks and no
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
scoped to the current tab — so invoking it repeatedly is *launch-or-focus-or-toggle*:

- no viewer pane open in this tab → open a split (focused)
- a viewer pane open but not focused → focus it
- the viewer pane already focused → close it (herdr has no hide-without-close; reopening just
  re-walks the tree)

**One-press access — bind a key.** herdr's `config.toml` binds keys to commands; point one at the
action so it runs with the plugin's working directory (no hard-coded paths):

```toml
[[keys.command]]
key = "prefix+f"   # any herdr key syntax — e.g. ctrl+b then f
type = "shell"     # run detached; do NOT use "pane" (it would close when the command exits)
command = "herdr plugin action invoke open-file-viewer --plugin herdr-file-viewer"
```

Reload with `herdr server reload-config`. Pressing the key then opens / focuses / hides the
viewer via the same idempotent launcher. (Alternatively, `command` may invoke
`scripts/open-file-viewer.sh` directly using the absolute install path from `herdr plugin list`.)

**Open in a tab instead of a split.** A second action, `open-file-viewer-tab`, opens the viewer
in its **own tab** (`scripts/open-file-viewer-tab.sh`, `--placement tab`). Its launcher is
idempotent *across tabs* — *open-or-switch-or-toggle*:

- no viewer anywhere → open it in a new tab (focused)
- a viewer in another tab → **switch to that tab** (never a duplicate)
- a viewer in the current tab, not focused → focus it in place
- the viewer already focused → close it (herdr auto-closes the emptied tab)

Bind it to its own key — e.g. `prefix+shift+f` alongside `prefix+f` for the split:

```toml
[[keys.command]]
key = "prefix+shift+f"
type = "shell"
command = "herdr plugin action invoke open-file-viewer-tab --plugin herdr-file-viewer"
```

**Limitation over `herdr --remote`.** `--remote` attaches with **local** keybindings by
default, and herdr has no way to fire a plugin action into the *attached* (remote) session from
a local key: a `type = "shell"` command runs against your **local** herdr (wrong session), and a
`type = "pane"` command runs in a throwaway pane that closes the instant it exits (so the viewer
doesn't persist). To drive the viewer on the remote, attach with
**`herdr --remote <host> --remote-keybindings server`** — the binding then lives in the
*server's* `config.toml` and behaves fully (open / focus / close-toggle).

This is a herdr keybinding/remote limitation, not the plugin's — the action and launcher work
the same locally and remotely; it's only *which* keymap fires them across `--remote` that differs.
