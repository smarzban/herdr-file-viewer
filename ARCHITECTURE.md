# Architecture

A high-level map of how `herdr-file-viewer` is built, for contributors and for anyone
reviewing the implementation. It's a small crate (~3.5k lines of source), so this stays brief.

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
| `tree` | The rooted, `.gitignore`-aware file tree: filters, cursor, expansion, status markers. |
| `view_policy` | A pure decision: which view mode a file gets (changed → diff, markdown → rendered, else → syntax content) and the cycle order. |
| `render` | Produce the content-pane text: classify the file, delegate styling to an external CLI, and **neutralize escape sequences** before display. |
| `presenter` | Draw the two-column (or zoomed / narrow) layout with ratatui; report the content viewport + pane geometry back. |
| `picker` | The modal worktree-switcher overlay state (rows, cursor, horizontal scroll) drawn over the layout; captures its own nav / confirm / cancel keys while open. |
| `input` | Map crossterm key events → intents. |
| `intent` | The closed set of user intents (one exhaustive enum). |
| `controller` | Orchestrate intents → state changes; hold the ephemeral session state; dispatch renders to the worker; on a worktree switch, rebuild the root-bound services through a provider factory and respawn the render worker. |
| `editor` | Hand a file off to `$EDITOR` (launch only — never reads or writes the file). |
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
- **In-memory, ephemeral state only.** No persistent store; the filesystem and git repo are the
  read-only source of truth.

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
