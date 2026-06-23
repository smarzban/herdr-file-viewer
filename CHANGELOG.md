# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
