# AGENTS.md

## Routing guideline

Stranger litmus test: would this instruction make sense to a stranger who cloned this repo? If
no, it belongs in AGENTS.local.md.

A gitignored AGENTS.local.md may exist beside this file; if present, read and follow it before starting work.

Pointer files carry no content: edits go to AGENTS.md or AGENTS.local.md, never CLAUDE.md: it is a
frozen one-line pointer and says so in-file.

Lazy creation: if an agent has private-routed content (per the litmus test above) and no
AGENTS.local.md exists yet in this working copy, it creates one; the committed .gitignore entry
already covers it, so the pattern self-propagates to every clone.

@AGENTS.local.md

## Project overview

**Cross-agent source of truth for this repo.** Any coding agent (Claude Code, Cursor, Codex,
Aider, …) should read this first. It is intentionally vendor-neutral: agent-specific entry files
(e.g. `CLAUDE.md`) import or point at this file rather than duplicating it.

> **Maintainability rule:** standing project rules live HERE, once. Don't copy them into per-agent
> files. Those should be thin shims that `@import`/reference this.

Companion docs:

- **`CONTEXT.md`**: the glossary (canonical vocabulary).
- **`constitution.md`**: the standing principles (the source for "Load-bearing constraints").

### What this is

A **herdr plugin**: a git-aware, read-only **file viewer**: a keyboard-driven TUI that opens in a
herdr split pane, with a directory tree on the left and a content pane on the right (rendered
markdown, diffs, or syntax-highlighted content). herdr is the host (a Rust+ratatui terminal agent
multiplexer); this plugin is built to align with it.

### Current state: BUILT & SHIPPED

The plugin is fully built and shipped publicly to **`smarzban/herdr-file-viewer`**. `Cargo.toml`,
`src/` (lib + modules + thin binary), `herdr-plugin.toml`, CI, and tagged releases all exist.
`main` is **protected** (PR + green CI required; force-push/delete blocked).

### Architecture (the big picture)

A **single in-process TUI owns both columns** (ADR-0002). It is not composed of multiple herdr
panes. Logical components and their one-line responsibilities (full contracts in `ARCHITECTURE.md`
and the spec chain):

- **Host Adapter**: the herdr boundary: manifest declaration + parsing injected context + open-pane requests
- **Root Resolver**: resolve the tree root (worktree root vs cwd) and git-presence
- **Tree Model**: the rooted, gitignore-aware file tree + filters + cursor
- **Git Service**: read-only git queries (status, baseline, changed-set, diff)
- **View Policy**: pure decision: which view mode for a file (changed→diff, md→rendered, else→content)
- **Content Renderer**: produce content-pane text by delegating to external CLIs, with guards
- **Presenter**: draw the two-column layout (ratatui)
- **Input Dispatcher**: map key events → intents (crossterm)
- **Session Controller**: orchestrate intents → state changes; holds in-memory session state
- **Editor Launcher**: hand a file off to an external editor / new herdr pane

State is **in-memory and ephemeral only**: no persistent store in v1; the filesystem and git repo
are the read-only source of truth. (`ARCHITECTURE.md` is the committed module map; keep it current.)

### Load-bearing constraints (from `constitution.md`)

These shape every decision; violating one is a design error, not a style nit:

- **Read-only.** No file or git mutations. The editor path is hand-off only. (AC-N1, AC-N2)
- **Delegate rendering.** Reuse external CLIs (`glow` markdown, `delta` diff, `bat` syntax); build
  only the shell. Never reinvent rendering. (ADR-0001)
- **Git is first-class**, woven through the tree and content pane, not a separate mode.
- **Keyboard-first.** Every function reachable by keyboard; no mouse required. (AC-18)
- **Good plugin citizen.** Drive herdr only through its documented CLI/socket; no persistent state
  beyond the plugin's own dirs.
- **YAGNI.** Smallest thing that meets the criteria; resist turning a viewer into a file manager or
  git client.

### Stack specifics

- **Rust 1.96 (edition 2024)** + **ratatui 0.30.1** (uses `ratatui-core` 0.1.x) + **crossterm 0.29.0**
- **`ansi-to-tui` 8.0.1** ingests the external renderers' ANSI output into ratatui spans, and
  doubles as the **AC-27 escape-neutralizer** (maps styling, drops cursor/screen-control). All file
  content flows through it.
- **`ignore` 0.4.26** for fast, `.gitignore`-aware tree walking (do not hand-roll gitignore).
- **git via the system CLI** (read-only subcommands only), no `git2`/`gix`.
- **`serde`/`serde_json`** only for parsing `HERDR_PLUGIN_CONTEXT_JSON`.
- Tests: `cargo test` + ratatui `TestBackend` + **`insta`** (snapshots) + **`expectrl`** (pty e2e).
- No `tokio` (off-thread rendering uses `std::thread`+`mpsc`), no `clap`. **Minimal-deps house
  style**: adding a crate is a deliberate decision, not a default.

### herdr integration (verified surface)

- **Check herdr's live docs/CLI before you scope OR build anything that touches the host boundary.**
  This section is called *verified surface* for a reason: herdr evolves, so never assume a command,
  flag, or JSON shape from memory. Confirm it against the installed herdr first: `herdr --help`,
  `herdr <cmd> --help` (e.g. `herdr pane --help`), a read-only probe of the real output (`herdr pane
  current`, `herdr pane layout --current`), and the `herdr` skill when running inside herdr
  (`HERDR_ENV=1`). Pin the exact argv you verified in a test comment so a future change can't
  silently break it.
- **Manifest** `herdr-plugin.toml`: declare the viewer as a `[[panes]]` entry with
  `placement = "split"` and `command = ["./target/release/herdr-file-viewer"]`, plus an
  `[[actions]]` to summon it; `min_herdr_version = "0.7.0"`, `platforms = ["linux","macos"]`,
  `[[build]] command = ["cargo","build","--release"]`. **No `[[events]]`** (AC-N4).
- **Runtime** (editor hand-off): via `$HERDR_BIN_PATH`: `pane split <id> --direction right
  --no-focus` → parse `result.pane.pane_id` from the JSON → `pane run <new_id> "<editor> <file>"`.
- External renderers (glow/delta/bat) are **runtime, install-time** dependencies, not Cargo deps;
  the Content Renderer falls back to plain text + a notice when one is absent (AC-24/25).
- Make external commands (renderers, editor, herdr CLI) **injected parameters** so tests stay
  hermetic, never depend on glow/delta/bat or a live herdr in unit/integration tests.

## Build / test / verify

The crate is a **library (`src/lib.rs` + modules) + thin binary (`src/main.rs` → `run()`)** so
components are unit-testable; integration/e2e tests live in `tests/`.

```bash
cargo test                      # all unit + integration + e2e tests
cargo test <name>               # a single test by name substring
cargo test --test <file>        # one integration test file (e.g. --test tree_filters)
cargo build --release           # what herdr's [[build]] step runs at install time
cargo run                       # run the viewer locally (outside herdr)

# deterministic health tier (keep green):
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo audit
```

## Conventions

### Working in this repo

- **The spec is the contract.** To change scope/criteria/design/stack, edit the artifact at the
  **owning stage** and **re-run the readiness check**, don't ad-hoc-edit downstream specs.
- **Definition of done for a user-facing feature:** the feature isn't done until the docs match it,
  IN the same PR: `CHANGELOG.md` entry, the relevant `docs/` page (`docs/keys.md` for a key + the
  Shift-keys note for a capital-letter key, `docs/usage.md` for the feature, `docs/configuration.md`
  for a config key), and `ARCHITECTURE.md`'s module table if components changed. The root `README.md`
  is a lean front door (a taste of keys + links to `docs/`), NOT the full reference: keep detail in
  `docs/`, not the README.
- **Verify the branch base before a PR.** Worktrees here are often branched off a feature commit,
  not `main`; always `git log main..HEAD` before committing/opening a PR, or strays get swept in.
- Keep the deterministic tier green (fmt/clippy/`cargo audit`) and tests hermetic.

### Releasing a version (owner-gated, confirm first)

1. **Bump the version in ALL THREE files**: `Cargo.toml`, `Cargo.lock`, **and `herdr-plugin.toml`**:
   herdr DISPLAYS the *manifest* version, so a missed `herdr-plugin.toml` ships a wrong version
   string. `release.yml` fails the build unless the tag matches **both** `Cargo.toml` and
   `herdr-plugin.toml`. Versioning: **minor per additive feature**, major only on a breaking change
   or a flagship feature.
2. Add the `## [X.Y.Z] - DATE` `CHANGELOG.md` entry (Keep-a-Changelog `Added`/`Changed`/`Fixed`,
   omit empty sections). Show the owner the release notes before posting.
3. Protected `main` → bump via a **`release/vX.Y.Z` PR** → green CI → merge.
4. **Tag `vX.Y.Z` AT the merge commit** (`git tag -a vX.Y.Z <merge-sha>` → push) so a bare
   `herdr plugin install`'s tagless-clone `HEAD` matches the published `COMMIT` asset. The tag push
   triggers `release.yml` (builds 3 binaries + `SHA256SUMS` + `COMMIT`, `--generate-notes`).
5. **Replace the auto-notes** with the approved body: `gh release edit vX.Y.Z --notes-file <f>`.
6. **Verify**: `gh release view vX.Y.Z` shows 5 assets, not draft/prerelease.

**Install gate (current, since PR #50):** the prebuilt binary is used by **declared version match**,
not commit-exact; main being ahead of the tag no longer forces a source build. So features can
batch into one release. Caveat: a change to how a launcher script/manifest **invokes** the binary
must bump the version in that same commit.
