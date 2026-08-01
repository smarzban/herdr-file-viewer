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
  move the cursor, clear the screen, set the window title, or otherwise drive the terminal; it
  can only paint text inside the viewer's own region.

- **Remote notices → fixed authority, bounded advisory data.** Remote notices have one publishing
  authority: `https://github.com/smarzban/herdr-file-viewer`. A hardened, noninteractive system-Git
  `ls-remote` query discovers only that repository's symbolic `HEAD` and stable tags; `CHANGELOG.md`
  is then read at the exact object ID resolved for the detected release tag. For an annotated tag,
  the resolved ID is its peeled commit; `project-spotlight.md` is read at the discovered
  default-branch HEAD object. The raw-document client is `ureq` 3.3.0 with default
  features disabled and only `rustls` enabled, and is fixed to `https://raw.githubusercontent.com`:
  HTTPS only, no proxy, and no redirects. This ureq/rustls dependency path and `Cargo.lock` are an
  explicit audit surface.

  The remote path has no credential posture: discovery clears inherited configuration, disables
  terminal prompting, and empties Git/SSH askpass and credential-helper variables. It sends no
  application credentials or telemetry. Discovery stdout is capped at 256 KiB. Each document is
  accepted only when it is at most 1 MiB. Its reader takes at most one additional sentinel byte
  solely to detect and reject oversize. One 15-second absolute deadline covers discovery plus both
  documents. The advisory `update-check.json` cache is capped at 20 MiB; its exact remote fields are each
  capped at 1 MiB and complete revisions are staged then atomically renamed under an advisory lock.
  Failures are typed and silent, so missing, invalid, unavailable, timeout, or transport states
  cannot become diagnostics or an alternate source.

  Remote bytes remain display-only to viewer-owned handling: they are sent to the configured
  Markdown renderer on stdin, never shell-interpreted or used as an argument, and all rendered
  output passes the same terminal neutralizer. The configured Markdown renderer executable is
  trusted local code and may have possible side effects outside those viewer guarantees. The
  neutralizer strips cursor, screen, OSC, C0, and C1 control sequences before ratatui display; only
  safe SGR styling remains. The viewer performs no automatic install/download/URL open/clipboard
  action, viewed-root or Git mutation, or raw terminal control. The only viewer-owned write is the
  advisory cache outside the viewed root. Residual trust includes the compiled GitHub authorities
  and their TLS trust path, plus the configured local Markdown renderer command: the viewer relies
  on the endpoint and successful TLS validation for authentic bytes; a transport or TLS-validation
  failure becomes a silent unavailable result, not a fallback endpoint. See [remote notices:
  publishing & trust](docs/remote-notices.md).

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

Please report suspected vulnerabilities privately rather than opening a public issue: open a
**GitHub private security advisory** ("Security" → "Report a vulnerability") on this repository.

You'll get an acknowledgement, and a fix or mitigation plan once the report is triaged. Thank you
for helping keep the viewer safe.
