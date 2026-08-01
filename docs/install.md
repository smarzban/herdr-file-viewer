# Install & updating

Requirements: **herdr 0.7.0+**, on **Linux** or **macOS** (native Windows
`x86_64-pc-windows-msvc` is a [preview](windows.md)). **Git** must be on `PATH` at
runtime. The viewer shells out to the system `git` CLI (read-only subcommands) for the
git-aware tree (status markers, changed-only filter, baseline toggle) and the diff view.
Without git the viewer still opens, but those features are degraded (no status colors, no
diffs). The optional renderers (`glow` / `delta` / `bat`) are separate. See
[external renderers](renderers.md).

> **No Rust toolchain needed when a prebuilt exists.** `herdr plugin install smarzban/herdr-file-viewer`
> downloads a prebuilt, SHA-256-verified binary for your platform (macOS arm64/x86_64, Linux x86_64,
> Windows x86_64 preview).
> The prebuilt is matched by **version**, so you get it even when `main` is ahead of the latest tag.
> You'll receive the most recent released binary (a note tells you when newer, unreleased changes
> aren't in it yet). It builds from source with `cargo` (Rust 1.96+) only when there's no matching
> prebuilt at all: an unsupported platform, or a version that hasn't been released yet. The install
> command is the same either way.

**Install through herdr**: herdr runs the manifest's `[[build]]` step at install time, either
downloading a prebuilt binary or compiling from source, producing `./target/release/herdr-file-viewer`,
which the viewer pane launches:

```bash
# install (and update, re-run any time to get the latest):
herdr plugin install smarzban/herdr-file-viewer
# …optional: pin a specific older version for reproducibility:
herdr plugin install smarzban/herdr-file-viewer --ref v1.0.0

# or, for local development, link this checkout in place:
cargo build --release            # plugin link does NOT run the [[build]] step, so build first
herdr plugin link /path/to/herdr-file-viewer
```

> You don't need `--ref` to stay current. A bare install pulls the latest. See [Updating](#updating).

Confirm it registered with `herdr plugin list`. To build manually outside herdr:

```bash
cargo build --release
```

## After installing

herdr's install output is intentionally terse (`Installed …` / `Config: …`) and won't prompt you,
so two quick steps remain:

1. **Bind a key** to summon the viewer. See [Quick start](../README.md#quick-start) (or
   [Summoning the viewer](summoning.md) for split-vs-tab and the `--remote` caveat). No key bound
   yet? Open it once from the CLI:
   `herdr plugin action invoke open-file-viewer --plugin herdr-file-viewer`.
2. **(Optional) install the renderers** (`glow` / `delta` / `bat`) so markdown, diffs, and code are
   styled instead of plain text. See [external renderers](renderers.md). The viewer works without
   them (plain-text fallback).

## Updating

herdr has no plugin auto-update. When the advisory check finds a newer release, a status line at
the bottom names it and points to `?` for details. The install command moved from that status row
to the **What's New** section, under **Available updates**, so the footer stays short.

That command is copy only: the viewer never runs it, downloads nothing, and takes no automatic
action. To update, copy it or just re-run the install yourself. It pulls the latest:

```bash
herdr plugin install smarzban/herdr-file-viewer
```

- You **don't** need `--ref` to stay current; it only *pins* a specific version (and a pin stays
  pinned until you change it).
- Want a heads-up the moment a release ships? On GitHub, **Watch → Custom → Releases**.
- Prefer no remote notices? Set `HERDR_FILE_VIEWER_NO_UPDATE_CHECK` in the pane's environment
  (to any value, the var's mere presence disables the check) and the check plus every remote
  notice are disabled entirely. The check otherwise runs at most once per 24h, off the UI thread,
  and never blocks or fails the viewer when offline. See [staying up to date](usage.md#staying-up-to-date)
  for project spotlights, dismissal, freshness, and the status forms.
