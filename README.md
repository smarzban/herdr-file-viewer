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
- **Git woven in** — per-file status markers (`M`/`A`/`D`/`?`), **colored** so changes read at
  a glance (changed files and folders containing changes are red, new files green); a
  changed-files-only filter; and a baseline you can switch between the merge-base of your
  branch and `HEAD`.
- **The right view per file** — a changed file shows its **diff**; a markdown file renders;
  anything else is shown as syntax-highlighted content with line numbers. Cycle the view to
  override.
- **Navigable content** — scroll the content pane in all four directions, toggle line
  wrapping, and resize the tree/content split; the layout reflows when the pane is resized.
- **Keyboard-first** — every function has a key; no mouse required.

## Install

Requirements: **Rust 1.96+** (edition 2024) and Cargo; **herdr 0.7.0+**, on **Linux** or
**macOS**.

**Install through herdr** — herdr runs the manifest's `[[build]]` step
(`cargo build --release`) at install time, producing `./target/release/herdr-file-viewer`,
which the viewer pane launches:

```bash
# from a published GitHub repo (owner/repo[/subdir]):
herdr plugin install <owner>/<repo>

# or, for local development, link this checkout in place:
cargo build --release            # plugin link does NOT run the [[build]] step, so build first
herdr plugin link /path/to/herdr-file-viewer
```

Confirm it registered with `herdr plugin list`. To build manually outside herdr:

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

**Limitation over `herdr --remote`.** `--remote` attaches with **local** keybindings by
default, and herdr has no way to fire a plugin action into the *attached* (remote) session from
a local key: a `type = "shell"` command runs against your **local** herdr (wrong session), and a
`type = "pane"` command runs in a throwaway pane that closes the instant it exits (so the viewer
doesn't persist).

This is a herdr keybinding/remote limitation, not the plugin's — the action and launcher work
the same locally and remotely; it's only *which* keymap fires them across `--remote` that differs.

## Keys

| Key | Action |
| --- | --- |
| `↑` / `k`, `↓` / `j` | Move the tree cursor — or **scroll the content pane** vertically when it is focused |
| `→` / `l` | Expand the selected directory — or **scroll the content pane right** when it is focused |
| `←` / `h` | Collapse the selected directory — or **scroll the content pane left** when it is focused |
| `i` | Toggle gitignored files |
| `c` | Toggle changed-files-only |
| `b` | Toggle the diff baseline (base branch ⇄ `HEAD`) |
| `v` | Cycle the content view mode |
| `e` | Open the selected file in `$EDITOR` |
| `Tab` | Move focus between the tree and content columns |
| `<` / `>` | Narrow / widen the tree column (move the divider) |
| `w` | Toggle line wrapping for the content pane |
| `q` / `Esc` | Close the viewer and return to the prior pane |

`Tab` to the content pane, then the arrow keys (or `h`/`j`/`k`/`l`) scroll it in all four
directions; `Tab` back to the tree to move between files. Long lines wrap in prose (markdown /
plain text); diffs and code keep their original lines so columns stay aligned — scroll
sideways with `←`/`→`, or press `w` to wrap them instead. The layout reflows automatically
when the pane is resized.

Character keys act only when no control chord is held (so terminal chords like `Ctrl+C` are
never intercepted); `Shift` is permitted, for keys such as `<` and `>`.

### Mouse

The viewer is keyboard-first (AC-18); the mouse is additive and on by default:

| Gesture | Action |
| --- | --- |
| **Click** a tree row | Select it (focus the tree) |
| **Double-click** a folder | Expand / collapse it |
| **Double-click** a file | Open it in `$EDITOR` (same as `e`) |
| **Wheel** over the content pane | Scroll it vertically; over the tree, move the selection |
| **Horizontal wheel / swipe** over the content | Scroll it sideways (terminal-dependent — see below) |
| **Drag** the divider | Resize the tree / content split |

**`Shift`+drag is left to your terminal**, so its native select-and-copy still works while the
viewer owns ordinary clicks — herdr reserves `Shift`+mouse for exactly this. (herdr forwards
mouse events to the pane because the viewer requests capture.)

**Horizontal mouse scroll is terminal-dependent** — it works only where your terminal emits
horizontal-scroll events (`ScrollLeft` / `ScrollRight`); many terminals send nothing for a
sideways trackpad swipe. The `←` / `→` keys always scroll the content sideways regardless.

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
