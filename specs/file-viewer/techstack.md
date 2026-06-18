# Techstack: herdr-file-viewer

- Status: drafted (techstack-selection stage) — 2026-06-18
- Amended: 2026-06-18 — pinned the herdr CLI/manifest surface (resolving readiness-check M-1).
- Inputs: `specs/file-viewer/design.md`, `specs/file-viewer/acceptance-criteria.md`,
  `constitution.md`
- Next stage: plan
- **All versions verified against current official sources on 2026-06-18.**

Project level (clean repo): choosing the whole stack — language, build, test tooling, and
a concrete product for each design component kind. The design shape is unchanged.

## Cross-cutting choices

| Concern | Product | Version | Checked | Source |
| --- | --- | --- | --- | --- |
| Language | Rust (stable, edition 2024) | 1.96.0 | 2026-06-18 | [rust-lang/rust releases](https://github.com/rust-lang/rust/releases) |
| Build | Cargo (bundled) | with 1.96.0 | 2026-06-18 | bundled with Rust |
| Unit/integration tests | `cargo test` (libtest) | bundled | 2026-06-18 | bundled with Rust |
| TUI render assertions | ratatui `TestBackend` | with ratatui | 2026-06-18 | bundled in ratatui |
| Snapshot tests | `insta` | 1.48.0 | 2026-06-18 | [crates.io/crates/insta](https://crates.io/crates/insta) |
| pty-based e2e | `expectrl` | 0.9.0 | 2026-06-18 | [crates.io/crates/expectrl](https://crates.io/crates/expectrl) |
| Concurrency | `std::thread` + `std::sync::mpsc` | std | 2026-06-18 | std — no crate |
| Context JSON parsing | `serde` (derive) + `serde_json` | 1.0.228 / 1.0.150 | 2026-06-18 | [serde](https://crates.io/crates/serde), [serde_json](https://crates.io/crates/serde_json) |

**Why std threads, not tokio:** off-thread rendering keeps the UI inside AC-23's 300 ms
without a full async runtime. herdr itself uses tokio, but our event loop doesn't need it
(YAGNI).

## Component → product

| Design component (kind) | Product | Version | Checked | Source |
| --- | --- | --- | --- | --- |
| **Presenter** (terminal view layer) | `ratatui` (uses `ratatui-core` 0.1.x) | 0.30.1 | 2026-06-18 | [crates.io/crates/ratatui](https://crates.io/crates/ratatui) |
| **Input Dispatcher** (terminal backend / key events) | `crossterm` | 0.29.0 | 2026-06-18 | [crates.io/crates/crossterm](https://crates.io/crates/crossterm) |
| **Content Renderer** — markdown CLI (delegated) | `glow` | v2.1.2 | 2026-06-18 | [charmbracelet/glow](https://github.com/charmbracelet/glow/releases) |
| **Content Renderer** — diff CLI (delegated) | `delta` | 0.19.2 | 2026-06-18 | [dandavison/delta](https://github.com/dandavison/delta/releases) |
| **Content Renderer** — syntax CLI (delegated) | `bat` | v0.26.1 | 2026-06-18 | [sharkdp/bat](https://github.com/sharkdp/bat/releases) |
| **Content Renderer** — ANSI→ratatui bridge + AC-27 neutralization | `ansi-to-tui` | 8.0.1 | 2026-06-18 | [crates.io/crates/ansi-to-tui](https://crates.io/crates/ansi-to-tui) |
| **Content Renderer** — binary detect / size cap | in-house (NUL/UTF-8 heuristic, byte/line count) | — | 2026-06-18 | std — no crate |
| **Tree Model** (walk + `.gitignore`) | `ignore` | 0.4.26 | 2026-06-18 | [crates.io/crates/ignore](https://crates.io/crates/ignore) |
| **Git Service** (read-only VCS queries) | system `git` CLI via `std::process` | system | 2026-06-18 | no crate (see note) |
| **Root Resolver** | Rust `std` + `git` CLI | std | 2026-06-18 | std |
| **View Policy** | Rust `std` (pure logic; markdown-by-extension) | std | 2026-06-18 | std |
| **Session Controller** | Rust `std` (+ `std::thread`/`mpsc`) | std | 2026-06-18 | std |
| **Editor Launcher** | Rust `std::process` + herdr CLI (new-pane path) | std | 2026-06-18 | std |
| **Host Adapter** | `herdr-plugin.toml` manifest + herdr CLI/socket | herdr ≥ 0.7.0 | 2026-06-18 | [herdr v0.7.0](https://github.com/ogulcancelik/herdr/releases), [plugin docs](https://herdr.dev/docs/plugins/) |

## Key choices, why over the alternatives

- **`ansi-to-tui` is the ingestion path for all content.** glow/delta/bat emit ANSI-styled
  text; `ansi-to-tui` ("Convert ANSI color and style codes into Ratatui Text") maps SGR
  styling into ratatui spans while *not* reproducing cursor/screen-control sequences — so
  it doubles as the AC-27 neutralizer for untrusted file bytes. One bridge, both jobs.
  It depends on `ratatui-core ^0.1.0`, which is what ratatui 0.30 is built on → compatible.
- **`ignore` (ripgrep's crate) for the Tree Model**, not a hand-rolled gitignore matcher:
  fast, parallel, `.gitignore`-aware traversal (AC-4, AC-5) that scales to AC-22's 10k-file
  budget. Reinventing gitignore matching would violate the constitution's "don't reinvent."
- **git via the system CLI, not a crate (`git2`/`gix`):** the Git Service only needs
  read-only `status` / `diff base...HEAD` / `diff HEAD` / `merge-base` — exactly what the
  CLI gives, and what `delta` already consumes. Avoids a heavy native/build dependency
  (YAGNI), and read-only subcommands enforce AC-N2 by construction.
- **glow / delta / bat over in-process crates** (pulldown-cmark / syntect): mandated by
  ADR-0001 (delegate rendering). These are the mature, standard terminal renderers.
- **Rust + ratatui over Go/Zig/runtime langs:** host alignment (herdr is Rust + ratatui),
  single static binary, and the existing `rust-release-check` example shows the exact
  `cargo build --release` → action-runs-binary wiring. (Decided with the user.)

## Criteria that drove a choice

- AC-4 / AC-5 / AC-22 → `ignore` (gitignore-aware, fast walk).
- AC-12 / AC-13 → in-house binary/size guards in the Content Renderer.
- AC-14 / AC-15 / AC-16 → `git` CLI (`merge-base`, `diff base...HEAD`, `diff HEAD`).
- AC-23 → `std::thread` off-thread rendering; glow/delta/bat are fast.
- AC-24 / AC-25 → Content Renderer detects a missing/failed CLI and falls back to plain
  text (bypassing the delegated renderer), surfacing a notice.
- AC-27 → `ansi-to-tui` ingestion drops control sequences; raw path strips them.
- AC-17 / AC-N4 → static `herdr-plugin.toml` (one action, placement = split, no event hooks).
- AC-N2 → read-only `git` subcommands only.

## Constitution check

Read-only (git CLI read-only, `ignore`/`std::fs` read-only, editor hand-off) ✓ · delegate
rendering (glow/delta/bat) ✓ · git first-class (Git Service via CLI) ✓ · keyboard-first
(crossterm events) ✓ · good plugin citizen (`herdr-plugin.toml` + `HERDR_BIN_PATH`/socket,
single binary, session-only state) ✓ · YAGNI (lean deps; no tokio, no `clap`, no git
library) ✓.

## Manifest + herdr CLI surface (verified 2026-06-18)

Verified against the `herdr` skill (runtime CLI) and the `github-link-preview` example
manifest (which uses `[[panes]] placement = "split"`).

**Manifest (`herdr-plugin.toml`):**
- `min_herdr_version = "0.7.0"` (current herdr is v0.7.0, 2026-06-15).
- `platforms = ["linux", "macos"]` for v1 (Windows deferred — see flagged).
- `[[build]]` → `command = ["cargo", "build", "--release"]`.
- A `[[panes]]` entry declares the viewer: `id`, `title`, `placement = "split"`,
  `command = ["./target/release/<bin>"]` — herdr performs the split placement on launch
  (AC-17); no runtime command is issued to open the viewer.
- An `[[actions]]` entry summons it. **No `[[events]]`** (AC-N4); no `[[link_handlers]]`.

**Runtime CLI (AC-19 editor hand-off, T-17), via `$HERDR_BIN_PATH`:**
- `$HERDR_BIN_PATH pane split <pane_id> --direction right --no-focus` → prints JSON; parse
  the new id from `result.pane.pane_id`.
- `$HERDR_BIN_PATH pane run <new_pane_id> "<editor> <file>"` to launch the editor there.
- The current pane id comes from the injected herdr context (or `herdr pane list`, whose
  focused entry is the plugin's own pane).
- Raw protocol reference: herdr socket-api docs (`https://herdr.dev/docs/socket-api/`).

## Unverified / flagged

1. **Exact `ratatui-core` minor** that ratatui 0.30.1 pins vs. what `ansi-to-tui` 8.0.1
   resolves: both target `ratatui-core` 0.1.x (caret `^0.1.0`), so expected compatible —
   confirm the resolved version in `Cargo.lock` at first build. *Low risk.*
2. **ratatui 0.30 ↔ crossterm 0.29 backend pairing:** ratatui re-exports crossterm as its
   default backend; confirm the feature/version line up on first `cargo build`. *Low risk.*
3. **glow / delta / bat are runtime (install-time) externals, not Cargo deps** — their
   presence is environment-dependent and handled by the AC-24/25 fallback. Install guidance
   (and whether to document/auto-detect them) is a plan-stage concern.
4. **herdr CLI/manifest surface — RESOLVED 2026-06-18** (was: subcommands TBD). Pinned in
   "Manifest + herdr CLI surface" above. Residual (Low, non-blocking): the action↔keybinding
   binding may be set by the user in herdr config rather than the manifest (the example
   manifests carry no keybinding field) — does not affect the build.
5. **Windows deferred for v1.** crossterm supports Windows, but glow/delta/bat availability
   and testing add scope; `platforms = ["linux","macos"]` to start.

## Glossary terms touched

None new — techstack introduces product names, not shared vocabulary. `CONTEXT.md` unchanged.
