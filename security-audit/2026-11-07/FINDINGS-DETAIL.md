# Findings detail

One confirmed finding, overall severity medium. No high or critical records.

## Nested submodule git status runs a clean filter configured only in that submodule

Fingerprint: `src/git.rs:git_command:submodule-only-filter-on-nested-status`

### Trace

1. **Entrypoint** `src/controller/mod.rs:1181` `Controller::new_with_changed_file_view`. Construction calls `refresh_git_state` before the first frame. Re-root calls `dispatch_status_refresh` at line 1446, which calls `git.status` at line 1475. A diff render calls `git.diff_directory` at line 1219 or `git.diff` at line 1224.
2. **Propagation** `src/controller/git_apply.rs:168` `refresh_git_state`. After the repo guard this calls `self.git.status()`, then `changed_set`. Baseline toggle calls `git.changed_set` directly. On Head baseline that path still invokes `status`.
3. **Propagation** `src/app.rs:475` `LiveGit::status`. `LiveGit` is built at line 82 with the resolved repo root and forwards status, diff, and diff_directory.
4. **Propagation** `src/git.rs:79` `status`. Argv is `status --porcelain=v1 -z -uall` with no `--ignore-submodules`. `diff` at line 254 and `diff_directory` at line 311 use `diff --no-ext-diff --no-textconv` and spawn through `capture_stdout` at line 597. `diff_directory` calls `status` again at line 336.
5. **Propagation** `src/git.rs:555` `git_command`. Filter overrides are loaded for the opened root only. `git_command_with` adds `--attr-source` when supported (line 577) and the `-c filter.*` overrides. Caller args are appended at line 582 with no `--ignore-submodules`.
6. **Propagation** `src/git.rs:437` `configured_filter_overrides`. The inventory is `git config --get-regexp ^filter\.` in the opened repo, including includes. A driver that exists only in the submodule config is absent.
7. **Propagation** `src/git.rs:541` `base_git_command`. Sets `GIT_OPTIONAL_LOCKS=0`, clears repo-redirecting env, sets `core.fsmonitor=false` and `core.hooksPath` to the null device, and forces `-c diff.submodule=short`. That `-c` is not `--ignore-submodules=dirty`.
8. **Sink** `src/git.rs:616` `run_bytes`. Spawns the assembled git command. On git 2.43.0 the status process spawned `git status --porcelain=2` in the populated submodule, and the diff process spawned `git status --porcelain=2 -uno`. The child runs the submodule clean filter while hashing a same-size file selected by `info/attributes`.

### Evidence

`SECURITY.md:41` states the same residual. `tests/git_baseline.rs:491` covers submodule `diff.external` under `diff.submodule=short`. `tests/git_baseline.rs:442` plants filters on the opened repository. Neither test asserts `--ignore-submodules`.

### Principal and resource

Dummy principal: author of a local superproject the operator opens. Affected resource: the operator account, via a clean command stored only in the submodule config.

### Bounded local check

1. Create a local superproject with a checked-out submodule. Put the clean command only in the submodule config and `* filter=<driver>` in that submodule's `$GIT_DIR/info/attributes`. Change a tracked file without changing its size. The minimum input is a command that appends a marker line, and a file changed from `hello` to `world` with both lines the same length.
2. Run `git -C <super> -c core.fsmonitor=false -c core.hooksPath=/dev/null -c diff.submodule=short --attr-source=4b825dc642cb6eb9a060e54bf8d69288fbee4904 status --porcelain=v1 -z -uall`.
3. Repeat with `diff --no-ext-diff --no-textconv --no-color HEAD --`. Repeat status with `--ignore-submodules=dirty` after setting `submodule.<name>.ignore=none`.

### Observed result

With `info/attributes` and a same-length edit, the marker was created. The child was `git status --porcelain=2` under the viewer status argv and `git status --porcelain=2 -uno` under the viewer diff argv. The marker was absent for a size-changing edit, for worktree `.gitattributes` while `--attr-source` was the empty tree, and for `status --ignore-submodules=dirty`. This run did not re-execute. The invariant shown is that the viewer's current argv still takes the path that ran the submodule-only clean filter.

### Conditions

Populated submodule, driver not in the superproject inventory, `info/attributes` selection, same-size mismatch, operator opens the superproject or the viewer renders a diff, system git that implements submodule dirtiness by spawning `git status` in the submodule (observed on 2.43.0).

### Remediation

Pass `--ignore-submodules=dirty` on every `status` and `diff` argv inside `git_command_with`. Keep `-c diff.submodule=short`. Do not substitute `-c diff.ignoreSubmodules=dirty`. Regression: the existing submodule `diff.external` test stays, and a submodule-only clean filter selected by `info/attributes` must not run on the status argv.
