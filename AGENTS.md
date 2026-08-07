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
- **`ARCHITECTURE.md`**: the committed module map (keep it current when components change).
- **`docs/`** (index: `docs/README.md`): the user-facing docs — `keys.md` (the full key/mouse
  reference), `configuration.md` (the `config.toml` + `[keys]` reference), `usage.md` (per-feature
  guide), plus `install.md` / `summoning.md` / `renderers.md` / `windows.md`. The root `README.md` is
  a lean front door that links into these; reference detail lives in `docs/`, never the README.

### What this is

A **herdr plugin**: a git-aware, read-only **file viewer**: a keyboard-driven TUI that opens in a
herdr pane — a split, its own tab, or an overlay over the active pane — or in a floating popup
(which is *not* a pane), with a directory tree on the left and a content pane on the right
(rendered markdown, diffs, or syntax-highlighted content). herdr is the host (a Rust+ratatui
terminal agent multiplexer); this plugin is built to align with it.

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
- **Official Repository Gateway**: fixed-source, bounded, display-only remote notices
- **Content Renderer**: produce content-pane text by delegating to external CLIs, with guards
- **Presenter**: draw the two-column layout (ratatui)
- **Input Dispatcher**: map key events → intents (crossterm)
- **Session Controller**: orchestrate intents → state changes; holds in-memory session state
- **Editor Launcher**: hand a file off to an external editor in-process, suspending and resuming the
  TUI around it (NOT a herdr pane — see the herdr integration section)

State is **in-memory and ephemeral only** except for the safe-to-delete, advisory
`update-check.json` cache, which never changes the viewed root or git repo.
(`ARCHITECTURE.md` is the committed module map; keep it current.)

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

- **Rust 1.96 (edition 2024)** + **ratatui 0.30.x** (uses `ratatui-core` 0.1.x) + **crossterm 0.29.0**
- **`ansi-to-tui` 8.0.1** ingests the external renderers' ANSI output into ratatui spans, and
  doubles as the **AC-27 escape-neutralizer** (maps styling, drops cursor/screen-control). All file
  content flows through it.
- **`ignore` 0.4.26** for fast, `.gitignore`-aware tree walking (do not hand-roll gitignore).
- **git via the system CLI** (read-only subcommands only), no `git2`/`gix`.
- **`serde`/`serde_json`** parse `HERDR_PLUGIN_CONTEXT_JSON` and the advisory cache.
- **System `curl`** is optional at runtime: it retrieves fixed official HTTPS documents for
  advisory notices; without it, document retrieval is unavailable without an error.
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
- **Manifest** `herdr-plugin.toml`: declare the viewer as a **single** `[[panes]]` entry with
  `placement = "split"` and `command = ["./target/release/herdr-file-viewer"]` — that placement is
  only the entry's default. The **four layouts (split, tab, overlay, popup) are chosen at *open*
  time** by a launcher script's `plugin pane open --placement …`, so one pane entry is summoned by
  **six `[[actions]]`**: the four unix ones (`open-file-viewer`, `-tab`, `-overlay`, `-popup`) plus
  the two existing `-windows` split/tab variants. Overlay and popup are linux+macos only this
  release. `min_herdr_version = "0.7.4"` — session-modal popups for plugins, with cell/percentage
  sizing, landed in herdr 0.7.4 (#1125), so that is the honest floor once popup ships (overlay
  alone would have needed no bump); `platforms = ["linux","macos","windows"]`
  (Windows is preview, with per-item launcher entries), and **platform-gated `[[build]]` steps**
  (`["/bin/sh","scripts/fetch-or-build.sh"]` on unix, `powershell … scripts/fetch-or-build.ps1` on
  Windows) that download the verified prebuilt binary and fall back to `cargo build --release`.
  **No `[[events]]`** (AC-N4).
- **Runtime host ops** via the herdr CLI (`$HERDR_BIN_PATH`, the `HerdrCli::run` / `run_json` seam in
  `src/herdr.rs`): read-only layout/query commands only — e.g. `pane zoom` (the `Z` full-screen), the
  worktree picker's queries, and the tab/split launcher scripts. The **editor hand-off is NOT a herdr
  pane**: `e` runs the editor *in-process* (the viewer suspends and resumes around `$EDITOR` / the
  config `editor`), so the viewer never spawns a pane for it.
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
- **Never weaken a spec-backed assertion to make a change pass.** A test that names an acceptance
  criterion, and a policy list an AC enumerates (e.g. the exhaustive read-only matrix in
  `src/intent.rs::intent_effects_never_mutate_files_or_git_and_classify_annotation_edits`, which
  AC-N3 defines row by row), are the contract in executable form: deleting an
  entry to accommodate new code silently changes behaviour the spec mandates — in the case that
  prompted this rule, a required user-visible notice became a silent no-op. If a criterion genuinely
  should change, change it at the owning stage first (above) and say so in the PR. If a spec-backed
  test looks wrong, STOP and ask rather than editing it away.
- **Definition of done for a user-facing feature:** the feature isn't done until the docs match it,
  IN the same PR: `CHANGELOG.md` entry, the relevant `docs/` page (`docs/keys.md` for a key + the
  Shift-keys note for a capital-letter key, `docs/usage.md` for the feature, `docs/configuration.md`
  for a config key), and `ARCHITECTURE.md`'s module table if components changed. The root `README.md`
  is a lean front door (a taste of keys + links to `docs/`), NOT the full reference: keep detail in
  `docs/`, not the README.
- **Verify the branch base before a PR.** Worktrees here are often branched off a feature commit,
  not `main`; always `git log main..HEAD` before committing/opening a PR, or strays get swept in.
- Keep the deterministic tier green (fmt/clippy/`cargo audit`) and tests hermetic.

### Tests prove things deterministically, or they don't count

This suite runs on macOS, Linux and Windows CI runners whose timing and layout differ from any dev
machine. Three rules, each learned from a real failure here. Unlike the drift guards below, these are
prose, not build-failing checks — hold yourself to them.

- **Don't assert a tight time budget, and don't prove a negative by sleeping.** Slack that is a small
  multiple of the thing being measured is a coin flip on a loaded runner (a ~200ms budget asserted
  under 300ms failed twice in one day), and a "nothing happened" poll passes vacuously when the thing
  lands after the window closes. Prefer a synchronous, observable tell: `Controller::render_seq` is
  bumped inside `dispatch_render` BEFORE the worker spawns, so an unchanged seq proves no render was
  dispatched where counting a stub provider's calls only races it.
  - **Where a wait is the point, split the claim in two.** Pin the budget's VALUE clock-free
    (`tests/whats_new_composer.rs` asserts `WHATS_NEW_COMPOSE_TIMEOUT == 200ms` and that every
    document receives exactly `opened_at + WHATS_NEW_COMPOSE_TIMEOUT`), and let the behavioural test
    bound only the wait, ~10x the budget and orders of magnitude below the stall it distinguishes
    from (`HELP_STALLED_RENDERER_MAX_WAIT`). Neither half alone is enough: a widened budget escapes
    the loose bound, and a blocking call escapes the clock-free test.
  - **The honest exception is a criterion that IS a latency budget.** AC-22/AC-23 mandate 300ms, so
    `help_open_switch_scroll_each_within_300ms` (`tests/controller.rs`) must hold a stopwatch — that
    is the spec, not a testing choice. Keep such tests, give them the widest slack the criterion
    permits, and don't add new ones without a criterion behind them.
  - **Two shapes to copy.** For "no work was dispatched", assert `Controller::render_seq` is
    unchanged (`tests/controller_async.rs`) — it is bumped synchronously inside dispatch, before the
    job reaches the render worker. For "bounded, not unbounded", bound a long-stalling fixture (60s
    in `src/proc.rs` / `src/render.rs` / `src/update/gateway.rs`, 30s and an endless loop in
    `tests/render_delegate.rs`) at a couple of seconds, rather than re-measuring the timeout you
    passed in — generous enough for a loaded runner, tight enough to still reject a multi-second
    tail.
  - **Know the render worker's shape before reasoning about ordering.** There is ONE long-lived
    worker (`Controller::spawn_worker`) that takes jobs over a channel in order and collapses a
    backlog, so a newer result landing means every earlier job already finished or was collapsed.
    That ordering is what lets a test assert with no wait at all; assuming thread-per-render instead
    leads to inventing sleeps that prove nothing.
  - **Force the race instead of hoping for it; document a limit only when you truly cannot.** The
    end-to-end superseded-render tests in `tests/controller_async.rs` look like they prove `poll`'s
    `seq == latest_seq` guard and do not: their polling loop drains the earlier result first, so
    removing the guard leaves them green. A gated renderer (`GatedContent`) makes the ordering
    happen on demand — hold one render open, dispatch a newer one behind it, release the first so
    its result arrives stale — and that test DOES fail when the guard is removed. Reach for the gate
    first. Where a race genuinely cannot be forced from outside, write the limit into the test's own
    comment and name what would prove it, so nobody trusts it past its scope.
- **Never send a key that assumes state the test has not observed.** In a pty journey `q`/Esc peels
  ONE state layer per press (`src/controller/mod.rs`: selection → flash → committed search → zoom →
  discard confirm → quit), and toggles like `z` flip whatever is actually there. A journey that
  presses a toggle "to undo" a state it never asserted will break on the runner whose layout decided
  otherwise — which is exactly how a pinned-preview e2e went red 4/4 on ubuntu while green everywhere
  else. Drive the state you depend on, or make the tail independent of it, and say which in a comment.
- **A negative that cannot be reproduced locally is not verified.** macOS-only green means nothing for
  a Linux-only failure: reproduce in a Linux container (`rust:1.96-trixie`, mount the worktree
  read-only, cache `CARGO_TARGET_DIR` in a volume) before claiming a fix, and say plainly when you
  could not.

### Adding a keybinding or a config key (touchpoints + drift guards)

Both surfaces are single-source-of-truth in code, with a build-failing test guarding the docs, so you
never wire them in two places or let the docs drift.

**A new keybinding / action.** `REGISTRY` in `src/input.rs` is the source of truth: the dispatcher,
the `?` overlay's Keybindings section, and `[keys]` remapping all derive from it.
1. Add the variant to the `Intent` enum in `src/intent.rs`, and to its `Intent::ALL` array (whose
   length constant must be bumped with it — read the current count from the source, don't trust a
   number written here).
2. Add a `Binding { intent, name, default_keys, description, category }` row to `REGISTRY`
   (`category` must be one of `CATEGORY_ORDER`).
3. Handle the intent in the session controller (`src/controller/`).
4. **Docs (same PR):** add the key row to the `## Keys` table in **`docs/keys.md`** and the
   intent-name row to the "Every remappable action" table in **`docs/configuration.md`**, plus a
   `CHANGELOG.md` entry. The `?` overlay updates itself — no manual edit. Two `src/input.rs` tests
   fail the build if you skip a doc: `keys_doc_table_documents_every_registry_action_ac21` (every
   registry key is in `docs/keys.md`) and `configuration_doc_lists_every_remappable_intent` (every
   registry name is in `docs/configuration.md`).

**A change to how an agent or a launcher invokes the viewer.** The bundled skill
(`skills/herdr-file-viewer/SKILL.md`), the paste-in block in `docs/usage.md`, and the launcher scripts
(`scripts/open-file-viewer*.sh`) all teach the same launch, and they have drifted apart before: the
skill told agents to pass `--cwd` to `herdr plugin pane open`, which cannot spawn the manifest's
RELATIVE pane command (and inside a built plugin checkout silently runs *that* checkout's binary),
while the shipped launcher never passed it (#139). Change all of them together, and keep
`no_documented_launch_passes_cwd_to_plugin_pane_open` (`tests/docs_consistency.rs`) honest — it holds
the docs and the scripts to one rule so this divergence fails the build instead of reaching a user.
The viewed root comes from the FOCUSED herdr pane's cwd (resolved to its worktree top level), never
from a flag.

**A new config key.** `src/config.rs` owns it: add the field to `Config`, resolve it in `resolve`
into `EffectiveSettings`, and apply it at wiring time. **Docs (same PR):** document it in
**`docs/configuration.md`**, add a commented `key = ...` line to **`config.example.toml`**, surface
the effective value in the `?` Settings section, and add a `CHANGELOG.md` entry. The
`config_example_documents_every_config_key` test (`tests/docs_consistency.rs`) requires a commented
assignment for every scalar `Config` field — keep its key list in lockstep with `Config`.

### Releasing a version (owner-gated, confirm first)

1. **Bump the version in ALL THREE files**: `Cargo.toml`, `Cargo.lock`, **and `herdr-plugin.toml`**:
   herdr DISPLAYS the *manifest* version, so a missed `herdr-plugin.toml` ships a wrong version
   string. `release.yml` fails the build unless the tag matches **both** `Cargo.toml` and
   `herdr-plugin.toml`. Versioning: **minor per additive feature**, major only on a breaking change
   or a flagship feature.
2. Add the `## [X.Y.Z] - DATE` `CHANGELOG.md` entry (Keep-a-Changelog `Added`/`Changed`/`Fixed`,
   omit empty sections; keep bullets terse and credit external contributors `Thanks @user (#NN)`).
   **The CHANGELOG section IS the release notes** (single source of truth) — never author them
   separately, or the two drift. Show the owner the section before posting.
3. Protected `main` → bump via a **`release/vX.Y.Z` PR** → green CI → merge.
4. **Tag `vX.Y.Z` AT the merge commit** (`git tag -a vX.Y.Z <merge-sha>` → push) so a bare
   `herdr plugin install`'s tagless-clone `HEAD` matches the published `COMMIT` asset. The tag push
   triggers `release.yml` (builds **4 binaries** — Linux musl, macOS arm64 + x86_64, Windows `.exe` —
   plus `SHA256SUMS` + `COMMIT`, `--generate-notes`).
5. **Set the release body FROM the CHANGELOG section** (single source of truth, so the notes can't
   drift from the changelog): extract this tag's `## [X.Y.Z]` block, drop the trailing `→ [docs]`
   pointers (a release note is a self-contained, pinned artifact), append a
   `**Full changelog:** <repo>/compare/vPREV...vX.Y.Z` line, then
   `gh release edit vX.Y.Z --notes-file <f>`. Extract with e.g.
   `awk '/^## \[X.Y.Z\]/{f=1;next} f&&/^## \[/{exit} f' CHANGELOG.md`.
6. **Verify**: `gh release view vX.Y.Z` shows **6 assets** (4 binaries + `SHA256SUMS` + `COMMIT`),
   not draft/prerelease.

**Install gate (current, since PR #50):** the prebuilt binary is used by **declared version match**,
not commit-exact; main being ahead of the tag no longer forces a source build. So features can
batch into one release. Caveat: a change to how a launcher script/manifest **invokes** the binary
must bump the version in that same commit.
