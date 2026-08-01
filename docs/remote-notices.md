# Remote notices: publishing and trust contract

This is the canonical maintainer reference for the viewer's advisory remote notices. It records the
implemented source and trust boundary, not a general notice channel or a user-facing usage guide.

## Fixed authority and source identities

The only publishing authority is the official repository,
[`https://github.com/smarzban/herdr-file-viewer`](https://github.com/smarzban/herdr-file-viewer).
Release discovery uses the system Git command `git ls-remote --symref` against that fixed repository
to obtain the symbolic `HEAD`, its object ID, and stable `vMAJOR.MINOR.PATCH` tags. Those tags
have no prerelease or build suffixes. The raw-document authority is fixed separately to
`https://raw.githubusercontent.com`; neither authority, proxy, redirect target, or transport mode
is configurable through the environment or viewer configuration.

The gateway keeps source identity separate from display policy:

- **Release details:** after finding an eligible newer release, it reads `CHANGELOG.md` from the
  exact object ID resolved for the detected release tag, never from the default branch or another
  tag. For an annotated tag, the resolved ID is its peeled commit; for a lightweight tag it is the
  direct object. Eligible level-two release sections remain immutable cache data only while they
  describe that same detected release.
- **Project spotlight:** it reads `project-spotlight.md` from the current default-branch HEAD object
  returned by that discovery, never from an alternate branch or a mutable path at a later revision.
  A valid document is UTF-8 whose first nonblank `# ` heading supplies the project name; the
  remaining document body is the spotlight content. The title line is not part of that body.

There is one current maintainer software spotlight, identified by that exact document, rather than a
general channel, feed, targeting system, or message history.

## Acquisition and failure contract

One background refresh shares one absolute **15 seconds** deadline across Git discovery and both
possible document requests. Each raw changelog and spotlight document is accepted only when it is
at most **1 MiB**; its reader takes at most one additional sentinel byte solely to detect and
reject oversize. Release-discovery stdout is separately capped at 256 KiB. A deadline, oversized
body,
transport failure, malformed response, redirect, or unexpected HTTP status is not surfaced as an
error message.

Outcomes are typed and independent:

| Source condition | Projection |
| --- | --- |
| `Available` document bytes | The release policy selects exact eligible sections, or the spotlight policy accepts a valid heading/body. |
| `Missing` document | Only HTTP 404 is missing. A missing spotlight withdraws it; missing release details do not erase an otherwise detected release. |
| `Unavailable` | Network, Git, timeout, malformed, redirect, non-404 status, and over-cap failures preserve independently valid cached data. |
| Invalid present spotlight | Invalid UTF-8, no first nonblank `# ` heading, or an empty neutralized title is a conclusive withdrawal, not `Unavailable`. |

The status row and Help projection fail silently: they expose usable typed data only, with no raw
network, Git, or parser diagnostic. A failed document never suppresses a valid neighboring notice.

## Advisory cache and dismissal scope

`update-check.json` is the bounded, advisory persistent-state exception. It is stored in the
viewer cache directory, records the successful-check time, detected release, immutable release
details, spotlight bytes/retrieval time, and an exact dismissed spotlight identity. It is safe to
delete: absence, invalid data, or an unwritable cache only causes an advisory recheck.

The cache throttles a successful release check for 24 hours. At exactly 24 hours, or with a future
check timestamp, a refresh is eligible. Spotlight freshness is also evaluated at session start:
only a timestamp less than 24 hours old is fresh. Stale and future spotlight content is hidden and
fetched only when the shared daily refresh is eligible or already underway. A fresh result does not
age into a request during that session. The encoded cache is capped at 20 MiB. Each cached remote
field is capped at 1 MiB.

Cache updates are best-effort and atomic for readers: one worker applies an intent-owned delta under
an advisory lock, writes a complete staged revision, then renames it into place. This protects
complete revisions during normal operation, not a crash- or power-loss durability guarantee.

A dismissal is deliberately scoped. Dismissing a status row is session-only. An update dismissal is
never persisted. A spotlight dismissal is remembered only as the exact accepted document identity;
it suppresses that matching spotlight status in later sessions but leaves its accepted body readable
in What's New.

## Display-only boundary

Remote notices are display-only data. The What's New composer receives an already-projected snapshot
and local embedded text, then passes each document through the Markdown rendering boundary. Remote
content is supplied to a configured renderer on standard input, never as a command argument, and
its output is converted to ratatui text. Escape, cursor, screen, OSC, and C1 control sequences are
neutralized before display; only safe SGR styling can become in-pane style spans.

The configured Markdown renderer executable is trusted local code. It has possible side effects.
The viewer's display-only promise covers its own handling, not that executable:
it supplies remote content only on standard input, never shell-interprets it or uses it as an
argument, and neutralizes the renderer output before display. This entails no shell interpretation
of remote content.

The viewer's remote-notice path has no action semantics. It has no automatic install and no download.
There is no URL open and no clipboard operation. There is no viewed-root or Git mutation. The
remote-notice path sends no application credentials and no telemetry. It permits no raw terminal
control. The only viewer-owned on-disk effect is the advisory cache outside the viewed root. Fixed
install guidance that may appear in rendered text is copy only, never an executable action.

## Maintainer audit boundary

Review changes to the fixed source identities, resource caps, deadline, cache schema, renderer
boundary, or this contract together. The HTTPS document client is `ureq` 3.3.0 with default features
disabled and only its `rustls` feature enabled; audit `Cargo.lock` and that ureq/rustls dependency
surface with the rest of the release dependencies. HTTPS authentication still relies on the compiled
GitHub authorities and their TLS trust path, so a trust or transport failure must remain an
`Unavailable` advisory result, not a fallback source.
