# Design: herdr-file-viewer

- Status: drafted (architecture-design stage) — 2026-06-17
- Inputs: `specs/file-viewer/brief.md`, `specs/file-viewer/acceptance-criteria.md`
- Next stage: techstack-selection

Logical shape only — the *kind* of each component, not the product. Single-plugin repo,
so this design is also the project architecture (no separate `architecture.md`).

## Components

Each component has one responsibility (one reason to change) and an explicit contract.

### Host Adapter
- **Responsibility:** mediate all interaction with herdr.
- **Kind:** integration boundary shim + static plugin manifest.
- **Inputs:** herdr launch invocation with injected context (cwd, worktree info, base-
  branch hint, plugin dirs); requests from the Controller/Editor Launcher to open a
  new pane.
- **Outputs:** a normalized `LaunchContext { cwd, worktreeRoot?, baseBranch?, isWorktree }`
  to the Root Resolver; pane-open requests issued to herdr (file path + placement).
- **Errors:** malformed/missing context → emit a minimal `{ cwd }` context, never
  crash. Manifest statically declares one action (keybinding, placement = split) and
  **no event hooks** (AC-N4).

### Root Resolver
- **Responsibility:** resolve the tree root and git-presence.
- **Kind:** pure resolver (stateless).
- **Inputs:** `LaunchContext`.
- **Outputs:** `{ root, isGitRepo, repoRoot?, isWorktree, baseBranch? }` — root is the
  worktree root inside a worktree (AC-1) else the cwd (AC-2).
- **Errors:** unreadable cwd → surfaces an init error to the Controller; not-a-repo is
  a normal result (`isGitRepo:false`), not an error (AC-26).

### Tree Model
- **Responsibility:** represent the browsable, rooted file tree and the current
  position within it.
- **Kind:** in-memory hierarchical model with lazy enumeration.
- **Inputs:** `root`; ignore-filter flag; changed-only flag + changed-set (from Git
  Service); expand/collapse and cursor intents.
- **Outputs:** ordered list of visible nodes `{ path, type, depth, expanded,
  statusMarker }` and the current selection.
- **Errors:** unreadable subdir → node marked empty/error, traversal continues; never
  resolves a path outside `root` (AC-N5). Reads only; never writes (AC-N1).
- **Notes:** enumerates lazily/on-expand to meet AC-22; hides ignored entries by
  default (AC-4), reveals on toggle (AC-5), restricts to changed-set on toggle (AC-6).

### Git Service
- **Responsibility:** answer read-only git questions.
- **Kind:** read-only query service over a VCS repository.
- **Inputs:** `repoRoot`; baseline mode (`base` | `HEAD`); file path (for status/diff).
- **Outputs:** effective baseline + default-mode decision (feature branch/worktree →
  `base`, AC-14; base/default branch → `HEAD`, AC-15); changed-set vs baseline; per-
  file status (AC-7); raw diff text for a file (AC-9).
- **Errors:** not-a-repo or git failure → reports "git unavailable" so the Controller
  degrades (AC-26). Issues **only read-only operations** (AC-N2).

### View Policy
- **Responsibility:** choose the default content view mode for a file.
- **Kind:** pure decision function.
- **Inputs:** `{ path, isMarkdown, isChanged }` + optional user override.
- **Outputs:** chosen mode ∈ `{ rendered-markdown, diff, syntax-content, raw-content }`
  by precedence — changed → diff (incl. markdown, AC-9); else markdown → rendered
  (AC-8); else → syntax-content (AC-10) — plus the applicable set for cycling (AC-11).
- **Errors:** unknown type → `syntax-content`/`raw-content`. No I/O.

### Content Renderer
- **Responsibility:** produce the content-pane text for `(file, mode)` via delegated
  rendering, with safety guards.
- **Kind:** delegating transformer (invokes external render processes) + guards.
- **Inputs:** `{ path, mode, rawDiff? }` (raw diff supplied by Git Service for diff mode).
- **Outputs:** a bounded, sanitized text block + flags `{ truncated?, fellBack?,
  binary? }` and an optional notice string.
- **Errors / guards:** binary → placeholder, no raw bytes (AC-12); size ≥ cap →
  bounded preview + truncated flag (AC-13); renderer missing/failed → plain-text
  fallback + `fellBack` notice (AC-24, AC-25); neutralizes control/escape sequences,
  permitting only a styling safelist and stripping cursor/screen-control regardless of
  source (AC-27); time-bounded per file to meet AC-23. Reads only (AC-N1).

### Presenter
- **Responsibility:** draw the UI.
- **Kind:** terminal view layer.
- **Inputs:** visible nodes + selection, content block + notices, focus state, pane
  width.
- **Outputs:** a drawn frame, clipped to the viewer's region; recursive tree display
  (AC-3), status markers (AC-7), truncation/fallback notices (AC-13, AC-25).
- **Errors:** width < 80 cols → single-column focus mode (AC-21); never emits partial
  garbage; clips all content to the pane region (defense-in-depth for AC-27).

### Input Dispatcher
- **Responsibility:** turn key events into intents.
- **Kind:** event-to-intent mapper.
- **Inputs:** raw key events.
- **Outputs:** intents `{ NavUp, NavDown, Expand, Collapse, ToggleIgnore,
  ToggleChangedOnly, ToggleBaseline, CycleView, OpenInEditor, ToggleFocus, Close }`.
- **Errors:** unmapped key → no-op. Every viewer function has a key (AC-18); **no edit
  intent exists** (AC-N3).

### Session Controller
- **Responsibility:** orchestrate intents into coordinated state changes.
- **Kind:** interaction orchestrator holding ephemeral session state.
- **Inputs:** intents from the Input Dispatcher; outputs of the other components.
- **Outputs:** state updates; coordination calls to Tree Model / Git Service / View
  Policy / Content Renderer / Editor Launcher / Host Adapter; redraw signals to the
  Presenter. Hides git features when `isGitRepo:false` (AC-26).
- **Errors:** any component error is reflected as a non-fatal status/notice; the loop
  never crashes. Exposes no editing path (AC-N3).

### Editor Launcher
- **Responsibility:** hand the selected file to an external editor.
- **Kind:** external hand-off launcher.
- **Inputs:** selected file path; target preference (configured editor vs new herdr
  pane).
- **Outputs:** spawns the editor, or asks the Host Adapter to open a new herdr pane/tab
  for it (AC-19); returns control to the viewer.
- **Errors:** no editor / launch failure → non-fatal notice. Performs hand-off only;
  never edits or writes the file itself (AC-N1).

## Data flow

1. **Launch:** herdr → Host Adapter (context) → Root Resolver → `{ root, isGitRepo, … }`
   → Session Controller initializes the Tree Model (enumerate root) and, if a repo, the
   Git Service (default baseline + changed-set + status).
2. **Navigate:** key → Input Dispatcher → intent → Session Controller → Tree Model
   updates cursor/expansion → Presenter redraws.
3. **Select & preview:** Controller asks View Policy for the default mode → asks Content
   Renderer for the block (diff mode pulls raw diff from Git Service) → Presenter draws
   the content pane.
4. **Toggle baseline / filters:** Controller flips the flag → Git Service recomputes the
   changed-set (baseline) → Tree Model re-filters → Presenter redraws (AC-5, AC-6, AC-16).
5. **Open-in-editor:** Controller → Editor Launcher → external editor or (via Host
   Adapter) a new herdr pane.
6. **Close:** Controller → Presenter teardown → Host Adapter exits the pane (AC-20).

## Key state

- **Session state (in-memory, ephemeral):** root, git-presence, active baseline mode,
  ignore-filter flag, changed-only flag, per-file view-mode overrides, cursor/selection,
  expansion set, focus (tree | content), narrow-split flag.
- **Derived/cached:** changed-set and per-file status from the Git Service; invalidated
  on baseline toggle or explicit refresh.
- **Persistent store:** none in v1. Filesystem and git repo are the read-only source of
  truth. (The plugin's state dir exists but is unused in v1.)

## Trust & failure boundaries

- **Untrusted file bytes → Content Renderer (primary trust boundary).** All file content
  is untrusted: it may be binary (AC-12), oversized (AC-13), or carry hostile escape
  sequences (AC-27). The Content Renderer neutralizes cursor/screen-control sequences and
  bounds size *before* anything reaches the Presenter; the Presenter additionally clips to
  the pane region (defense-in-depth).
- **herdr-injected context → Host Adapter.** Validated/normalized at the boundary;
  malformed input degrades to a minimal `{ cwd }` context.
- **External renderer processes → Content Renderer.** Treated as fallible: missing binary,
  non-zero exit, or timeout all degrade to plain-text + notice (AC-24, AC-25).
- **git → Git Service.** Non-repo or command failure degrades to "git unavailable"; the
  viewer continues as a plain browser (AC-26).
- **Failure principle:** every component fails to a non-fatal notice surfaced by the
  Controller; the interaction loop never crashes on bad input.

## Criterion-to-component map

| Criterion | Component(s) |
| --- | --- |
| AC-1 | Root Resolver |
| AC-2 | Root Resolver |
| AC-3 | Tree Model, Presenter |
| AC-4 | Tree Model |
| AC-5 | Tree Model, Input Dispatcher, Session Controller |
| AC-6 | Tree Model, Git Service |
| AC-7 | Git Service, Presenter |
| AC-8 | View Policy, Content Renderer |
| AC-9 | View Policy, Git Service, Content Renderer |
| AC-10 | View Policy, Content Renderer |
| AC-11 | Input Dispatcher, Session Controller, View Policy |
| AC-12 | Content Renderer |
| AC-13 | Content Renderer, Presenter |
| AC-14 | Git Service |
| AC-15 | Git Service |
| AC-16 | Git Service, Session Controller, Presenter |
| AC-17 | Host Adapter |
| AC-18 | Input Dispatcher |
| AC-19 | Editor Launcher, Host Adapter |
| AC-20 | Input Dispatcher, Session Controller, Host Adapter |
| AC-21 | Presenter |
| AC-22 | Tree Model |
| AC-23 | Content Renderer |
| AC-24 | Content Renderer |
| AC-25 | Content Renderer, Presenter |
| AC-26 | Root Resolver, Git Service, Session Controller |
| AC-27 | Content Renderer |
| AC-N1 | Tree Model, Content Renderer, Editor Launcher (read-only by construction) |
| AC-N2 | Git Service (read-only operations only) |
| AC-N3 | Input Dispatcher, Session Controller (no edit intent/path) |
| AC-N4 | Host Adapter (manifest declares no event hooks) |
| AC-N5 | Tree Model |

## ADRs created

- `docs/adr/0002-single-in-process-tui.md` — one process owns both columns, not herdr-
  native split panes.
- (Carried from idea stage: `docs/adr/0001-hybrid-build-delegate-rendering.md`.)

## Constitution check

No MUST principle is violated: read-only by default (no fs/git mutation; editor is hand-
off only), delegated rendering (Content Renderer), git first-class (Git Service woven
through Tree Model, View Policy, Content Renderer), keyboard-first (Input Dispatcher,
AC-18), good plugin citizen (Host Adapter via documented herdr surface; session-only
state), YAGNI (no persistence; each component is justified by ≥1 criterion).

## Glossary terms touched

No new shared vocabulary beyond `CONTEXT.md`; component names are internal to the design.
