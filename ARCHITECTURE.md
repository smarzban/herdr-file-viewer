# Architecture

A high-level map of how `herdr-file-viewer` is built, for contributors and for anyone
reviewing the implementation. It's a small crate (~11k lines of Rust, inline tests included), so
this stays brief.

## The shape: one in-process TUI owning both columns

The viewer is a **single process** that draws *both* the directory tree (left) and the content
pane (right) inside one [ratatui](https://ratatui.rs) frame. It is **not** composed of multiple
herdr panes — herdr opens it as one split pane and the viewer owns the whole rectangle. This
keeps focus, layout, and keyboard routing entirely in-process (no cross-pane IPC for the core
UX), at the cost of drawing the two-column layout ourselves.

The crate is a **library + a thin binary**: `src/main.rs` is a few lines that either prints a
launcher decision (for the shell launch scripts) or calls `lib::run()`; everything testable
lives in the library modules.

## Components

Each module has one responsibility; the side-effecting ones sit behind traits so the controller
is unit-testable with stubs.

| Module | Responsibility |
| --- | --- |
| `host` | The herdr boundary — parse the injected `HERDR_PLUGIN_CONTEXT_JSON` launch context, degrading to `{ cwd }` on anything malformed (never panics). |
| `context` | The normalized `LaunchContext` the host hands to the resolver. |
| `root` | Resolve the tree root (git worktree top-level, else cwd) and git-presence; the re-root engine re-resolves the root and rebuilds the tree + git services in place when you switch worktrees. |
| `git` | Read-only git queries: status, baseline selection, changed-set, per-file diff. The **only** module that shells out to `git`, and only with read-only subcommands. |
| `herdr` | Read-only queries to the herdr CLI (`$HERDR_BIN_PATH`) — list git worktrees and which workspaces have an active agent; an absent or failing herdr degrades gracefully to git-only. |
| `worktree` | Enumerate the repo's git worktrees (`git worktree list --porcelain`) and overlay herdr's agent-active workspace + per-row agent status, feeding the switch-worktree picker. |
| `tree` | The rooted, `.gitignore`-aware file tree: filters (gitignored, changed-only, hidden/dotfiles), cursor, expansion, status markers. |
| `view_policy` | A pure decision: which view mode a file gets (changed → diff, markdown → rendered, else → syntax content) and the cycle order. |
| `render` | Produce the content-pane text: classify the file, delegate styling to an external CLI, and **neutralize escape sequences** before display. |
| `presenter` | Draw the two-column (or zoomed / narrow) layout with ratatui; scroll the tree to keep the selection in view (and horizontally for long rows) and draw scrollbars where a pane overflows; report the content viewport + tree scroll offset + widest tree row + pane geometry back, so the controller can hit-test a tree click and map a scrollbar drag. |
| `picker` | The modal worktree-switcher overlay state (rows, cursor, horizontal scroll) drawn over the layout; captures its own nav / confirm / cancel keys while open. |
| `proc` | Shared subprocess reaping: one `wait_bounded` (child wait + poll + timeout-kill) used by both the content renderer and the update check, so the timeout-kill semantics are defined once. |
| `finder` | The modal go-to-file finder overlay state (query, ranked matches, cursor, scroll) drawn over the layout; captures its own keys while open and navigates the tree selection on confirm. |
| `fuzzy` | A pure fuzzy matcher: rank file paths against a typed query (the finder's scoring), no I/O. |
| `index` | Build the flat, `.gitignore`-aware list of repo file paths the finder searches. |
| `search` | A pure in-file substring matcher: find every occurrence of a query within the displayed content's lines (smartcase, literal — never a regex), returning byte-offset match ranges in document order. No I/O. |
| `highlight` | Overlay match highlighting onto the content pane: re-segment each line's spans at the match byte boundaries and patch a highlight style over the matched runs, with a distinct style on the current match. Pure; composes over the delegated render rather than re-rendering. |
| `text_layout` | A pure text-wrapping helper: how many display rows a line occupies at a given width — shared by the content pane, the finder, and the help overlay. No I/O. |
| `prompt` | A reusable single-line text-input buffer (push / backspace / clear) backing the finder query — and future keyboard prompts. |
| `infile` | In-file-navigation modal state: which bottom prompt is open (go-to-line or in-file search), its `prompt` input buffer, the live `SearchState` (query, matches, current match), and the content-scroll snapshot for cancel-restore. |
| `lineselect` | Line-select modal state: anchor + marker source-line indices (plus mouse char carets), the focus-gated `L` entry that auto-switches to the source view, key/mouse handling, and the two confirms — formatting the `path:line` / `path:start-end` reference (`Enter`) and extracting the selected text (`y`/`Y`, joined by newlines, gutter stripped, residual control bytes removed) for the clipboard. Read-only. |
| `help` | Help overlay state: the embedded changelog source and About text, plus the section and vertical scroll position for the `?` overlay. Pure; no I/O — the changelog is compiled in at build time. |
| `input` | Map crossterm key events → intents. |
| `intent` | The closed set of user intents (one exhaustive enum). |
| `controller` | Orchestrate intents → state changes; hold the ephemeral session state; dispatch renders to the worker; map mouse events (clicks, wheel, divider + scrollbar drags) against the fed-back geometry; on a worktree switch, rebuild the root-bound services through a provider factory and respawn the render worker. One `Controller` split across feature submodules of `controller/`: `mod` (the type defs, construction, intent/poll/render core, tree-navigation intents), `mouse` (column/tree pointer handling), `help`, `finder`, `picker`, `infile` (the bottom prompt), and `lineselect` (the copy-line-reference marker mode) overlays, and `git_apply` (apply git status + changed-set to the tree). The single open overlay is a `Modal` enum (including `Modal::LineSelect`), so "at most one modal open" is type-enforced. |
| `app` | The event loop (`run()`): assemble the live components, then `draw → poll input → route to the controller (or the active modal) → drain finished renders`, until the user closes the viewer. |
| `update` | The bounded, read-only, fail-silent update check: at most once per 24h a hardened `git ls-remote --tags` (off the UI thread, in a private temp dir) compares the latest release to the running build and feeds the dismissable "update available" banner; opt out via `HERDR_FILE_VIEWER_NO_UPDATE_CHECK`. |
| `editor` | Hand a file off to `$EDITOR` (launch only — never reads or writes the file). |
| `opener` | Read-only OS hand-off for the `O` / `R` keys: a pure per-OS argv builder (open-with-default-app / reveal-in-file-manager) plus an `Opener` seam over the reused editor `Spawner`, spawned **non-blocking** (no terminal takeover, stdio nulled) so the TUI keeps running. |
| `launch` | The "launch-or-focus-or-toggle" decision behind the shell launch scripts (pure, hermetically testable). |

## Data flow

```
herdr → env (HERDR_PLUGIN_CONTEXT_JSON)
          │
   host::from_env → root::resolve → git::default_baseline
          │
   Controller::new  ── wires live GitService / ContentProvider / EditorHandoff / Clipboard behind traits
          │
   event loop (app::run):  draw → poll input → handle(intent) → drain finished renders → repeat
```

**Rendering is off the input thread.** Selecting a file *dispatches* a render job to a worker
thread (`std::thread` + `mpsc`); `handle()` returns immediately so input never blocks on a slow
external renderer. The finished text arrives later and is drained by `Controller::poll()` each
tick. Jobs carry a monotonic sequence so a stale render for a file the user has left is dropped.
A renderer panic is contained (`catch_unwind`) so the worker survives. No `tokio`.

## Load-bearing decisions

- **Read-only.** The viewer never mutates a file or the git repository. The editor path is a
  hand-off to an external process; the path-copy keys (`y`/`Y`) only copy a path string to the
  clipboard (via an OSC 52 escape). Every `git` invocation uses read-only subcommands.
- **Delegate rendering.** Markdown, diffs, and syntax highlighting are produced by best-in-class
  external CLIs (`glow`, `delta`, `bat`) — the viewer builds only the shell and ingests their
  ANSI output. Each renderer is optional; a missing one degrades to plain text + a notice.
- **Git is first-class**, woven through the tree (status markers, colors, changed-only filter,
  baseline toggle) and the content pane (diff view) — not a separate mode.
- **In-memory, ephemeral state only** — with one on-disk exception: the update-check timestamp
  cache (`update-check.json` under the cache dir), which is advisory and safe to delete. Apart
  from that, no persistent store; the filesystem and git repo are the read-only source of truth.

## Trust boundaries

Three untrusted inputs are handled defensively (see [SECURITY.md](SECURITY.md)):

1. **File content** is untrusted — fed to renderers on **stdin** (never as an argument), and the
   renderer output is re-sanitized so no escape sequence can drive the terminal.
2. **The git repository** may be untrusted (an agent's worktree, a clone) — every `git`
   invocation is hardened against repo-controlled code execution (no external diff/textconv,
   neutralized `core.fsmonitor`/`core.hooksPath`, scrubbed repo-redirecting env). This hardening
   lives in **one** shared builder so it cannot drift between callers.
3. **The herdr-injected context** is parsed defensively and degrades to a minimal default.

## Tests

`cargo test` runs unit tests, integration tests, and end-to-end tests that drive the real binary
over a pseudo-terminal (`expectrl`), plus ratatui `TestBackend` snapshot tests (`insta`). The e2e
tests stub the editor via `$EDITOR` and run in temp directories, so they need neither the
external renderers nor a live herdr.
