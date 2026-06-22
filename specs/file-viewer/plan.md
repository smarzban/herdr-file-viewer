# Plan: herdr-file-viewer

- Status: drafted (plan stage) — 2026-06-18
- Amended: 2026-06-18 — T-1 manifest test → `[[panes]] placement`; T-17 herdr open-pane
  pinned; AC-23 tasks aligned to the responsiveness reword; added T-25 (AC-17 manual);
  AC-16 coverage fix (resolving readiness-check H-1/H-2/M-1 + L-1).
- Inputs: `acceptance-criteria.md`, `design.md`, `techstack.md`, `constitution.md`
- Next: readiness-check gate → build

Dependency-ordered, atomic tasks. Each leaves the repo green, names exact files, states
the failing test to write first, and references the `AC-N` it advances and the design
component it touches. Build executes each via red-green-refactor; **this stage writes no
product code.**

Crate layout: a library crate (`src/lib.rs` + modules) with a thin binary (`src/main.rs`).
Unit tests live inline (`#[cfg(test)]`); integration/e2e tests live in `tests/`. Each task
adds its `pub mod` to `src/lib.rs` as it lands.

## Tasks

### T-1 — Scaffold crate, dependencies, and manifest
- **Files:** `Cargo.toml`, `src/lib.rs`, `src/main.rs`, `herdr-plugin.toml`, `.gitignore`
- **Test first:** `tests/manifest.rs` — read `herdr-plugin.toml` to a string and assert: a
  `[[panes]]` entry with `placement = "split"` whose `command` is the release binary
  (AC-17); at least one `[[actions]]`; `min_herdr_version = "0.7.0"`; a `[[build]]`
  `command = ["cargo", "build", "--release"]`; `platforms = ["linux","macos"]`; and **no
  `[[events]]` table** (AC-N4).
- **Implements:** Cargo.toml with pinned deps (ratatui 0.30.1, crossterm 0.29.0, ansi-to-tui
  8.0.1, ignore 0.4.26, serde 1.0.228 + serde_json 1.0.150; dev: insta 1.48.0, expectrl
  0.9.0); `src/main.rs` calls `herdr_file_viewer::run()` (stub `Ok(())`); the manifest with a
  `[[panes]]` (`placement = "split"`, `command = ["./target/release/herdr-file-viewer"]`),
  an `[[actions]]` to summon it, and `[[build]]` — no `[[events]]`/`[[link_handlers]]`;
  `.gitignore` (`/target`).
- **AC:** AC-17 (declares split-pane launch), AC-N4 (no event hooks). **Component:** Host Adapter.
- **Deps:** none.

### T-2 — View Policy: default mode + applicable set
- **Files:** `src/view_policy.rs`
- **Test first:** inline unit tests: unchanged markdown → `RenderedMarkdown`; changed file →
  `Diff` even when markdown; non-changed non-markdown → `SyntaxContent`; a changed file's
  `applicable_modes` offers `FullDiff` (full-context diff) after the compact `Diff`.
- **Implements:** `enum ViewMode`, `struct FileDescriptor { path, is_markdown, is_changed }`,
  `fn default_mode(&FileDescriptor) -> ViewMode`, `fn applicable_modes(&FileDescriptor) -> Vec<ViewMode>`.
- **AC:** AC-8, AC-9, AC-10, AC-11. **Component:** View Policy. **Deps:** T-1.

### T-3 — Root Resolver + LaunchContext contract
- **Files:** `src/context.rs` (the `LaunchContext` struct), `src/root.rs`
- **Test first:** `tests/root.rs`: (a) plain temp dir → `root == dir`, `is_git_repo == false`;
  (b) `git init` temp repo → `root == repo root`, `is_git_repo == true`; (c) `git worktree add`
  → `root == worktree root`, `is_worktree == true`.
- **Implements:** `struct LaunchContext { cwd, worktree_root, base_branch, is_worktree }`;
  `struct Resolved { root, is_git_repo, repo_root, is_worktree, base_branch }`;
  `fn resolve(&LaunchContext) -> Resolved`.
- **AC:** AC-1, AC-2, AC-26 (git-presence detection). **Component:** Root Resolver. **Deps:** T-1.

### T-4 — Git Service: read-only invoker + per-file status
- **Files:** `src/git.rs`
- **Test first:** `tests/git_status.rs`: temp repo with modified/added/deleted/untracked
  files → `status()` returns the correct `Status` per path; after calling all query methods,
  assert `git status` shows the repo unchanged (AC-N2).
- **Implements:** read-only git runner (`std::process`, capture stdout), `enum Status`,
  `fn status(repo_root) -> Map<PathBuf, Status>`. No mutating subcommands exist.
- **AC:** AC-7, AC-N2. **Component:** Git Service. **Deps:** T-1.

### T-5 — Git Service: baseline resolution, changed-set, diff
- **Files:** `src/git.rs` (extend)
- **Test first:** `tests/git_baseline.rs`: on the default branch → `default_baseline == Head`,
  changed-set = uncommitted; on a feature branch forked from main with commits →
  `default_baseline == Base`, changed-set includes committed work; toggling baseline changes
  the changed-set and diff; `diff(path, baseline)` returns raw unified diff text.
- **Implements:** `enum Baseline { Head, Base }`, base-branch detection (context hint →
  `git merge-base`/default-branch fallback), `fn default_baseline(&Resolved) -> Baseline`,
  `fn changed_set(repo, Baseline) -> Map<PathBuf, Status>`, `fn diff(repo, path, Baseline) -> String`.
- **AC:** AC-9 (diff text), AC-14, AC-15, AC-16. **Component:** Git Service. **Deps:** T-4.

### T-6 — Tree Model: gitignore-aware recursive enumeration, root boundary
- **Files:** `src/tree.rs`
- **Test first:** `tests/tree.rs`: temp dir with nested dirs + a `.gitignore` →
  `visible_nodes()` is recursive on expand (AC-3); ignored entries absent by default (AC-4);
  resolving above `root` is a no-op (AC-N5); filesystem unchanged after use (AC-N1).
- **Implements:** `struct TreeModel { root, expanded, cursor }`, `struct Node { path, kind,
  depth, expanded, status: Option<Status> }`, lazy enumeration via the `ignore` crate,
  `fn visible_nodes(&self) -> Vec<Node>`, expand/collapse/cursor methods. Read-only.
- **AC:** AC-3, AC-4, AC-N5, AC-N1. **Component:** Tree Model. **Deps:** T-1, T-4 (Status type).

### T-7 — Tree Model: ignore toggle + changed-only filter
- **Files:** `src/tree.rs` (extend)
- **Test first:** `tests/tree_filters.rs`: `set_show_ignored(true)` reveals ignored files,
  `false` hides them (AC-5); `set_changed_only(true, changed_set)` shows only changed files,
  `false` restores the full tree (AC-6).
- **Implements:** `fn set_show_ignored(bool)`, `fn set_changed_only(bool, &Map<PathBuf,Status>)`.
- **AC:** AC-5, AC-6. **Component:** Tree Model. **Deps:** T-6, T-5 (changed-set shape).

### T-8 — Tree Model: perf — interactive ≤ 1 s at 10k files
- **Files:** `tests/tree_perf.rs`
- **Test first:** generate a temp tree of 10,000 files; assert `TreeModel::new(root)` + first
  `visible_nodes()` completes in < 1 s (`std::time::Instant`).
- **Implements:** nothing new if T-6 is lazy; otherwise optimize enumeration to pass.
- **AC:** AC-22. **Component:** Tree Model. **Deps:** T-6.

### T-9 — Content Renderer: binary detection + size cap
- **Files:** `src/render.rs`
- **Test first:** inline unit tests: file with NUL bytes → `Prepared::Binary` (placeholder,
  no raw bytes) (AC-12); text file ≥ 1 MB or ≥ 5,000 lines → `Prepared::Truncated` with a
  bounded preview + truncation notice (AC-13); reads only (AC-N1).
- **Implements:** `enum Prepared { Binary, Truncated { text, notice }, Full { text } }`,
  `fn classify(path) -> Prepared` (NUL/UTF-8 + byte/line-count heuristics).
- **AC:** AC-12, AC-13, AC-N1. **Component:** Content Renderer. **Deps:** T-1.

### T-10 — Content Renderer: escape-sequence neutralization
- **Files:** `src/render.rs` (extend)
- **Test first:** `tests/render_escape.rs`: ingest content containing cursor-move / screen-
  clear sequences (`\x1b[2J`, `\x1b[10;10H`) → resulting `ratatui::text::Text` reproduces no
  cursor/screen-control ops (styling-only or literal text) (AC-27).
- **Implements:** `fn to_text(raw: &str) -> Text` via `ansi-to-tui`, with cursor/screen-control
  sequences stripped on the raw path.
- **AC:** AC-27. **Component:** Content Renderer. **Deps:** T-9.

### T-11 — Content Renderer: delegate to external renderers + fallback + notice
- **Files:** `src/render.rs` (extend)
- **Test first:** `tests/render_delegate.rs` (renderer commands injected so tests are
  hermetic — substitute `printf`/`cat` or a missing path): `SyntaxContent` → invokes the
  syntax command, output ingested (AC-10); `RenderedMarkdown` / `Diff` likewise (AC-8, AC-9);
  a **nonexistent** renderer → plain-text fallback, no crash/empty pane (AC-24) **and** a
  notice naming the missing capability (AC-25).
- **Implements:** `struct Renderers { markdown, diff, syntax }` (command paths), `fn render(
  &Prepared, ViewMode, raw_diff: Option<&str>) -> (Text, Option<Notice>)` — spawn via
  `std::process`, capture stdout, ingest via `to_text`; spawn error / non-zero → plain text +
  `Notice`.
- **AC:** AC-8, AC-9, AC-10, AC-24, AC-25. **Component:** Content Renderer. **Deps:** T-10, T-2.

### T-12 — Content Renderer: perf — in-process ingest ≤ 300 ms at 1 MB
- **Files:** `tests/render_perf.rs`
- **Test first:** classify + `to_text` of a 1 MB text file completes in < 300 ms (`Instant`)
  — the in-process portion of the AC-23 responsiveness bound.
- **Implements:** optimize ingestion if needed (the external-process portion runs off-thread,
  T-19).
- **AC:** AC-23 (responsiveness — in-process bound). **Component:** Content Renderer. **Deps:** T-11.

### T-13 — Input Dispatcher: key → intent, keyboard-complete, no edit intent
- **Files:** `src/intent.rs` (the `Intent` enum), `src/input.rs`
- **Test first:** inline unit tests: each bound key maps to its `Intent` (Nav, Expand/Collapse,
  ToggleIgnore, ToggleChangedOnly, ToggleBaseline, CycleView, OpenInEditor, ToggleFocus,
  Close) (AC-18); unmapped key → `None`; every `Intent` variant has ≥ 1 key (keyboard-
  complete, AC-18); **no `Intent` variant edits a file's contents** (AC-N3).
- **Implements:** `enum Intent { … }`, `fn map_key(crossterm::event::KeyEvent) -> Option<Intent>`.
- **AC:** AC-18, AC-11 (trigger), AC-20 (trigger), AC-N3. **Component:** Input Dispatcher. **Deps:** T-1.

### T-14 — Presenter: two-column layout, tree display, status markers, notices
- **Files:** `src/presenter.rs`
- **Test first:** `tests/presenter.rs` with ratatui `TestBackend` + `insta` snapshots: a known
  tree+content `ViewState` → two columns, recursive indentation (AC-3 display), per-file
  status markers (AC-7 display), truncation notice (AC-13), fallback notice (AC-25).
- **Implements:** `struct ViewState { nodes, content: Text, notices, focus, width }`,
  `fn draw(frame, &ViewState)`.
- **AC:** AC-3 (display), AC-7 (display), AC-13 (notice), AC-25 (notice). **Component:** Presenter. **Deps:** T-6, T-11.

### T-15 — Presenter: narrow-split focus-toggle (< 80 cols)
- **Files:** `src/presenter.rs` (extend)
- **Test first:** `tests/presenter_narrow.rs`: width < 80 + focus = tree → snapshot shows tree
  full-width, content hidden; focus = content → content full-width; width ≥ 80 → both columns.
- **Implements:** width-aware layout branching in `draw`.
- **AC:** AC-21. **Component:** Presenter. **Deps:** T-14.

### T-16 — Editor Launcher: hand-off to editor / new pane
- **Files:** `src/editor.rs`
- **Test first:** `tests/editor.rs` (command runner / host hook injected, nothing really
  launched): target = Editor → spawned command == configured editor + selected file path;
  target = NewPane → a herdr open-pane request carries the file; the file is never written
  (AC-N1).
- **Implements:** `enum Target { Editor, NewPane }`, `fn open(path, Target, &mut impl Spawner)`.
- **AC:** AC-19, AC-N1. **Component:** Editor Launcher. **Deps:** T-1.

### T-17 — Host Adapter: parse injected context + open-pane request
- **Files:** `src/host.rs`
- **Test first:** `tests/host_context.rs`: with `HERDR_PLUGIN_CONTEXT_JSON` + `HERDR_*` env set
  → `from_env()` yields a populated `LaunchContext`; malformed/missing JSON → minimal
  `{ cwd }`, no panic; with an injected `Spawner`, `open_pane(path)` issues two `$HERDR_BIN_PATH`
  calls — `pane split <current_pane_id> --direction right --no-focus`, then `pane run
  <result.pane.pane_id> "<editor> <path>"` — asserting the exact argv and that the new pane id
  is parsed from the split's JSON `result.pane.pane_id`.
- **Implements:** `fn from_env() -> LaunchContext` (serde_json over the context JSON +
  `HERDR_*` vars; current pane id from context or `herdr pane list`), `fn open_pane(path, editor,
  &mut impl Spawner)` issuing the `pane split` → parse `result.pane.pane_id` → `pane run`
  sequence against `$HERDR_BIN_PATH`.
- **AC:** AC-19 (new-pane path), AC-26 (degrade on missing context). **Component:** Host Adapter. **Deps:** T-3.

### T-18 — Session Controller: state + intent orchestration
- **Files:** `src/controller.rs`
- **Test first:** `tests/controller.rs` (components behind traits, stubbed): ToggleIgnore →
  tree `show_ignored` flips + redraw signalled (AC-5); ToggleChangedOnly → flips (AC-6);
  CycleView → view override advances through `applicable_modes` (AC-11); ToggleBaseline →
  Git Service recompute invoked + state updated (AC-16); `is_git_repo == false` → git intents
  inert, no error (AC-26); a component error → non-fatal notice, loop continues; no edit path
  exists (AC-N3).
- **Implements:** `struct SessionState`, component traits for stubbing, `fn handle(Intent) ->
  Effects`, redraw signalling.
- **AC:** AC-5, AC-6, AC-11, AC-16, AC-26, AC-N3. **Component:** Session Controller. **Deps:** T-2, T-5, T-6, T-7, T-11, T-13.

### T-19 — Session Controller: off-thread rendering (non-blocking UI)
- **Files:** `src/controller.rs` (extend)
- **Test first:** `tests/controller_async.rs`: a select intent dispatches rendering on a
  worker thread; `handle()` returns/redraws within the AC-23 300 ms responsiveness bound and
  never blocks input while a deliberately slow renderer stub finishes later via an `mpsc`
  channel; the rendered content then arrives as a later effect (AC-23).
- **Implements:** `std::thread` + `std::sync::mpsc` render dispatch; content arrives as a
  later effect.
- **AC:** AC-23 (responsiveness — non-blocking). **Component:** Session Controller. **Deps:** T-18, T-12.

### T-20 — main wiring: assemble + run loop
- **Files:** `src/main.rs`, `src/lib.rs` (`run()`)
- **Test first:** `tests/cli_smoke.rs` (expectrl pty): spawn the built binary in a temp dir,
  assert it draws a known filename, press the Close key → process exits 0 (AC-20).
- **Implements:** `fn run()` wiring Host Adapter → Root Resolver → Controller → Presenter/Input
  event loop (crossterm raw mode + restore on exit).
- **AC:** AC-17 (launch behavior), AC-20. **Component:** Session Controller / main. **Deps:** T-3, T-13, T-14, T-15, T-17, T-18, T-19.

### T-21 — e2e: keyboard-complete operability
- **Files:** `tests/e2e_keyboard.rs`
- **Test first:** drive only the keyboard over a pty: navigate, expand, toggle ignore, toggle
  changed-only, toggle baseline, cycle view, focus-toggle, open-in-editor (stub editor via
  `EDITOR`), close — each produces the expected screen change; no mouse used (AC-18).
- **AC:** AC-18 (e2e). **Component:** integration. **Deps:** T-20, T-16.

### T-22 — e2e: open-in-editor hand-off
- **Files:** `tests/e2e_editor.rs`
- **Test first:** set `EDITOR` to a recording script; select a file, press open-in-editor;
  assert the script was invoked with the selected file path and the file content is unchanged
  (AC-19, AC-N1).
- **AC:** AC-19 (e2e), AC-N1. **Component:** integration. **Deps:** T-20, T-16.

### T-23 — e2e: non-git directory degrades gracefully
- **Files:** `tests/e2e_nongit.rs`
- **Test first:** launch in a plain (non-git) temp dir; assert tree browsing + a file renders;
  assert git features (status markers, diff, changed-only, baseline toggle) are inactive and
  their keys do not error (AC-26).
- **AC:** AC-26 (e2e). **Component:** integration. **Deps:** T-20.

### T-24 — README + runtime-dependency / install guidance
- **Files:** `README.md`
- **Verification (reviewer-checked — docs):** README documents install (herdr `cargo build
  --release`), the optional external renderers (glow/delta/bat) and that the viewer falls back
  to plain text + a notice when one is absent (AC-24/AC-25), and the summon keybinding.
- **AC:** AC-17 (install/launch), AC-24, AC-25 (documents the fallback + deps). **Component:** Host Adapter (packaging). **Deps:** T-1, T-11.

### T-25 — Manual verification: live herdr launch
- **Files:** `docs/manual-verification.md`
- **Verification (test-backed: manual):** a written, repeatable procedure — link the plugin
  into a running herdr, invoke the declared action, confirm the viewer opens in a **split**
  pane in the current workspace, and that the Close key returns control to the prior pane.
  This covers the live-herdr behavior of AC-17 that cannot be reliably automated in CI; the
  static `placement = "split"` declaration is automatically checked by T-1.
- **AC:** AC-17 (live launch), AC-20 (close, manual confirmation). **Component:** Host Adapter. **Deps:** T-20.

## Task → criterion coverage map

| Criterion | Advanced by |
| --- | --- |
| AC-1 | T-3 |
| AC-2 | T-3 |
| AC-3 | T-6, T-14 |
| AC-4 | T-6 |
| AC-5 | T-7, T-18, T-21 |
| AC-6 | T-7, T-18, T-21 |
| AC-7 | T-4, T-14 |
| AC-8 | T-2, T-11 |
| AC-9 | T-2, T-5, T-11 |
| AC-10 | T-2, T-11 |
| AC-11 | T-2, T-13, T-18 |
| AC-12 | T-9 |
| AC-13 | T-9, T-14 |
| AC-14 | T-5 |
| AC-15 | T-5 |
| AC-16 | T-5, T-14, T-18 |
| AC-17 | T-1, T-20, T-24, T-25 |
| AC-18 | T-13, T-21 |
| AC-19 | T-16, T-17, T-22 |
| AC-20 | T-13, T-20 |
| AC-21 | T-15, T-21 |
| AC-22 | T-8 |
| AC-23 | T-12, T-19 |
| AC-24 | T-11, T-24 |
| AC-25 | T-11, T-14, T-24 |
| AC-26 | T-3, T-5, T-18, T-23 |
| AC-27 | T-10 |
| AC-N1 | T-6, T-9, T-16, T-22 |
| AC-N2 | T-4, T-5 |
| AC-N3 | T-13, T-18 |
| AC-N4 | T-1 |
| AC-N5 | T-6 |

Every criterion is advanced by ≥ 1 task; every task traces to ≥ 1 criterion.

## Notes for the build agent

- **Test injection for hermetic tests.** The Content Renderer, Editor Launcher, and Host
  Adapter take their external commands/spawner as injected parameters (a `Spawner`/command-path
  seam), so unit/integration tests substitute `printf`/`cat`/recording scripts and never depend
  on glow/delta/bat or a live herdr being installed. The e2e tests (T-21–T-23) run the real
  binary over a pty (`expectrl`) but still stub the editor via `EDITOR` and run in temp dirs.
- **Green between tasks.** After each task `cargo test` passes and the crate builds. Add each
  module's `pub mod` to `src/lib.rs` in the task that creates it.
- **Constitution check.** No task mutates files or git state (Git Service exposes only read-only
  subcommands; Tree/Render read only; Editor Launcher hands off). Rendering is delegated (T-11).
  Git is woven through Tree/Policy/Render. All interaction is keyboard (T-13). Session-only
  state. No task gold-plates — each maps to a criterion.
- **Flagged from techstack, to pin during build (not blockers):** confirm the resolved
  `ratatui-core` version and the ratatui↔crossterm backend pairing on first `cargo build`.
  (The herdr CLI/manifest surface is now RESOLVED and pinned in techstack — T-1 manifest
  `[[panes]] placement="split"` and T-17 `pane split`→`pane run`.)
- **Enabling content:** T-1 (scaffold/manifest) and T-24 (README) are infrastructure/packaging;
  both are traced to criteria above rather than left as orphans.
