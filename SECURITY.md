# Security

`herdr-file-viewer` is a **read-only** viewer that routinely opens **untrusted** content: the
files and git repositories it browses may be an agent's worktree, a fresh clone, or anything a
collaborator handed you. Its security posture is built around that.

## Threat model & mitigations

- **Read-only by construction.** The viewer never writes a file or mutates the git repository.
  Every `git` call uses read-only subcommands; opening a file in an editor is a hand-off to an
  external process, not an in-app edit.

- **Untrusted file content → terminal-control neutralization.** All file bytes are treated as
  hostile. Content is fed to the external renderers on **stdin** (never as a command argument, so
  a file name can't inject), and the result is run through an escape-sequence neutralizer before
  display: cursor-movement, screen-control, OSC, C1, and other control sequences are stripped;
  only SGR (color/style) is kept and mapped to ratatui styles. A malicious file therefore cannot
  move the cursor, clear the screen, set the window title, or otherwise drive the terminal — it
  can only paint text inside the viewer's own region.

- **Untrusted repository → hardened git invocations.** Because the opened repo may be hostile,
  every `git` command is hardened against repo-controlled code execution: `--no-ext-diff` /
  `--no-textconv` refuse repo-configured diff/textconv programs, `--attr-source` reads attributes
  from the empty tree (so a planted `.gitattributes` can't designate a filter/diff driver),
  `core.fsmonitor` and `core.hooksPath` are neutralized, `GIT_OPTIONAL_LOCKS=0` prevents index
  writes, and repo-redirecting environment variables (`GIT_DIR`, `GIT_WORK_TREE`, …) are scrubbed.
  This hardening lives in a single shared builder so it cannot drift between callers.

- **Injection guards.** Host-supplied pane ids are validated before they reach an argv (so a
  flag-like id can't option-inject the herdr CLI). Paths are passed to `git` as raw `OsStr`
  arguments after a within-root check (no traversal above the root, no arbitrary reads).

- **Resource bounds.** File reads and captured renderer/diff output are size-capped, and external
  renderers run under a wall-clock timeout, so a huge or slow input degrades gracefully rather
  than hanging or exhausting memory.

- **Crash containment.** A renderer failure (including a panic on the render worker) is contained
  and surfaced as a non-fatal notice/placeholder; the viewer never crashes on bad input.

## Reporting a vulnerability

Please report suspected vulnerabilities privately rather than opening a public issue:

- Open a **GitHub private security advisory** ("Security" → "Report a vulnerability") on this
  repository, **or**
- Email **git@smarzban.com** with details and reproduction steps.

You'll get an acknowledgement, and a fix or mitigation plan once the report is triaged. Thank you
for helping keep the viewer safe.
