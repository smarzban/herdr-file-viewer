# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Entries are short on purpose; follow the
`→` links for the full detail.

## [1.16.0] - 2026-08-07

### Added
- Two more ways to summon the viewer, alongside the split and tab actions. `open-file-viewer-overlay` opens it as a temporary overlay over the active pane; closing it restores the previous focus and zoom. `open-file-viewer-popup` opens it in a floating, session-modal popup at 90% × 85% of the terminal — big enough to stay two-column. Both are Linux/macOS only in this release. → [summoning](docs/summoning.md)
- The popup is open-only: herdr has no CLI verb that can close one, so exit the viewer to dismiss it (`Esc` is the un-remappable floor; with annotations held, `q` or `y` through the discard confirm). A second press while one is open is refused by herdr with `popup already open`. → [summoning](docs/summoning.md)

### Changed
- Minimum herdr version is now **0.7.4** (was 0.7.0). Session-modal popups for plugins, with cell/percentage sizing, arrived in that release, so it is the honest floor now that the popup layout ships; overlay alone would have needed no bump. → [install](docs/install.md)
- `Z` skips the herdr pane zoom under the new overlay and popup layouts — an overlay already covers the terminal, and a popup has no pane of its own to zoom — so there it is the in-pane full-screen only. Split and tab are unchanged. → [keys](docs/keys.md)

### Fixed
- Agent skill: the launch instructions no longer tell agents to pass `--cwd`. herdr resolves the manifest's relative pane command against it, so the launch failed with `plugin_pane_open_failed` — or worse, inside a built plugin checkout, silently ran that checkout's binary. The skill and the `docs/usage.md` snippet now explain that the viewed root follows the *focused herdr pane's* directory, so an agent's own `cd` does not move it. Thanks @AntonyKor (#139) → [agent skill](skills/herdr-file-viewer/SKILL.md) · [usage](docs/usage.md#teach-your-agent)

## [1.15.0] - 2026-08-03

### Added
- Bundled `herdr-file-viewer` agent skill: agents can resolve a file, source location, or function definition and open it in a fresh Files pane. Includes safe target handling, an explicit user-confirmation prompt for proactive offers, and platform guidance. → [agent skill](skills/herdr-file-viewer/SKILL.md) · [usage](docs/usage.md#teach-your-agent)
- `show_ignored`: show gitignored (and git-excluded) files at startup, exactly as if the `i` key had already been pressed once. `.git/` stays hidden regardless, and the `i` key still toggles the state during the session. Off by default. Thanks @leonfox28 for the suggestion (#119) → [configuration](docs/configuration.md)
- `compact_dirs`: draw a chain of single-child directories as one row (`src/main/java/br/com` instead of six indented rows), in both the full tree and the changed-only view. Off by default; worth turning on when your paths are deeper than your pane is wide. Thanks @Jprnp (#132) → [configuration](docs/configuration.md) · [usage](docs/usage.md#the-tree)
- `]` / `[` jump the tree cursor to the next / previous changed file, wrapping at the ends with a notice — `n`/`N` for the tree instead of arrowing past directory rows. Walks the set the tree is filtered by (working-tree status under `d`, else the baseline-aware set behind `c` / `b`) and expands a collapsed directory to reach a changed file inside it. Thanks @Jprnp (#131) → [usage](docs/usage.md#git-awareness) · [keys](docs/keys.md)
- Page-wise scrolling: `Space` / `PageDown` move down one screenful and `PageUp` moves up one, paging the content pane when it is focused and the tree cursor otherwise. The step is the focused pane's live height, so it follows a resize and stays a screenful in the narrow single-column layout. `Space` follows the pager convention (`less`, `more`, `man`, and so `bat`); both are remappable as `page_down` / `page_up`. Thanks @wasuregusa18 (#133) → [keys](docs/keys.md) · [configuration](docs/configuration.md#keybindings)

### Changed
- The `[keys]` configuration snippet in the docs is clearer. Thanks @petrusck (#120) → [configuration](docs/configuration.md#keybindings)

### Fixed
- Windows: `config.toml` is read again. The Windows launchers spawn the viewer by absolute path (they cannot use the manifest's relative pane command), so the pane never received `HERDR_PLUGIN_CONFIG_DIR`; the viewer then fell back to `$XDG_CONFIG_HOME` / `$HOME`, which Windows does not set, resolved a relative path, and correctly refused to read it. Every key — `editor`, `tree_width`, `tree_max_cols`, the lot — was silently ignored. The launchers now pass the directory through, and a standalone run falls back to `%USERPROFILE%\.config\herdr-file-viewer\config.toml`. Thanks @Jprnp (#129) → [configuration](docs/configuration.md#file-location)
- Tree rows now indent by depth alone. A file row reserved no space for the expand arrow, so a file sat two columns left of a directory at the same depth — putting every file in the exact column of its parent directory's name, and every top-level file two columns left of the directory beside it. Files now reserve the arrow's width, so siblings line up and a child always reads one level in from its parent. Thanks @Jprnp (#130) → [usage](docs/usage.md#the-tree)
- **Windows only:** AltGr characters now trigger their bound actions; only `Ctrl+Alt` plus optional `Shift` on character keys is inferred as AltGr. Thanks @Jprnp (#128) → [keys](docs/keys.md)

## [1.14.0] - 2026-07-20

### Added
- Launch open target: open straight to a file (and optional line) via `--open <path>[:line]` or `HERDR_FILE_VIEWER_OPEN` (flag wins). Same `path:line` shape as a copied line reference. Teach your agent the flag and you can just ask it to open a file, jump to a line, or show a function in the viewer. Thanks @tieubao for the suggestion (#109). → [usage](docs/usage.md#open-at-a-known-file) · [teach your agent](docs/usage.md#teach-your-agent)
- Double-click the content pane title (filename border) to toggle zoom / show or hide the tree (same as `z`). Complements double-clicking a file in the tree to open zoomed. Thanks @nullbio for the suggestion (#106). → [keys](docs/keys.md#mouse)
- `D` cycles changed-file diffs through Delta unified, Delta side-by-side, and plain git diff presentation. Thanks @rrrrnmtsu (#111). → [usage](docs/usage.md#git-awareness) · [keys](docs/keys.md)
- Git-status mode (`d`): filter the tree to current working-tree status and force working-tree diffs (file or directory-scoped); sticky until `d` again, mutually exclusive with baseline-aware `c`. Thanks @jellydn (#110). → [usage](docs/usage.md#git-awareness) · [keys](docs/keys.md)

### Fixed
- Diff and full-file-diff panes now pass the pane width to Delta, so side-by-side diffs size their columns to the pane instead of a fixed fallback, and re-render on resize. Thanks @rrrrnmtsu (#111).

## [1.13.0] - 2026-07-16

### Added
- Configurable content-preview caps: `preview_max_lines` (100–100000, default 10000) and `preview_max_kib` (64–65536 KiB, default 1024 = 1 MB) set how much of a file the content pane shows before a truncated preview. → [configuration](docs/configuration.md)
- Session-only file and line/range annotations, with persistent tree/title/source-line indicators, stable dialogs, an overview for edit/delete/clear-all, and exact clipboard export. Quitting or switching worktree with annotations held confirms first, listing what would be lost (`y` copies them and continues, `Esc` cancels); disable with `confirm_discard = false`. Thanks @mschwarzmueller (#100, #101). → [usage](docs/usage.md#annotating-files-and-ranges)

### Changed
- Settings tab shows each setting's effective value instead of a `(default)` placeholder; the renderer commands are no longer listed (they live in the config file). → [configuration](docs/configuration.md)

### Fixed
- Native Windows launchers now preserve non-ASCII pane titles and paths while parsing herdr JSON, instead of falling back to the plugin install directory. Thanks @mkioutcc (#105).

## [1.12.0] - 2026-07-13

### Added
- Configurable tree layout: `tree_width` (split %, 20–80), `tree_position` (`left`/`right`), and `tree_max_cols` (column cap). → [configuration](docs/configuration.md)
- Configurable mouse-wheel scroll speed: `scroll_lines` (1–10). Thanks @alargoa (#93). → [configuration](docs/configuration.md)
- Customizable keybindings: remap any global key with a `[keys]` table; the `?` overlay lists effective keys. → [keybindings](docs/configuration.md#keybindings)
- Config file (`config.toml`): override the editor, renderer/opener commands, and startup toggles (read-only, `config > env > default`). → [configuration](docs/configuration.md)
- `Z` full-screens the selected file (in-pane zoom + the herdr pane zoom); `z` still zooms in-pane only. → [keys](docs/keys.md)

### Changed
- Default tree width is now 30% (was 40%), for more content room.
- Tree is capped at 30 columns by default so it stays compact on wide panes.
- Docs reorganized: the README is a lean front door; the full reference moved to [docs/](docs/README.md), plus a `CONTRIBUTING.md` and issue/PR templates.

### Fixed
- Wide markdown tables now fit the pane instead of shattering; `w` toggles a full-width scrollable view.

## [1.11.0] - 2026-07-09

### Added
- Ambient mouse text selection: click-drag in the content pane to select text and copy on release; `Shift`+drag keeps the terminal's native selection. Thanks @j-pollack (#78). → [keys](docs/keys.md#mouse)
- `y`/`Y` in line-select mode copy the selected content (no line-number gutter, indentation preserved); `Enter` still copies the `path:line` reference.

### Changed
- In line-select mode a mouse press now places the caret; copying is always an explicit `Enter`/`y`.

### Fixed
- Open-in-tab (`prefix+shift+f`) no longer jumps to a viewer in another workspace.
- Closed the left gap in the rendered-markdown and diff views (now aligned with the syntax view).

## [1.10.0] - 2026-07-07

### Added
- Reveal in file manager (`R`) and open with the OS default app (`O`): read-only, non-blocking hand-offs. Thanks @amitav13 (#68). → [keys](docs/keys.md)

## [1.9.0] - 2026-07-06

### Added
- Copy a line reference: `L` line-select mode. `Enter` copies `path:line` (or `path:start-end`) over OSC 52; `j`/`k` move the marker, `Shift` extends. → [keys](docs/keys.md)

## [1.8.0] - 2026-07-05

### Added
- Native Windows support (preview): builds and runs on `x86_64-pc-windows-msvc`; summon via the `-windows` action ids. Thanks @sanirudh17 (#58). → [Windows](docs/windows.md)

## [1.7.0] - 2026-06-30

The "Optimisations" release: accessibility cues, UX papercuts, and internal hardening.

### Added
- Tree horizontal scroll is keyboard-reachable: `H`/`L` scroll the tree left/right (inert unless the tree is focused).
- A `? help` hint on the content pane's bottom border, so the help overlay is discoverable.
- Empty-state guidance: `Directory: select a file to view` and `No files` instead of a blank pane.

### Changed
- Non-color accessibility cues alongside color: a `●` on dirty directories, a `▶` on the active help tab, a `(current)` label on the current worktree, and theme-relative `REVERSED` highlights for the current match and status banners.
- Renderer fallback notices are now short and actionable instead of raw OS errors.
- Editor hand-off distinguishes a launch failure from a non-zero editor exit.
- Internal: shared the subprocess reaper, hardened test timing/coverage, and fixed doc drift across `install.md` / `renderers.md` / `ARCHITECTURE.md` and the release workflow.

### Fixed
- Content pane no longer shows a stale file under a new title while a render is in flight (shows a `Rendering…` placeholder).

## [1.6.0] - 2026-06-28

### Added
- Go to line (`:`): jump the content pane to a source line by number. → [usage](docs/usage.md)
- Search in file (`/`, `n`/`N`): smartcase, works in every view. → [usage](docs/usage.md)
- Go to file (`f`): fuzzy-find any file in the tree. → [usage](docs/usage.md)
- Help overlay (`?`): What's New + About.
- The tree names its root (top border) and branch (bottom border).

### Changed
- Install reuses the latest released binary even when `main` is ahead of the tag (matches by version, not commit). → [install](docs/install.md)

### Fixed
- The worktree picker's `←` responds immediately after over-scrolling right.

## [1.5.0] - 2026-06-25

### Added
- Scrollbars on the tree and content panes (draggable; press the track to jump).
- The tree scrolls horizontally (horizontal wheel or scrollbar drag) to read long names.
- Hide hidden files (`.`): drop dot-prefixed entries, independent of the gitignore toggle. Thanks @julianduque (#46).

### Fixed
- The tree scrolls to follow the selection on long file lists. Thanks @julianduque (#45).

## [1.4.0] - 2026-06-25

### Added
- Switch worktree (`W`): re-root the viewer at another git worktree in place (read-only, no checkout). The picker marks the current worktree, pre-selects the one with an active herdr agent, and shows each branch + agent status. Thanks @mitralone (#40). → [usage](docs/usage.md)

### Changed
- All notice-strip messages are now stripped of control characters at the render sink.

## [1.3.0] - 2026-06-24

### Added
- Copy a file's path: `y` (repo-relative) / `Y` (absolute), over OSC 52. Thanks @riclib (#37, #38). → [keys](docs/keys.md)

### Security
- The copied path and its notice are stripped of control characters, so a maliciously-named file can't paste-inject or emit a terminal escape.

## [1.2.2] - 2026-06-23

### Changed
- Docs-only re-tag (binary unchanged from 1.2.1): slimmed the README and moved the longer guides into `docs/`, documented `$EDITOR` setup for `e`, added a rendered-markdown screenshot, and trimmed `SECURITY.md`.

## [1.2.1] - 2026-06-23

### Fixed
- Prebuilt install path now works for a normal `herdr plugin install`: the gate compares `HEAD` to a published `COMMIT` marker instead of a local tag (which the install checkout lacks).

## [1.2.0] - 2026-06-23

### Added
- Prebuilt-binary install: tagged releases ship SHA-256-verified binaries for macOS (arm64 + x86_64) and Linux x86_64, so no Rust toolchain is needed on supported platforms (source build on any miss). → [install](docs/install.md)

## [1.1.0] - 2026-06-22

### Added
- Update-available notification: a once-a-day check (read-only `git ls-remote`, off the UI thread) shows a dismissable banner when you're behind; `u` dismisses it, `HERDR_FILE_VIEWER_NO_UPDATE_CHECK` opts out. → [install](docs/install.md)

### Changed
- Clarified updating: re-running `herdr plugin install …` pulls the latest; `--ref` only pins a version.

## [1.0.0] - 2026-06-22

First public release: a git-aware, read-only file viewer that runs as a herdr plugin pane.

### Added
- Tree scoped to your work: rooted at the git worktree top-level (else the launch dir), `.gitignore`-aware with a reveal toggle.
- Git woven in: per-file `M`/`A`/`D`/`?` status markers, a changed-files-only filter, and a baseline toggle (merge-base ⇄ `HEAD`).
- The right view per file: diff for a changed file, rendered markdown, else syntax-highlighted content; cycle with `v` (incl. a full-file diff).
- Navigable content: scroll all directions, wrap (`w`), resize the split (`<`/`>`), zoom (`z`).
- Open in `$EDITOR` (`e`): a read-only hand-off.
- Two ways to summon it: a split action and an idempotent tab action. → [summoning](docs/summoning.md)
- Delegated rendering to `glow` / `delta` / `bat`, each optional with a plain-text fallback. → [renderers](docs/renderers.md)
- Refresh (`r`) and automatic git re-read on focus-gain.

### Security
- Read-only by construction; untrusted content is escape-neutralized and fed to renderers on stdin; every `git` invocation is hardened for untrusted repos. See [SECURITY.md](SECURITY.md).

[1.12.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.12.0
[1.11.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.11.0
[1.10.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.10.0
[1.9.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.9.0
[1.8.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.8.0
[1.7.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.7.0
[1.6.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.6.0
[1.5.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.5.0
[1.4.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.4.0
[1.3.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.3.0
[1.2.2]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.2.2
[1.2.1]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.2.1
[1.2.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.2.0
[1.1.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.1.0
[1.0.0]: https://github.com/smarzban/herdr-file-viewer/releases/tag/v1.0.0
