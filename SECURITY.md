# Security

`herdr-file-viewer` is a **read-only** viewer that routinely opens **untrusted** content. It may browse an agent's worktree, a fresh clone, or files from a collaborator. Its security controls assume that this content is hostile.

## Threat model & mitigations

- **Read-only by construction.** The viewer never writes a file or mutates the git repository.
  Every `git` call uses read-only subcommands; opening a file in an editor is a hand-off to an
  external process, not an in-app edit.

- **The viewer neutralizes terminal controls in untrusted file content.** External renderers receive content on **stdin**, never as a command argument, so a file name cannot inject an argument. Before display, the viewer strips cursor movement, screen controls, OSC, C1, and other control sequences. It keeps only SGR color and style information, which it maps to ratatui styles. A malicious file can paint text only inside the viewer's region. It cannot move the cursor, clear the screen, set the window title, or control the terminal.

- **Remote notices use isolated, bounded display-only data.** They use fixed official HTTPS sources
  off the UI thread under one 15-second deadline. Private Git discovery excludes viewed-repo
  configuration but inherits global/system proxy and CA configuration; curl inherits ambient proxy
  settings and installed curl CA/TLS behavior. Curl starts with `.curlrc` disabled, uses a fixed
  authority over HTTPS only, and follows no redirects. A 1 MiB document cap is enforced in memory
  and on disk: curl's `--max-filesize` cannot bound a length-less (chunked) response, so the
  transient body file's size is watched during the transfer and an over-cap transfer is killed
  mid-stream. Accepted display content is bounded further still (spotlight title and body,
  combined release-details text); `404` withdraws a spotlight, while every other failure
  becomes typed, fail-silent outcomes. Remote Markdown reaches the configured renderer only on
  stdin and passes the terminal-control neutralizer, with no content-triggered actions. The
  complete, atomic, safe-to-delete cache (`update-check.json`) is the sole viewer-owned write and
  never affects the viewed root or Git repository.

- **Git invocations are hardened in untrusted repositories.** Because the opened repo may be hostile,
  queries disable configured clean, smudge, and process filters, and use `--no-ext-diff` /
  `--no-textconv` to refuse diff/textconv programs. Git 2.40+ also uses `--attr-source` to
  read worktree attributes from the empty tree. Concurrent hostile changes to Git configuration
  between filter inspection and query execution are outside this protection.
  `core.fsmonitor` and `core.hooksPath` are neutralized, `GIT_OPTIONAL_LOCKS=0` prevents index
  writes, and repo-redirecting environment variables (`GIT_DIR`, `GIT_WORK_TREE`, …) are scrubbed.
  This hardening lives in a single shared builder so it cannot drift between callers.

- **Injection guards.** Host-supplied pane ids are validated before they reach an argv (so a
  flag-like id can't option-inject the herdr CLI). Paths are passed to `git` as raw `OsStr`
  arguments after a within-root check (no traversal above the root, no arbitrary reads).

- **Resource bounds.** File reads and captured renderer/diff output are size-capped, and external
  renderers run under a wall-clock timeout, so a huge or slow input degrades gracefully rather
  than hanging or exhausting memory.

- **Crash containment.** The viewer catches renderer failures, including a panic on the render worker, and shows a non-fatal notice or placeholder instead of crashing.

## Reporting a vulnerability

Report suspected vulnerabilities through a **GitHub private security advisory**. Open this repository's Security page and select "Report a vulnerability." Do not open a public issue.

We will acknowledge the report and provide a fix or mitigation plan after triage.
