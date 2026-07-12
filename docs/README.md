# Documentation

The full docs for **herdr-file-viewer** — a git-aware, read-only file viewer that runs as a herdr
TUI pane. New here? Start with the [README](../README.md) for what it is and a quickstart.

## Where to start

- **Just installed it?** → [Summoning the viewer](summoning.md) to bind a key and open it, then the
  [Usage guide](usage.md).
- **Want to know a key?** → [Keys & mouse](keys.md).
- **Customizing it?** → [Configuration](configuration.md).
- **On Windows?** → [Windows (preview)](windows.md).
- **Contributing?** → [CONTRIBUTING](../CONTRIBUTING.md).

## Pages

| Page | What's in it |
| --- | --- |
| [Install & updating](install.md) | Prebuilt vs. source, pinning a version, local-dev linking, and how updates surface (the in-app "update available" banner). |
| [Summoning the viewer](summoning.md) | The open actions, the idempotent launcher, split vs. tab, and the `--remote` caveat. |
| [Usage guide](usage.md) | A feature-by-feature tour: the tree, view modes, git awareness, find/search, copying, hand-offs, worktrees, help. |
| [Keys & mouse](keys.md) | The complete key table, mouse gestures, and the editor hand-off (`$EDITOR` troubleshooting). |
| [Configuration](configuration.md) | The full `config.toml` reference — editor/renderer/opener commands, startup toggles, tree layout, and `[keys]` remapping. |
| [External renderers](renderers.md) | The optional `glow` / `delta` / `bat` integrations and the plain-text fallback when they're absent. |
| [Windows (preview)](windows.md) | Native-Windows specifics: the `-windows` action ids, the herdr v0.7.2 keybinding requirement, and WSL. |

## Beyond the essentials

- [Architecture](../ARCHITECTURE.md) — one in-process TUI owning both columns, the component map,
  off-thread rendering, and the load-bearing decisions (read-only, delegate rendering, git-first).
- [Security](../SECURITY.md) — the threat model and mitigations for opening untrusted content, and
  how to report a vulnerability.
- [Changelog](../CHANGELOG.md) — the release history (also viewable in-app under `?` → What's New).
