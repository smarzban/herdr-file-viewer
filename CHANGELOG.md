# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Ambient mouse text selection in the content pane.** Click-drag over the content pane during
  normal navigation — no mode to enter — to select text character-by-character (press at the start,
  drag to the end while the pane auto-scrolls past an edge, release). Releasing the drag copies the
  selected text to the clipboard automatically ("Copied selection"), and the highlight stays for
  feedback; `Esc`, a click elsewhere, or switching files clears it. Line-select mode (`L`) remains a
  separate keyboard-driven mode. `Shift`+drag is left untouched so the terminal's own native
  selection still works, and dragging over placeholder text (a directory / empty pane) copies nothing.
- **Mouse text selection in line-select mode.** Click-drag in the content pane to select text
  character-by-character (press at the start, drag to the end — the pane auto-scrolls past an
  edge — release), with the selected characters highlighted as you drag. `Shift`+mouse is left
  untouched so the terminal's own native selection still works.
- **Line-select mode copies content with `y`/`Y`.** One selection, two products: `Enter` keeps
  copying the `path:line` / `path:start-end` **reference** (unchanged since 1.9.0), while the new
  `y`/`Y` copy the selected **content** — for a keyboard selection, the line(s) joined by
  newlines; for a mouse selection, the exact character span. Whole-line copies come from the
  file's own retained text, byte-faithful (real tabs, no renderer decoration); the syntax view's
  line-number gutter (`bat --style=numbers`) never reaches the copy, indentation is preserved,
  and residual control bytes are removed before it reaches the clipboard (tabs kept). The confirmation
  notice names what was copied (e.g. "Copied lines 42-58" / "Copied selection").

### Changed
- **In line-select mode, a mouse press now places the selection caret** (previously a click placed
  the line marker, and a double-click copied — copying is now always an explicit `Enter`/`y`).

## [1.10.0] - 2026-07-07

### Added
- **Reveal in file manager / open with default app** — new `O` (open the selected file or directory
  with the OS default application) and `R` (reveal the selected entry in the OS file manager) keyboard
  shortcuts, each a read-only, non-blocking hand-off to the host OS (macOS `open`/`open -R`, Linux
  `xdg-open`, Windows `explorer`). Requested in #68.

## [1.9.0] - 2026-07-06

### Added
- **Copy a line reference — `L` line-select mode.** With the content pane focused (or zoomed),
  `L` enters line-select mode: a marker appears on the top visible source line. `j`/`k` (or
  `↑`/`↓`) move the marker and hold `Shift` (`J`/`K`, or Shift+`↑`/`↓`) to extend a selection;
  `Enter` — or a double-click — copies the repo-relative reference (`src/app.rs:42` for one line,
  `src/app.rs:42-58` for a range) to the clipboard over OSC 52, the same path as `y`/`Y`; `Esc`
  exits. A mouse click places the marker and a double-click copies. Entering
  from a rendered-markdown or diff view first switches that file to the line-numbered source view,
  since line numbers only map there. `L` stays focus-gated: with the tree focused it still does the
  tree's horizontal scroll (unchanged). Correct under the `w` wrap toggle — the marker, mouse click,
  and copied reference map through the wrapped layout, so they land on the right source line.

## [1.8.0] - 2026-07-05

### Added
- **Native Windows support (preview).** The viewer now builds and runs on `x86_64-pc-windows-msvc`,
  alongside Linux and macOS. Install via herdr's preview channel and summon it with
  `herdr plugin action invoke open-file-viewer-windows`; see the README's Windows section.
  Preview is best-effort: no Windows-on-ARM and no code-signing yet.

## [1.7.0] - 2026-06-30

### Added
- **Tree horizontal scroll is now keyboard-reachable (AC-18).** The tree's horizontal scroll
  offset — for reading long or deeply-nested rows that overflow the tree column — was
  mouse-only (drag the horizontal scrollbar or a horizontal wheel/swipe); there was no key
  for it. New `H` (Shift+`h`) and `L` (Shift+`l`) intents scroll the tree left/right by the same
  step the mouse wheel uses, clamped to the measured max (mirroring the content pane's `←`/`→`
  h-scroll). Inert unless the tree is focused, so the keys never fight the content pane's own
  horizontal scroll. The lowercase `h`/`l` stay Collapse/Expand — no collision.
- **Discoverability: a `? help` hint on the content pane's bottom border.** A new user no
  longer has to guess that `?` opens the help overlay — a right-aligned `? help` segment now
  rides the content block's bottom border, visible on the default screen without opening any
  modal. One short segment; it shares the border row (not the layout), so it never crowds the
  content or steals a row. Sanitized + clipped like the other border titles (AC-27).
- **Empty-state guidance for blank panes.** Selecting a directory now shows
  `Directory — select a file to view` in the content pane (instead of a blank void), and an
  empty / zero-match tree (no files, or a filter — changed-only / gitignore / hidden — that
  matched nothing) shows `No files`. The copy flows through the normal content path.

### Changed
- **Non-color cues alongside color-only signalling.** Several key UI states were
  conveyed by color alone, so a colorblind user or a non-default terminal theme could lose the
  signal entirely. Each now carries a non-color cue too:
  - **Dirty directory** — the tree's `▾ dir` row for a directory containing a git change now shows
    a leading `●` glyph (files already had `M`/`A`/`D`/`?` letters; directories were color-only
    LightRed). The `tree_rows_max_width` calc flows from the same `tree_row` the tree draws, so the
    added glyph column stays aligned with the h-scroll clamp / hit-test.
  - **Active help tab** — the active section in the help overlay now carries a leading `▶ ` marker
    alongside the existing REVERSED+BOLD indicator (the marker is counted in `help_tab_rects` so the
    click hit-test still tracks the drawn tab).
  - **Current worktree** — the picker's current-worktree row now carries a trailing `(current)`
    text label alongside the existing cyan `●` glyph.
  - **Current search match** — `CURRENT_HIGHLIGHT` is now `REVERSED|BOLD` (theme-relative: it
    inverts whatever the terminal palette is) instead of hardcoded `Black`-on-`Yellow`, so the
    active match is distinguishable with color stripped; the non-current matches keep their
    black-on-cyan `HIGHLIGHT`.
  - **Update banner / bottom prompt** — these status bars now use `REVERSED` (theme-relative)
    instead of hardcoded `Black`-on-`Cyan` / `Black`-on-`Gray`, so they read on any palette.
  All labels still flow through `sanitize_label` (AC-27).
- **Renderer fallback notices no longer leak raw OS errors.** When an external renderer
  (glow/delta/bat) is missing, times out, or otherwise fails, the viewer's fallback notice now
  reports a short, actionable message — naming the missing binary and pointing to
  `docs/renderers.md` for the not-found case — instead of the raw OS error string (e.g.
  `No such file or directory (os error 2)`, which told you nothing you could act on). AC-24/AC-25
  plain-text-plus-notice fallback is unchanged; the raw detail is retained behind a future
  debug/verbose path, not the default notice.
- **Editor hand-off now distinguishes a launch failure from a non-zero editor exit.** Previously
  any error from `open_in_editor` surfaced as `"Could not open editor: …"`, so a successful
  launch that exited non-zero (e.g. a vim exit code) was misleadingly reported as a launch
  failure. The `EditorHandoff` return is now an `EditorOutcome` enum — `NotLaunched(reason)` for
  a process that never started (e.g. missing binary, no `$EDITOR`) vs `NonZeroExit(detail)` for a
  process that ran and returned a failing status — and the controller words each case correctly:
  a launch failure still says `"Could not open editor: {reason}"`, while a non-zero exit says
  `"Editor exited with {detail}"` and still refreshes git state and forces a full repaint (the
  editor did take the terminal). A new `SpawnError` enum at the `Spawner` boundary keeps
  `LiveEditor`/`ProcessSpawner` the only place that knows about `std::process`.
- Extracted the duplicated `wait_bounded` subprocess reaper (child wait + poll + timeout-kill)
  from `render.rs` and `update/mod.rs` into one shared `src/proc.rs` helper. Pure dedup — no
  behavior change; the total wall-clock timeout bound is unchanged (the audit's
  highest-agreement finding, security-adjacent).
- Documented `git` as a runtime requirement in `docs/install.md` (the git-aware tree + diff
  views shell out to the system `git` CLI; without it those features degrade but the viewer
  still opens). Also corrected the `HERDR_FILE_VIEWER_NO_UPDATE_CHECK` wording: any value (the
  var's mere presence) disables the check, not just `=1`.
- `docs/renderers.md`: documented the bundled `assets/markdown-style.json` palette glow is
  pointed at when present (falling back to glow's built-in `dark` style), and corrected the
  overclaimed `cargo` fallback — only `delta` and `bat` are cargo-installable; `glow` is Go
  and the helper prints its manual install link instead.
- `ARCHITECTURE.md`: noted the one on-disk exception to "ephemeral state only" — the advisory
  `update-check.json` timestamp cache (safe to delete); added the new `proc` module to the
  component table.
- `herdr-plugin.toml`: corrected the pane comment (the `[[actions]]` do summon the viewer at
  runtime via launcher scripts — the old comment claimed no runtime command did).
- `.github/workflows/release.yml`: corrected the stale prebuilt-gate comment — the install
  step selects the prebuilt by **declared version match**, not commit exactness; the published
  `COMMIT` marker is informational only (used to note when the checkout is ahead of the
  released binary).
- Swept `src/**` and `tests/**` comments clean of internal build-process references (issue
  IDs, plan task IDs, and review notes) and corrected the stale "search keystrokes are no-ops"
  comment in `controller.rs` (in-file search is fully implemented). No code behavior changed.
- `CHANGELOG.md`: added the missing `[1.1.0]`–`[1.6.0]` release-tag link references (only
  `[1.0.0]` had one).
- **Test timing hardening:** added a `perf` cargo feature to gate the
  absolute-stopwatch perf-budget tests (`render_perf`, `tree_perf`, the `reroot` AC-17 budget)
  off the default PR lane — a plain `cargo test` no longer runs them (they flake on a loaded
  shared runner for reasons unrelated to a regression); run via `cargo test --features perf`.
  Rewrote `search_perf` and `index_perf` as **relative-scaling** asserts (`time(2N) < ~4×
  time(N)`, with a minimum-base floor below which a small absolute bound applies, modelled on
  the `render.rs` `mul_f32(1.5)` exemplar) so they catch an O(n²) regression without flaking on
  a 2–3× slower machine — these run on the default lane. Replaced
  the pty e2e tests' fixed `thread::sleep` "screen is ready" assumptions with `expectrl`
  wait-for-content (`expect` on the prompt/overlay label the next key depends on), eliminating
  the torn-read flake class; the deliberate Esc inter-byte gaps and terminal-resume settles are
  kept (they prevent Alt+char decoding and have no screen-content anchor). The 2 macOS
  `#[ignore]` e2e tests (`e2e_help`, `e2e_editor`) are retained with their existing rationale —
  they may now pass on macOS CI after the timing fix, but that can only be confirmed on the
  macOS CI matrix, so the ignores stay until verified.
- **Test coverage:** strengthened `no_handled_intent_mutates_the_filesystem`
  so it routes every `Intent::ALL` variant through the real handler (closing any modal an intent
  opened before the next iteration, so guards don't short-circuit the dispatch) and asserts a
  content-aware FS/git snapshot — the read-only invariant (AC-N1/N2) is now genuinely exercised.
  Added a modal × intent cross-product guard matrix (5 modal states × every `Intent::ALL`
  variant) driving off `Intent::ALL` so a new intent variant is auto-covered — asserts modal isolation
  (AC-5/AC-6), no second modal opens, tree/FS unchanged. Extracted the git porcelain/diff parser
  into testable helpers (`parse_porcelain_status`, `parse_name_status`) and added table-driven
  unit tests for malformed/truncated input, rename/copy edge cases, unknown status codes, and
  direct `classify`/`classify_name_status` per-code assertions — the defensive branches that were
  previously unreachable are now exercised. Added an OSC-52 clipboard-exfiltration ingestion test
  (AC-27 named vector) on the content-renderer path, and gated the CLI smoke test's network path
  by setting `HERDR_FILE_VIEWER_NO_UPDATE_CHECK` so it performs no network I/O (hermetic).

### Fixed
- **Content pane no longer shows a stale file under a new title while a render is in flight.**
  On a slow off-thread render, the content pane used to keep displaying the PREVIOUS file's body
  while the content title (derived from the live tree cursor) already named the NEW selection —
  the pane briefly misrepresented what was selected. The content title is now derived from the
  displayed content's file (`content_path`, updated only when the render result lands in `poll`),
  so the title and body switch to the new file together; while a render is in flight the body
  shows a `Rendering…` loading placeholder (and the title stays on the previously-displayed file,
  or a neutral `Content` label at launch / after a re-root when no content has landed yet). The
  existing `latest_seq`/`applied_seq` supersession already keyed stale-result dropping, so a
  superseded render result (the user moved on) still does not overwrite the pane.

## [1.6.0] - 2026-06-28

### Added
- **Go to line (`:`).** Press `:` to open a prompt and jump the content pane to a source line by
  number — `Enter` jumps (out-of-range clamps to the last line), `Esc` cancels. Works in every view:
  in a rendered-markdown or diff view (where a source line has no 1:1 display row) confirming switches
  the file to the line-numbered content view and jumps there. Read-only navigation. (The first half of
  in-file navigation.) ([#54](https://github.com/smarzban/herdr-file-viewer/pull/54))
- **Search in file (`/`, `n`/`N`).** Press `/` to search the open file's content: every match
  highlights as you type and the content scrolls to the first match; `Enter` commits the search
  (highlights persist) and `n` / `N` cycle through matches in document order, wrapping at the ends
  with a notice. Matching is **smartcase** — a lowercase query is case-insensitive, a query with any
  uppercase letter is case-sensitive — and **literal** (regex metacharacters match literally). Search
  works in **every** view (code, rendered markdown, or diff), over the content **as displayed**; `Esc`
  cancels and restores the scroll, and the search clears when the displayed content changes.
  Read-only navigation. (The second half of in-file navigation.) ([#55](https://github.com/smarzban/herdr-file-viewer/pull/55))
- **Go to file (`f`).** Open a fuzzy finder over every file in the tree and jump straight to one by
  name — type to filter, `↑` / `↓` to move, `Enter` to open, `Esc` to cancel; `←` / `→` (or the
  horizontal wheel) scroll long result rows, and the result list has a draggable scrollbar.
  Read-only navigation; it never modifies a file. ([#51](https://github.com/smarzban/herdr-file-viewer/pull/51))
- **Help overlay (`?`).** Press `?` (Shift+`/`) to open a view-only overlay with two sections:
  **What's New** — the changelog rendered as markdown so you can read release notes without leaving
  the viewer — and **About** — version, repository URL, license, and update status. Navigate with
  `↑` / `↓` (or the mouse wheel); `Esc` or `q` closes the overlay and returns to where you were.
  Read-only; no files are modified. ([#56](https://github.com/smarzban/herdr-file-viewer/pull/56))
- **The tree names its root and branch.** The tree column's top border shows the root directory's
  name and its bottom border the current git branch (omitted outside a git repo / on a detached
  HEAD), with long names middle-ellipsized to fit — so you can always see *which* directory and
  branch you're viewing. ([#52](https://github.com/smarzban/herdr-file-viewer/pull/52))

### Changed
- **Installing now reuses the latest released binary even when `main` is ahead of the tag.** The
  install step (`scripts/fetch-or-build.sh`) matches the prebuilt by **version** rather than by exact
  commit, so landing a PR no longer forces new users to compile while a release is pending — they get
  the last released, SHA-256-verified binary instead. A version with no published release still falls
  back to building from source, and when the checkout is ahead of the release it's installing, the
  install prints a note saying the binary doesn't yet include the unreleased source.
  ([#50](https://github.com/smarzban/herdr-file-viewer/pull/50))

### Fixed
- **The worktree picker's `←` now responds immediately after over-scrolling right.** The picker's
  horizontal scroll offset is clamped to the measured maximum each frame (mirroring the file
  finder), so it can't park past the widest row and swallow the first few `←` presses. ([#52](https://github.com/smarzban/herdr-file-viewer/pull/52))

## [1.5.0] - 2026-06-25

### Added
- **Scrollbars** appear on the tree and content panes whenever there's more to see than fits — a
  vertical bar when the list or file is taller than the pane, and a horizontal bar when a row /
  unwrapped line is wider than the pane. They show only where there is something to scroll. The
  bars render **inside** the pane (one cell off the text) and are **draggable with the mouse**:
  drag a vertical bar ↕ or a horizontal bar ↔ to scroll, and pressing the track jumps to that
  position. Dragging the tree's vertical bar scrubs the selection through the file list.
- **The tree scrolls horizontally** so a long or deeply-nested file name can be read in full — via
  the horizontal mouse wheel or by dragging the tree's horizontal scrollbar (the `←`/`→` keys stay
  expand/collapse in the tree).
- **Hide hidden files (`.`).** A toggle that drops dot-prefixed files and folders from the tree —
  handy when you open a directory (like `$HOME`) that's flooded with them. It's independent of the
  gitignore toggle (`i`) and off by default, so `.gitignore` / `.github` stay visible until you ask
  to hide them. ([#46](https://github.com/smarzban/herdr-file-viewer/issues/46))

### Fixed
- **The tree now scrolls to follow the selection.** On a long file list the tree stayed pinned to
  the top, so moving the cursor past the last visible row selected files you couldn't see. The tree
  now scrolls to keep the selected row in view (mouse clicks still map to the right row when
  scrolled). ([#45](https://github.com/smarzban/herdr-file-viewer/issues/45))

## [1.4.0] - 2026-06-25

### Added
- **Switch worktree (`W`).** Press `W` to open a picker of the repository's git worktrees and
  select one to re-root the viewer in place — the tree, git status, and content pane all rebuild
  around the chosen worktree without relaunching the plugin. Your view preferences carry over and
  navigation resets to the new root. It is purely a change of *where you're looking*: read-only, no
  branch is checked out, and nothing on disk is modified.
- The picker marks the worktree you're currently viewing and pre-selects the one where a herdr
  agent is active, so you can jump straight to what an agent is working on.
- Each picker row shows the worktree's branch (or a detached-HEAD marker) and, when herdr reports
  it, the live status of the agent running there.

### Changed
- All notices shown in the viewer's notice strip are now stripped of control characters at the
  render sink (previously only the copied-path confirmation was), so any filesystem-derived string
  surfaced in a notice — such as a worktree path — cannot emit a terminal escape or paste-inject.

## [1.3.0] - 2026-06-24

### Added
- **Copy a file's path to the clipboard.** `y` copies the selected file's repo-relative path
  (e.g. `src/app.rs`); `Y` copies its absolute path. The copy uses the terminal's OSC 52
  clipboard escape, so it travels through herdr and SSH to your real clipboard with no extra
  tooling, and a confirmation shows in the notices strip. Read-only — like every other key, it
  never touches the file's contents.

### Security
- The copied path and its confirmation notice are stripped of control characters, so a
  maliciously-named file (e.g. one with an embedded newline or escape byte) can't paste-inject
  into a shell or emit a terminal escape when its path is copied — consistent with how the viewer
  already sanitizes other filesystem-derived strings it displays.

## [1.2.2] - 2026-06-23

### Docs
- Slimmed the README to the essentials (pitch, quick start, keys, links) and moved the longer
  guides into dedicated files: `docs/install.md` (install & updating), `docs/renderers.md`
  (the optional `glow`/`delta`/`bat` integrations), and `docs/usage.md` (summoning & keybindings).
- Added a **Roadmap** section (in-app help overlay, settings, go-to-file) and an invitation to
  open issues for bugs and feature requests.
- Documented `$EDITOR` setup for the `e` key: the editor is read from the herdr **server's**
  environment, so export it in the right shell startup file and restart the server
  (`herdr server stop` + relaunch) — `reload-config` and quitting the client are not enough.
- Added a rendered-markdown screenshot to the README; trimmed `SECURITY.md` to the GitHub private
  advisory channel.

This is a docs-only release; the binary is unchanged from 1.2.1 in behavior — it is re-tagged so a
normal `herdr plugin install` uses the prebuilt fast path again instead of building from source.

## [1.2.1] - 2026-06-23

### Fixed
- Prebuilt install path now works for a normal `herdr plugin install`. v1.2.0 gated the prebuilt on
  a local `v<version>` tag ref, but herdr's install checkout clones the commit *without* tags, so the
  gate always fell back to a source build (failing when Rust was absent). The gate now compares the
  checkout's `HEAD` to a `COMMIT` marker published in the release — so the prebuilt is used whenever
  the source is exactly the released commit, while a `main` ahead of the tag still builds from source.

## [1.2.0] - 2026-06-23

### Added
- Prebuilt-binary install path: tagged releases now ship SHA-256-verified binaries for macOS
  (arm64 + x86_64) and Linux x86_64 (static/musl). The install step downloads the binary matching
  the source's version + platform and falls back to a `cargo` source build on any miss, so no Rust
  toolchain is needed for supported platforms. The install command is unchanged.

## [1.1.0] - 2026-06-22

### Added
- **Update-available notification** — the viewer checks for a newer release (at most once per
  day, off the UI thread, over a read-only `git ls-remote`) and, when you're behind, shows a
  dismissable bottom status-line banner with the one-line update command. Press `u` to dismiss
  it for the session. Opt out entirely with `HERDR_FILE_VIEWER_NO_UPDATE_CHECK=1`. No new
  dependencies, no telemetry.

### Docs
- Clarified updating: re-running `herdr plugin install …` pulls the latest; `--ref` only pins a
  specific version and is no longer presented as part of the normal install.

## [1.0.0] - 2026-06-22

First public release: a git-aware, read-only file viewer that runs as a herdr plugin pane.

### Added
- **Tree, scoped to your work** — rooted at the git worktree top-level (else the launch
  directory), honoring `.gitignore` with a toggle to reveal ignored files.
- **Git woven in** — per-file status markers (`M`/`A`/`D`/`?`) with color, a changed-files-only
  filter, and a baseline you can toggle between your branch's merge-base and `HEAD`.
- **The right view per file** — a changed file shows its diff; markdown renders; everything else
  is syntax-highlighted with line numbers. Cycle the view (`v`), including a **full-file diff**
  (whole file + line numbers + inline change).
- **Navigable content** — scroll all four directions, toggle line wrapping (`w`), resize the
  split (`<` / `>`), and **zoom** (`z`) to hide the tree for a full-screen read.
- **Activate** (`Enter` / double-click) — expand a folder, or open a file in zoom mode.
- **Open in `$EDITOR`** (`e`) — a read-only hand-off; the viewer never edits the file itself.
- **Keyboard-first**, with additive mouse support (click, double-click, wheel, divider drag).
- **Two ways to summon it** — a split-pane action and an idempotent tab action
  (open-or-switch-or-toggle).
- **Delegated rendering** to `glow` / `delta` / `bat`, each optional with graceful plain-text
  fallback and a notice when a renderer is absent.
- **Refresh** (`r`) and automatic git re-read on focus-gain, so external changes (a merge, pull,
  or commit elsewhere) show up.

### Security
- Read-only by construction; untrusted file content is escape-neutralized and fed to renderers
  on stdin; every `git` invocation is hardened for untrusted repositories. See
  [SECURITY.md](SECURITY.md).

[1.0.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.0.0
[1.1.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.1.0
[1.2.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.2.0
[1.2.1]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.2.1
[1.2.2]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.2.2
[1.3.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.3.0
[1.4.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.4.0
[1.5.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.5.0
[1.6.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.6.0
[1.7.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.7.0
