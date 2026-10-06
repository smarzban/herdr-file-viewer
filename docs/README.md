# Documentation

These are the full docs for **herdr-file-viewer**, a git-aware, read-only file viewer that runs as a
herdr TUI pane. Start with the [README](../README.md) for an overview and quick start.

## Where to start

- **Just installed it?** Read [Summoning the viewer](summoning.md) to bind a key and open it, then
  read the [Usage guide](usage.md).
- **Want to know a key?** See [Keys and mouse](keys.md).
- **Customizing it?** See [Configuration](configuration.md).
- **On Windows?** See [Windows (preview)](windows.md).
- **Contributing?** See [CONTRIBUTING](../CONTRIBUTING.md).

## Pages

| Page | What's in it |
| --- | --- |
| [Install & updating](install.md) | Prebuilt vs. source, pinning a version, local development linking, and the in-app update banner. |
| [Summoning the viewer](summoning.md) | The open actions, the idempotent launcher, split vs. tab, and the `--remote` caveat. |
| [Usage guide](usage.md) | A feature-by-feature tour of the tree, open-at-launch, view modes, git awareness, find/search, copying, hand-offs, worktrees, and help. |
| [Keys & mouse](keys.md) | The complete key table, mouse gestures, and the editor hand-off (`$EDITOR` troubleshooting). |
| [Configuration](configuration.md) | The full `config.toml` reference for editor, renderer, and opener commands, startup toggles, tree layout, and `[keys]` remapping. |
| [External renderers](renderers.md) | The optional `glow` / `delta` / `bat` integrations and the plain-text fallback when they're absent. |
| [Windows (preview)](windows.md) | Native-Windows specifics: the `-windows` action ids, preview-channel requirement, and WSL. |

## Beyond the essentials

- [Architecture](../ARCHITECTURE.md) explains the single-process TUI, component map, off-thread
  rendering, and core decisions.
- [Security](../SECURITY.md) describes the threat model, controls for untrusted content, and
  vulnerability reporting.
- [Changelog](../CHANGELOG.md) contains the release history, which is also available in the `?`
  overlay under What's New.
