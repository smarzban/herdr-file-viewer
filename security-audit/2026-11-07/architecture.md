# Architecture (delta run 4)

Scoped standard audit of `smarzban/herdr-file-viewer` at `d17a068ee753daf5ba205bafd59c130b81b8c6c0` (clean worktree). Baseline `97ba479f6ecd577ca5acaad2f32983c852355f9f`. Partial pass: the four-commit delta plus revalidation of the carried git-filter confirmation. Budget unset. Execution is sandboxed-source-and-local-only. This host has no bubblewrap, empty `CapEff`, and `unshare --net` / `--mount` fail, while the tree is writable, so target code is not executed.

## Product and principals

herdr-file-viewer is a read-only keyboard-driven TUI plugin. The local operator views one git worktree. The lower-trust principal is the author of that worktree (file bytes, git config, attributes, submodule config). herdr supplies pane context and `pane list` JSON. Protected resources are the operator account (subprocess execution), files outside the opened root, and the terminal (escape and OSC). There is no peer authentication. Trust is the OS user who launched the process. Comparable baseline, from repo docs: pager CLIs (`glow`, `delta`, `bat`) plus read-only `git`, not libgit2. Same-account self-effect is not a cross-user finding. A repository-controlled git filter that runs as the operator is the historical defect class.

## Stack and limits

Rust 2024 / 1.96, ratatui, crossterm, ansi-to-tui, `ignore`, serde. Entry is `src/main.rs` into `run`, plus `--launch-decision-tab` and `--pick-root`. Offline `cargo test` would write `target/` and is prohibited here. No live herdr, curl, or release workflow.

## Delta

`git diff --stat 97ba479..d17a068` is 10 files, about 93 insertions and 115 deletions. `src/git.rs` and `SECURITY.md` are unchanged.

- `src/help.rs` `about_text` replaces the literal `https://x.com/smarzbanX` with `https://x.com/smarzban`. `handle_help_key` and `handle_help_mouse` only switch sections, scroll, and close. They do not spawn a process or open a URL.
- `docs/keys.md`, `docs/usage.md`, and `tests/docs_consistency.rs` document those help-overlay keys. The security-doc test now ends the remote-notice slice at `- **Git invocations` because `- **Untrusted repository` is not a heading. `SECURITY.md` itself is unchanged and still states the submodule-only filter residual.
- `src/view_policy.rs` drops unused `FileDescriptor.path`. `default_mode` and `applicable_modes` still branch only on `is_markdown`, `is_changed`, and `is_deleted`. `Controller::descriptor` still derives those flags from the path, git status, and `path.exists()`.
- `src/prompt.rs` removes unused `set_cursor` and `From` impls. In-tree caret motion uses `insert` and `move_*`, which stay on char boundaries. No `PromptInput::set_cursor` callers remain.
- `src/presenter.rs` `draw_blank_annotation_cells` no longer returns the painted positions. The caller in `draw_content` ignores a return value. The function sets a cell style and does not write file bytes or spans.
- `src/controller/lineselect.rs`, `src/controller/mouse.rs`, and most of `src/controller/mod.rs` are comments. `descriptor` is the executable edit in `mod.rs`. The render worker still wraps `git.diff` / `git.diff_directory` and `render_at_width` in `catch_unwind` (`spawn_worker`).

## Trust boundaries in scope

- Help display: compile-time About text and operator keys. Control: no command sink on that path.
- View policy: flags only. Path confinement stays in `render.rs` `classify` and `git.rs` `is_within_root`, which this diff does not touch.
- Prompt buffer: operator keystrokes. No file or process sink.
- Annotation blank cells and line-select copy: terminal draw and OSC 52. Prior sanitize-before-copy control is comment-only in this diff and is rechecked.
- Render worker: a panic must still send a result. Same-process availability, not a shared service.
- Git hardening (carried, same source): `configured_filter_overrides` inventories only the opened repo. `status` and `diff` still omit `--ignore-submodules`. `base_git_command` still sets `-c diff.submodule=short`. `SECURITY.md` still documents the submodule-only filter residual. Hunters must not re-file fingerprint `src/git.rs:git_command:submodule-only-filter-on-nested-status`. Draft fix PRs #214 and #215 are not in this commit.

## Prior coverage

Prior run `security-audit/2026-10-20` at `97ba479` (scoped standard, one confirmed finding, zero `needs_validation`). Carried same-source confirmation: the submodule filter fingerprint above. No prior `needs_validation` record to carry. Unchanged surfaces (install script, opener, renderer spawn, root resolver, update client, worktree picker, launch decision, modal intent gate, input registry, narrow layout) stay `out_of_scope`. Their security code is not in this diff.

## Companions

`DESKTOP-MOBILE-AND-LOCAL-IPC.md` privileged-helper and local-file stays on the reopened clipboard unit and on the carried git unit, because those paths spawn or display through a local helper or terminal sink. `RESOURCE-EXHAUSTION-AND-AVAILABILITY.md` stays on the carried git unit only (config inventory and child git). New delta units do not add a helper, an HTTP client, a release step, unsafe code, or a shared-service cost path, so those companions are excluded there with that source fact.
