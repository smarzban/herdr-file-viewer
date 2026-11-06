# Security audit report (delta run 4)

Scoped **standard** audit of `smarzban/herdr-file-viewer` at `d17a068ee753daf5ba205bafd59c130b81b8c6c0` (clean). Baseline `97ba479f6ecd577ca5acaad2f32983c852355f9f`. This is a partial pass: four commits (help X profile link, help-overlay key docs, dropped unread view-policy/prompt/annotation code, stale controller comments). Budget was unset. Agents spent: 4 reconnaissance, 3 hunters, 2 coverage critics, 2 verifiers (11). Planned reserves were met; no unit was deferred for budget.

Execution was sandboxed-source-and-local-only. This host has no bubblewrap, empty `CapEff`, and `unshare --net` / `--mount` fail, and the tree is writable, so target code was not executed.

Prior run: `security-audit/2026-10-20` at `97ba479` (one confirmed finding, zero `needs_validation`). Carried same-source confirmation: `src/git.rs:git_command:submodule-only-filter-on-nested-status`. `src/git.rs` and `SECURITY.md` are unchanged. No changed-source revalidation of that root cause. No prior `needs_validation` records.

Twelve prior surfaces stay **out of scope** (install script, opener, renderer spawn, root resolver, update client, worktree picker, launch decision, modal intent gate, input registry, narrow layout, and the other unchanged boundaries in the ledger). Seven delta units are covered. The git unit is a candidate only because it carries the confirmed record.

## Posture

The four-commit delta does not add a command sink, a path check, or a secret. Help About text is a compile-time HTTPS literal. Help keys do not open it. View policy still decides from flags only. Prompt caret motion stays on the operator's string. Annotation blank-cell paint sets a style and does not write file bytes. Line-select copy still filters controls before OSC 52. The render worker still wraps `git diff` and the render in `catch_unwind`. The submodule-only clean-filter residual is unchanged and still documented in `SECURITY.md`. Draft fix PRs #214 and #215 are open and not in this commit.

## Confirmed findings

| Severity | Title | Boundary | Observed result |
|---|---|---|---|
| medium | Nested submodule git status runs a clean filter configured only in that submodule | `git_command` does not neutralize a filter that exists only in a populated submodule | On git 2.43.0 a same-size submodule file selected by `$GIT_DIR/info/attributes` ran that clean filter during status and diff. This run did not re-execute; the argv is unchanged. |

### Nested submodule git status runs a clean filter configured only in that submodule

- **Where:** `src/git.rs` `status` (line 79), `diff` (line 254), `diff_directory` (line 311), `configured_filter_overrides` (line 437), `base_git_command` (line 541), `git_command` (line 555), `run_bytes` (line 616). Launch reaches it from `src/controller/mod.rs` line 1181 via `refresh_git_state` (`src/controller/git_apply.rs` line 168) and `LiveGit::status` (`src/app.rs` line 475).
- **Lower-trust principal:** author of the worktree the operator opens.
- **Bounded reproduction:** local superproject, clean command only in the submodule config, `* filter=<driver>` in that submodule's `$GIT_DIR/info/attributes`, tracked file changed without a size change. Replay the viewer's status argv (`status --porcelain=v1 -z -uall`) and diff argv (`diff --no-ext-diff --no-textconv --no-color`) with `-c diff.submodule=short` and `--attr-source` set to the empty tree.
- **Conditions:** populated submodule; driver absent from the superproject filter inventory; `info/attributes` selection; same-size content or mtime mismatch. A size-changing edit does not run the filter. Worktree `.gitattributes` does not select it while `--attr-source` is the empty tree. `submodule.<name>.ignore=none` defeats `-c diff.ignoreSubmodules=dirty` and does not defeat the argv flag `--ignore-submodules=dirty`.
- **Result:** the marker file was created. The child was `git status --porcelain=2` (status) or `git status --porcelain=2 -uno` (diff). `--ignore-submodules=dirty` left the marker absent. This run did not re-execute.
- **Impact:** the filter runs as the operator during launch status or a diff. Likelihood is medium because the same-size mismatch and `info/attributes` are required. Overall severity is medium.
- **Smallest fix:** pass `--ignore-submodules=dirty` on every `status` and `diff` argv inside `git_command_with`. Keep `-c diff.submodule=short`. Do not substitute `-c diff.ignoreSubmodules=dirty`.

Draft PRs already track fixes and were not updated: [#215](https://github.com/smarzban/herdr-file-viewer/pull/215) (`security-fix/submodule-clean-filter`) and [#214](https://github.com/smarzban/herdr-file-viewer/pull/214) (`fix/submodule-filter-drivers`).

## NEEDS VALIDATION

No `needs_validation` records. The carried confirmation was not re-executed because this host cannot enforce the OS sandbox. The argv and call path are unchanged, so that limit did not open a new lead.

## Hardening and positive patterns

- About interpolates only a three-integer `Version`. The profile URL stays a literal and is drawn through `render::to_text`.
- `section()` in the docs test returns the rest of the file if its end marker disappears. The current marker `- **Git invocations` exists, so the remote-notice slice stops before the git residual. The test does not delete that residual.
- Root-picker drawing slices the prompt at a private caret that every mutator keeps on a UTF-8 boundary.
- `descriptor` uses `path.exists()` only to clear the deleted flag. Content bytes still go through `render::classify`.
- OSC 52 copy base64-encodes text after control filtering. Blank annotation paint calls `set_style` only.
- `spawn_worker` still sends a result after `catch_unwind`.

## Coverage

| Status | Count |
|---|---|
| covered | 7 |
| candidate | 1 (carried confirmation) |
| blocked | 0 |
| deferred | 0 |
| out_of_scope | 12 |

Post-wave critic and the distinct final-clean critic both returned no missing units and no reassign IDs. Phase 5 verified the confirmed record and applied line corrections only (`capture_stdout` spawn is line 597; baseline toggle calls `changed_set`, which still uses `status` on Head). No new confirmed finding in the delta.
