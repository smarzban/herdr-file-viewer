# Acceptance criteria: herdr-file-viewer

- Status: drafted (acceptance-criteria stage) — 2026-06-17
- Amended: 2026-06-18 — AC-17 oracle → manual; AC-23 reworded to responsiveness
  (resolving readiness-check findings H-1/H-2).
- Input: `specs/file-viewer/brief.md`
- Next stage: architecture-design

Each criterion is atomic, observable, falsifiable, quantified where it claims a degree,
and in scope. Verification type is one of **test-backed** (with kind-of-oracle) or
**reviewer-checked** (with axis + pass/fail question + justification). IDs are stable
handles; do not renumber.

> Note on `HOW`: criteria state what must be true. Mechanisms (base-branch detection,
> lazy loading, which renderer, manifest schema) belong to design/techstack, not here.

## Tree & navigation

- **AC-1** — When the viewer is launched with its working directory inside a git
  worktree, the tree root is that worktree's root directory.
  *(test-backed: integration)*
- **AC-2** — When launched outside any git worktree, the tree root is the invoking
  pane's working directory.
  *(test-backed: integration)*
- **AC-3** — Directories appear as nodes that expand and collapse, revealing their
  nested entries on expansion (the tree is recursive).
  *(test-backed: integration)*
- **AC-4** — By default, files matched by the repository's ignore rules are absent
  from the tree.
  *(test-backed: integration)*
- **AC-5** — A toggle reveals ignored/all files in the tree; toggling again restores
  the default filtered view.
  *(test-backed: integration)*
- **AC-6** — A toggle restricts the tree to only files reported as changed against the
  active diff baseline; toggling again restores the full tree.
  *(test-backed: integration)*
- **AC-7** — Each file's git-status indicator in the tree (modified / added / deleted /
  untracked) matches git's reported status for that file against the active baseline.
  *(test-backed: integration)*

## Content & rendering

- **AC-8** — Selecting an unchanged markdown file renders it as formatted markdown
  (not raw source) in the content pane by default.
  *(test-backed: integration)*
- **AC-9** — Selecting a file that is changed against the active baseline shows its
  diff in the content pane by default — including when the file is markdown (changed
  status takes precedence over markdown rendering).
  *(test-backed: integration)*
- **AC-10** — Selecting a file that is neither changed nor markdown shows
  syntax-highlighted content in the content pane by default.
  *(test-backed: integration)*
- **AC-11** — A dedicated key cycles the content pane among the view modes applicable
  to the selected file (raw content / rendered / diff), overriding the auto-selected
  default.
  *(test-backed: integration)*
- **AC-12** — Selecting a binary file shows a placeholder identifying it as binary and
  does not emit its raw bytes into the pane.
  *(test-backed: integration)*
- **AC-13** — Selecting a text file at or above the size cap (≥ 1 MB or ≥ 5,000 lines)
  renders only a bounded preview and displays a visible truncation notice.
  *(test-backed: integration)*

## Git / diff baseline

- **AC-14** — When the current checkout is a feature branch / worktree (a branch other
  than the repository's base/default branch), the default diff baseline is the base
  branch, so committed work is included in "changed" and in diffs.
  *(test-backed: integration)*
- **AC-15** — When the current checkout is the base/default branch, the default diff
  baseline is HEAD (uncommitted changes only).
  *(test-backed: integration)*
- **AC-16** — A toggle switches the active diff baseline between base-branch and HEAD,
  and the tree's changed set plus the content pane's diff update to reflect the new
  baseline.
  *(test-backed: integration)*

## herdr integration & interaction

- **AC-17** — Invoking the plugin's declared herdr action via its keybinding opens the
  viewer in a split pane within the current workspace.
  *(test-backed: manual — link/run the plugin in herdr, invoke the action, confirm a split
  pane opens. The static `placement = "split"` manifest declaration that produces this is
  additionally covered by the automated manifest test. Justification: live herdr
  action→pane behavior needs a running herdr instance not guaranteed in CI, so the
  automatable part is tested and the live launch is a manual check.)*
- **AC-18** — Every viewer function (navigation, expand/collapse, all toggles, mode
  cycle, open-in-editor, close) is invocable from the keyboard; no function requires a
  mouse.
  *(test-backed: e2e)*
- **AC-19** — Invoking open-in-editor on the selected file hands that exact file to an
  editor outside the viewer (the user's configured editor or a new herdr pane/tab).
  *(test-backed: integration)*
- **AC-20** — A dedicated key closes the viewer and returns control to the prior
  pane/workspace.
  *(test-backed: integration)*
- **AC-21** — When the viewer's pane is narrower than 80 columns, a focus-toggle gives
  the full pane width to either the tree or the content pane (the inactive column is
  hidden), keeping the active column fully readable.
  *(test-backed: integration)*

## Performance

- **AC-22** — Launched on a repository of up to 10,000 tracked files, the tree becomes
  interactive (navigable) within 1 second.
  *(test-backed: integration / perf)*
- **AC-23** — Selecting a file or switching its view keeps the UI responsive to input
  within 300 ms; the fully rendered content may arrive asynchronously and must never block
  input or navigation.
  *(test-backed: integration / perf)*

## Robustness & degradation

- **AC-24** — If a delegated renderer is unavailable, the content pane still shows the
  file as plain unstyled text (no crash, no empty pane).
  *(test-backed: integration)*
- **AC-25** — When a renderer fallback occurs, the viewer displays a non-fatal notice
  identifying the missing capability.
  *(test-backed: integration)*
- **AC-26** — When the root is not inside a git repository, the viewer operates as a
  plain file browser and git-dependent features (status markers, diff view,
  changed-only filter, baseline toggle) are inactive rather than producing errors.
  *(test-backed: integration)*

## Security

- **AC-27** — Control/escape sequences present in displayed file content are
  neutralized: showing any file cannot move the cursor, clear the screen, or otherwise
  alter the terminal outside the viewer's own drawing. (Styling-only sequences from a
  trusted renderer may be permitted; cursor/screen-control sequences are stripped
  regardless of source.)
  *(test-backed: integration)*

## Negative criteria (out-of-bounds — non-goals as checks)

- **AC-N1** — No viewer action creates, renames, moves, or deletes any file or
  directory; the filesystem under the root is unchanged after exercising every action.
  *(test-backed: integration)*
- **AC-N2** — No viewer action stages, unstages, commits, discards, or changes
  branches; git state is unchanged after exercising every action.
  *(test-backed: integration)*
- **AC-N3** — The viewer offers no in-pane editing of file contents.
  *(reviewer-checked — axis: Spec Conformance. Pass/fail: "Is there any keybinding or
  code path that lets a user modify and persist a file's contents from within the
  viewer?" Must be No. Justification: proving the absence of a capability is a
  conformance judgment over the action set, not a positive behavioral test.)*
- **AC-N4** — The viewer never auto-launches; it appears only in response to an
  explicit user action/keybinding, with no event-hook or automatic invocation.
  *(reviewer-checked — axis: Spec Conformance. Pass/fail: "Does the plugin declare any
  event hook or any auto-invocation path that can open the viewer without an explicit
  user action?" Must be No. Justification: this is determined by what the manifest
  declares — a static conformance check, not a runtime assertion.)*
- **AC-N5** — The viewer does not navigate above its root directory; attempting to do
  so is a no-op.
  *(test-backed: integration)*

## Verification map

| Criterion | Verification |
| --- | --- |
| AC-1 | test-backed: integration |
| AC-2 | test-backed: integration |
| AC-3 | test-backed: integration |
| AC-4 | test-backed: integration |
| AC-5 | test-backed: integration |
| AC-6 | test-backed: integration |
| AC-7 | test-backed: integration |
| AC-8 | test-backed: integration |
| AC-9 | test-backed: integration |
| AC-10 | test-backed: integration |
| AC-11 | test-backed: integration |
| AC-12 | test-backed: integration |
| AC-13 | test-backed: integration |
| AC-14 | test-backed: integration |
| AC-15 | test-backed: integration |
| AC-16 | test-backed: integration |
| AC-17 | test-backed: manual (+ automated manifest static check) |
| AC-18 | test-backed: e2e |
| AC-19 | test-backed: integration |
| AC-20 | test-backed: integration |
| AC-21 | test-backed: integration |
| AC-22 | test-backed: integration / perf |
| AC-23 | test-backed: integration / perf |
| AC-24 | test-backed: integration |
| AC-25 | test-backed: integration |
| AC-26 | test-backed: integration |
| AC-27 | test-backed: integration |
| AC-N1 | test-backed: integration |
| AC-N2 | test-backed: integration |
| AC-N3 | reviewer-checked: Spec Conformance |
| AC-N4 | reviewer-checked: Spec Conformance |
| AC-N5 | test-backed: integration |

## Deferred

- None. (Mechanisms such as base-branch detection, large-tree loading strategy, and
  the choice of render tools are intentionally out of scope here — they belong to the
  design and techstack stages, not deferred criteria.)

## Glossary terms touched

Sharpened/added and mirrored to `/CONTEXT.md`: size cap, truncation notice, renderer
fallback, focus-toggle. Existing terms used as-is: viewer, tree, content pane, view
mode, diff baseline, base branch, changed-only filter, root.
