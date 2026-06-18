# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A **herdr plugin**: a git-aware, read-only **file viewer** — a keyboard-driven TUI that opens
in a herdr split pane, with a directory tree on the left and a content pane on the right
(rendered markdown, diffs, or syntax-highlighted content). herdr is the host (a Rust+ratatui
terminal agent multiplexer); this plugin is built to align with it.

## Current state: spec-complete, NO source code yet

This repo currently contains **only the specification**, produced through the CodeRight
pipeline. There is no `Cargo.toml`, no `src/`, no manifest yet — those are created by executing
the plan. **Before writing any code, read the spec chain in order:**

1. `specs/file-viewer/brief.md` — problem, scope, non-goals
2. `specs/file-viewer/acceptance-criteria.md` — the contract: 32 criteria (AC-1…AC-27, AC-N1…AC-N5), each with its verification type
3. `specs/file-viewer/design.md` — the 10 logical components, their contracts, data flow, trust boundaries, and the AC→component map
4. `specs/file-viewer/techstack.md` — the concrete, version-pinned stack + the herdr CLI/manifest surface
5. `specs/file-viewer/plan.md` — **the build instructions**: 25 dependency-ordered, test-first tasks (T-1…T-25)
6. `specs/file-viewer/verify-report.md` — the readiness gate (verdict: READY TO BUILD)

`CONTEXT.md` (glossary) and `constitution.md` (standing principles) are repo-wide; `docs/adr/`
records the two hard-to-reverse decisions.

## Building it

**Execute `plan.md` task-by-task, in order, starting at T-1**, each via red-green-refactor:
write the named failing test first, then the smallest code to pass, keeping `cargo test` green
between tasks. Each task names its exact files, its failing test, and the AC + component it
advances. Do not jump ahead or batch tasks — they leave the repo green individually.

Once T-1 scaffolds the crate, the commands are standard Cargo:

```bash
cargo test                      # all unit + integration + e2e tests
cargo test <name>               # a single test by name substring
cargo test --test <file>        # one integration test file (e.g. --test tree_filters)
cargo build --release           # what herdr's [[build]] step runs at install time
cargo run                       # run the viewer locally (outside herdr)
```

The crate is a **library (`src/lib.rs` + modules) + thin binary (`src/main.rs` → `run()`)** so
components are unit-testable; integration/e2e tests live in `tests/`.

## Architecture (the big picture)

A **single in-process TUI owns both columns** (ADR-0002) — it is not composed of multiple herdr
panes. Logical components and their one-line responsibilities (full contracts in `design.md`):

- **Host Adapter** — the herdr boundary: manifest declaration + parsing injected context + open-pane requests
- **Root Resolver** — resolve the tree root (worktree root vs cwd) and git-presence
- **Tree Model** — the rooted, gitignore-aware file tree + filters + cursor
- **Git Service** — read-only git queries (status, baseline, changed-set, diff)
- **View Policy** — pure decision: which view mode for a file (changed→diff, md→rendered, else→content)
- **Content Renderer** — produce content-pane text by delegating to external CLIs, with guards
- **Presenter** — draw the two-column layout (ratatui)
- **Input Dispatcher** — map key events → intents (crossterm)
- **Session Controller** — orchestrate intents → state changes; holds in-memory session state
- **Editor Launcher** — hand a file off to an external editor / new herdr pane

State is **in-memory and ephemeral only** — no persistent store in v1; the filesystem and git
repo are the read-only source of truth.

## Load-bearing constraints (from `constitution.md`)

These shape every decision; violating one is a design error, not a style nit:

- **Read-only.** No file or git mutations. The editor path is hand-off only. (AC-N1, AC-N2)
- **Delegate rendering.** Reuse external CLIs (`glow` markdown, `delta` diff, `bat` syntax);
  build only the shell. Never reinvent rendering. (ADR-0001)
- **Git is first-class**, woven through the tree and content pane — not a separate mode.
- **Keyboard-first.** Every function reachable by keyboard; no mouse required. (AC-18)
- **Good plugin citizen.** Drive herdr only through its documented CLI/socket; no persistent
  state beyond the plugin's own dirs.
- **YAGNI.** Smallest thing that meets the criteria; resist turning a viewer into a file
  manager or git client.

## Stack specifics (pinned in `techstack.md`)

- **Rust 1.96 (edition 2024)** + **ratatui 0.30.1** (uses `ratatui-core` 0.1.x) + **crossterm 0.29.0**
- **`ansi-to-tui` 8.0.1** ingests the external renderers' ANSI output into ratatui spans — and
  doubles as the **AC-27 escape-neutralizer** (maps styling, drops cursor/screen-control). All
  file content flows through it.
- **`ignore` 0.4.26** for fast, `.gitignore`-aware tree walking (do not hand-roll gitignore).
- **git via the system CLI** (read-only subcommands only) — no `git2`/`gix`.
- **`serde`/`serde_json`** only for parsing `HERDR_PLUGIN_CONTEXT_JSON`.
- Tests: `cargo test` + ratatui `TestBackend` + **`insta`** (snapshots) + **`expectrl`** (pty e2e).
- No `tokio` (off-thread rendering uses `std::thread`+`mpsc`), no `clap`.

## herdr integration (verified surface)

- **Manifest** `herdr-plugin.toml`: declare the viewer as a `[[panes]]` entry with
  `placement = "split"` and `command = ["./target/release/herdr-file-viewer"]`, plus an
  `[[actions]]` to summon it; `min_herdr_version = "0.7.0"`, `platforms = ["linux","macos"]`,
  `[[build]] command = ["cargo","build","--release"]`. **No `[[events]]`** (AC-N4).
- **Runtime** (editor hand-off): via `$HERDR_BIN_PATH` — `pane split <id> --direction right
  --no-focus` → parse `result.pane.pane_id` from the JSON → `pane run <new_id> "<editor> <file>"`.
- External renderers (glow/delta/bat) are **runtime, install-time** dependencies, not Cargo
  deps; the Content Renderer falls back to plain text + a notice when one is absent (AC-24/25).
- Make external commands (renderers, editor, herdr CLI) **injected parameters** so tests stay
  hermetic — never depend on glow/delta/bat or a live herdr in unit/integration tests.

## Working in this repo

- The spec is the contract. To change scope/criteria/design/stack, edit the artifact at the
  **owning stage** and **re-run the readiness check** — don't ad-hoc-edit downstream specs.
- `.claude/` (CodeRight skills, local settings) is intentionally gitignored — it's local
  tooling, not part of the plugin.
- Default branch `main`; remote `origin` is a private GitHub repo.
