# herdr-file-viewer

A git-aware, **read-only** file viewer that runs as a **herdr** plugin (herdr is a terminal
agent-multiplexer): a keyboard-driven TUI that opens in a split pane beside your current work,
with a directory
tree on the left and a content pane on the right (rendered markdown, diffs, or
syntax-highlighted content).

It never modifies your files or your git repository. Opening a file in your editor is a
hand-off to an *external* editor — the viewer itself only reads.

## What it does

- **Tree, scoped to your work** — rooted at the worktree root inside a git repo, else the
  launch directory. Honors `.gitignore` (toggle to reveal ignored files).
- **Git woven in** — per-file status markers (`M`/`A`/`D`/`?`), a changed-files-only filter,
  and a baseline you can switch between the merge-base of your branch and `HEAD`.
- **The right view per file** — a changed file shows its **diff**; a markdown file renders;
  anything else is shown as syntax-highlighted content. Cycle the view to override.
- **Keyboard-first** — every function has a key; no mouse required.

## Install

The plugin builds from source. herdr runs the build step declared in the manifest
(`herdr-plugin.toml`) at install time:

```toml
[[build]]
command = ["cargo", "build", "--release"]
```

So installing it through herdr produces `./target/release/herdr-file-viewer`, which the
viewer pane launches. Requirements:

- **Rust 1.96+** (edition 2024) and Cargo.
- **herdr 0.7.0+**, on **Linux** or **macOS**.

To build manually:

```bash
cargo build --release
```

## Optional runtime dependencies (external renderers)

Rendering is **delegated** to best-in-class external CLIs. These are *runtime, install-time*
dependencies — not Cargo dependencies — and each is **optional**:

| View | Renderer | Install |
| --- | --- | --- |
| Rendered markdown | [`glow`](https://github.com/charmbracelet/glow) | `brew install glow` / package manager |
| Diffs | [`delta`](https://github.com/dandavison/delta) | `brew install git-delta` / `cargo install git-delta` |
| Syntax-highlighted content | [`bat`](https://github.com/sharkdp/bat) | `brew install bat` / package manager |

**If a renderer is not installed, the viewer falls back to plain text** and shows a short
notice in the content pane naming the missing capability (e.g. *“Markdown renderer
unavailable (glow: …); showing plain text.”*). The viewer never crashes or shows an empty
pane when a renderer is absent — it degrades gracefully. So the renderers are recommended for
the best experience but not required to use the viewer.

Untrusted file content is always fed to a renderer on **stdin** (never as a command argument),
and the renderer's output is re-sanitized before display, so a hostile file name or file
content cannot inject a command or drive the terminal.

## Summoning the viewer

The viewer opens **only** in response to an explicit action — there are no event hooks and no
automatic invocation. The manifest declares one action:

```toml
[[actions]]
id = "open-file-viewer"
title = "Open file viewer"
pane = "file-viewer"
```

Bind this action to a key in your herdr configuration (see your herdr docs for the action
keybinding syntax). Invoking it opens the viewer in a **split** pane beside your current work.

## Keys

| Key | Action |
| --- | --- |
| `↑` / `k`, `↓` / `j` | Move the tree cursor |
| `→` / `l` | Expand the selected directory |
| `←` / `h` | Collapse the selected directory |
| `i` | Toggle gitignored files |
| `c` | Toggle changed-files-only |
| `b` | Toggle the diff baseline (base branch ⇄ `HEAD`) |
| `v` | Cycle the content view mode |
| `e` | Open the selected file in `$EDITOR` |
| `Tab` | Move focus between the tree and content columns |
| `q` / `Esc` | Close the viewer and return to the prior pane |

All character keys act only when no modifier is held, so terminal chords (e.g. `Ctrl+C`) are
never intercepted.

### Opening in an editor

`e` hands the selected file to the editor named by the `$EDITOR` environment variable
(e.g. `export EDITOR="vim"` or `EDITOR="code --wait"`). The viewer suspends, runs the editor,
and resumes when it exits. If `$EDITOR` is unset, a notice is shown — the viewer never edits a
file itself.

## Development

This crate is a library (`src/lib.rs` + modules) plus a thin binary (`src/main.rs` →
`run()`), so the components are unit-testable.

```bash
cargo test                 # unit + integration + e2e (pty) tests
cargo build --release      # what herdr's [[build]] step runs
cargo run                  # run the viewer locally, outside herdr
```

The e2e tests drive the real binary over a pseudo-terminal; they stub the editor via
`$EDITOR` and run in temporary directories, so they need neither glow/delta/bat nor a live
herdr.
