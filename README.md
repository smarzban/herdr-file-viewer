# herdr-file-viewer

[![CI](https://github.com/smarzban/herdr-file-viewer/actions/workflows/ci.yml/badge.svg)](https://github.com/smarzban/herdr-file-viewer/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Rust 1.96+](https://img.shields.io/badge/rust-1.96%2B-orange.svg)
![herdr 0.7+](https://img.shields.io/badge/herdr-0.7%2B-8a2be2)
![platforms: linux • macOS](https://img.shields.io/badge/platforms-linux%20%E2%80%A2%20macOS-informational)

**Browse your repo without leaving your terminal session — a git-aware, read-only file viewer
that lives in a herdr pane.** A keyboard-driven TUI with a directory tree
on the left and, on the right, exactly the view each file deserves: a **diff** if it changed,
**rendered markdown** if it's markdown, **syntax-highlighted code** otherwise. Git status is woven
right into the tree. It opens beside whatever you're doing and never touches your files.

![herdr-file-viewer open in a herdr split beside your work — the directory tree on the left, syntax-highlighted content on the right](assets/File-viewer.png)

*…and running full-screen — the same tree + content, filling the terminal:*

![herdr-file-viewer running full-screen](assets/File-Viewer-FS.png)

<!-- TODO: swap these stills for a short GIF (open → arrow to a changed file (diff) → `v` rendered markdown → `z` zoom). asciinema + agg → gif. -->

## Why you'd want it

- **The right view, automatically.** Stop `cat`-ing files and squinting at raw diffs. A changed
  file shows its diff; a README renders; code is highlighted — no mode-switching, no commands.
- **Git at a glance.** `M`/`A`/`D`/`?` markers, colored so changes pop, a changed-files-only
  filter, and a baseline you can flip between your branch's merge-base and `HEAD` — all in the
  tree, not a separate mode.
- **It sits beside your work.** Opens in a herdr split (or its own tab) with one keypress, and
  toggles away just as fast. Great next to an agent, a build, or an editor.
- **Safe on anything.** Read-only by construction and hardened to open *untrusted* repos (an
  agent's worktree, a fresh clone) without running repo-controlled code or letting hostile file
  content drive your terminal. See [SECURITY.md](SECURITY.md).
- **Keyboard-first**, mouse-optional, and it never reinvents rendering — it delegates to
  `glow` / `delta` / `bat` and degrades gracefully when they're absent.

## What it does

- **Tree, scoped to your work** — rooted at the worktree root inside a git repo, else the
  launch directory. Honors `.gitignore` (toggle to reveal ignored files).
- **Git woven in** — per-file status markers (`M`/`A`/`D`/`?`), **colored** so changes read at
  a glance (changed files and folders containing changes are red, new files green); a
  changed-files-only filter; and a baseline you can switch between the merge-base of your
  branch and `HEAD`.
- **The right view per file** — a changed file shows its **diff**; a markdown file renders;
  anything else is shown as syntax-highlighted content with line numbers. Cycle the view
  (`v`) to override — a changed file can also show a **full-file diff**: the whole file with
  line numbers and the diff shown inline.
- **Navigable content** — scroll the content pane in all four directions, toggle line
  wrapping, resize the tree/content split, or zoom (`z`) to hide the tree and read a file
  full-screen; the layout reflows when the pane is resized.
- **Keyboard-first** — every function has a key; no mouse required.

## Quick start

```bash
# 1. Install the plugin (tagged releases download a prebuilt binary; otherwise builds from source):
herdr plugin install smarzban/herdr-file-viewer

# 2. (recommended) install the renderers, so markdown / diffs / code are styled, not plain text:
brew install glow git-delta bat     # macOS — or use your package manager
#   Linux / cross-platform: run scripts/install-renderers.sh from the plugin dir (`herdr plugin list`)
```

Then **bind a key** in your herdr config (`~/.config/herdr/config.toml`) so one press summons it:

```toml
[[keys.command]]              # open in a split beside your work
key = "prefix+f"
type = "shell"
command = "herdr plugin action invoke open-file-viewer --plugin herdr-file-viewer"

[[keys.command]]              # …or in its own tab
key = "prefix+shift+f"
type = "shell"
command = "herdr plugin action invoke open-file-viewer-tab --plugin herdr-file-viewer"
```

Run `herdr server reload-config`, then press your key. That's the whole setup — the split-pane
viewer and its open actions ship **inside** the plugin and register automatically on install, so
you only add the keybinding. Everything below is detail: pinning a release, the renderer fallback,
the launcher's open/focus/toggle behavior, the full key map, and the `--remote` caveat.

## Keys

| Key | Action |
| --- | --- |
| `↑` / `k`, `↓` / `j` | Move the tree cursor — or **scroll the content pane** vertically when it is focused |
| `→` / `l` | Expand the selected directory — or **scroll the content pane right** when it is focused |
| `←` / `h` | Collapse the selected directory — or **scroll the content pane left** when it is focused |
| `Enter` | Activate the selection — expand/collapse a directory, or open a file in **zoom mode** (content full-screen) |
| `i` | Toggle gitignored files |
| `c` | Toggle changed-files-only |
| `b` | Toggle the diff baseline (base branch ⇄ `HEAD`) |
| `v` | Cycle the content view mode |
| `e` | Open the selected file in `$EDITOR` |
| `Tab` | Move focus between the tree and content columns |
| `<` / `>` | Narrow / widen the tree column (move the divider) |
| `w` | Toggle line wrapping for the content pane |
| `z` | Zoom — hide the tree so the content pane fills the frame; press again (or `q`/`Esc`) to restore the two-column layout |
| `r` | Refresh git state — pick up changes made outside the viewer (a merge / pull / commit elsewhere) |
| `u` | Dismiss the "update available" banner for this session |
| `q` / `Esc` | Back out of zoom if zoomed; otherwise close the viewer and return to the prior pane |

`Tab` to the content pane, then the arrow keys (or `h`/`j`/`k`/`l`) scroll it in all four
directions; `Tab` back to the tree to move between files. Long lines wrap in prose (markdown /
plain text); diffs and code keep their original lines so columns stay aligned — scroll
sideways with `←`/`→`, or press `w` to wrap them instead. The layout reflows automatically
when the pane is resized.

**Git state stays current.** The viewer re-reads git status when the pane **regains focus**, so
changes you make outside it (a merge, pull, or commit in another pane) show up automatically; `r`
forces a full refresh on demand. (Focus-refresh updates the tree's status without disturbing your
content scroll.)

Character keys act only when no control chord is held (so terminal chords like `Ctrl+C` are
never intercepted); `Shift` is permitted, for keys such as `<` and `>`.

### Mouse

The viewer is keyboard-first; the mouse is additive and on by default:

| Gesture | Action |
| --- | --- |
| **Click** a tree row | Select it (focus the tree) |
| **Double-click** a folder | Expand / collapse it (same as `Enter`) |
| **Double-click** a file | Open it in **zoom mode** — content full-screen (same as `Enter`); the editor is the `e` key |
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

## Install

Requirements: **herdr 0.7.0+**, on **Linux** or **macOS**.

> **No Rust toolchain needed for tagged releases.** `herdr plugin install smarzban/herdr-file-viewer`
> downloads a prebuilt, SHA-256-verified binary for your platform (macOS arm64/x86_64, Linux x86_64).
> If no matching prebuilt is available — an unsupported platform, or installing from a `main` that is
> ahead of the latest release — it automatically builds from source with `cargo` instead (Rust 1.96+).
> The install command is the same either way.

**Install through herdr** — herdr runs the manifest's `[[build]]` step at install time, either
downloading a prebuilt binary or compiling from source, producing `./target/release/herdr-file-viewer`,
which the viewer pane launches:

```bash
# install (and update — re-run any time to get the latest):
herdr plugin install smarzban/herdr-file-viewer
# …optional: pin a specific older version for reproducibility:
herdr plugin install smarzban/herdr-file-viewer --ref v1.0.0

# or, for local development, link this checkout in place:
cargo build --release            # plugin link does NOT run the [[build]] step, so build first
herdr plugin link /path/to/herdr-file-viewer
```

> You don't need `--ref` to stay current — a bare install pulls the latest. See
> [Updating](#updating).

Confirm it registered with `herdr plugin list`. To build manually outside herdr:

```bash
cargo build --release
```

## Updating

herdr has no plugin auto-update, so the viewer tells you when a new release exists: open it
(`prefix+f`) and, if you're behind, a status line appears at the bottom naming the new version
and the command to update. Press `u` to dismiss it for the session.

To update, just re-run the install — it pulls the latest:

```bash
herdr plugin install smarzban/herdr-file-viewer
```

- You **don't** need `--ref` to stay current; it only *pins* a specific version (and a pin stays
  pinned until you change it).
- Want a heads-up the moment a release ships? On GitHub, **Watch → Custom → Releases**.
- Prefer no network check? Set `HERDR_FILE_VIEWER_NO_UPDATE_CHECK=1` in the pane's environment —
  the check (and banner) are disabled entirely. The check otherwise runs at most once per 24h,
  off the UI thread, over a read-only `git ls-remote`, and never blocks or fails the viewer when
  offline.

## Optional runtime dependencies (external renderers)

Rendering is **delegated** to best-in-class external CLIs. These are *runtime, install-time*
dependencies — not Cargo dependencies — and each is **optional**:

| View | Renderer | Install |
| --- | --- | --- |
| Rendered markdown | [`glow`](https://github.com/charmbracelet/glow) | `brew install glow` / package manager |
| Diffs | [`delta`](https://github.com/dandavison/delta) | `brew install git-delta` / `cargo install git-delta` |
| Syntax-highlighted content | [`bat`](https://github.com/sharkdp/bat) | `brew install bat` / package manager |

Or install all three at once with the bundled helper (best-effort; detects brew/apt/dnf/pacman
and falls back to `cargo`):

```bash
./scripts/install-renderers.sh
```

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

## Architecture & security

- **[ARCHITECTURE.md](ARCHITECTURE.md)** — the design at a glance: one in-process TUI owning both
  columns, the component map, off-thread rendering, and the load-bearing decisions (read-only,
  delegate rendering, git-first).
- **[SECURITY.md](SECURITY.md)** — the threat model and mitigations for opening untrusted content:
  read-only by construction, escape-sequence neutralization, hardened git invocations, and how to
  report a vulnerability.

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

## License

[MIT](LICENSE) © Saeed Marzban
