# Install & updating

Requirements: **herdr 0.7.0+**, on **Linux** or **macOS**.

> **No Rust toolchain needed for tagged releases.** `herdr plugin install smarzban/herdr-file-viewer`
> downloads a prebuilt, SHA-256-verified binary for your platform (macOS arm64/x86_64, Linux x86_64).
> If no matching prebuilt is available — an unsupported platform, or installing from a `main` that is
> ahead of the latest release — it automatically builds from source with `cargo` instead (Rust 1.96+).
> The install command is the same either way.

**Install through herdr** — herdr runs the manifest's `[[build]]` step at install time, either
downloading a prebuilt binary or compiling from source, producing `./target/release/herdr-file-viewer`,
which the viewer pane launches:

```bash
# install (and update — re-run any time to get the latest):
herdr plugin install smarzban/herdr-file-viewer
# …optional: pin a specific older version for reproducibility:
herdr plugin install smarzban/herdr-file-viewer --ref v1.0.0

# or, for local development, link this checkout in place:
cargo build --release            # plugin link does NOT run the [[build]] step, so build first
herdr plugin link /path/to/herdr-file-viewer
```

> You don't need `--ref` to stay current — a bare install pulls the latest. See [Updating](#updating).

Confirm it registered with `herdr plugin list`. To build manually outside herdr:

```bash
cargo build --release
```

## After installing

herdr's install output is intentionally terse (`Installed …` / `Config: …`) and won't prompt you,
so two quick steps remain:

1. **Bind a key** to summon the viewer — see [Quick start](../README.md#quick-start) (or
   [Summoning & keybindings](usage.md) for split-vs-tab and the `--remote` caveat). No key bound
   yet? Open it once from the CLI:
   `herdr plugin action invoke open-file-viewer --plugin herdr-file-viewer`.
2. **(Optional) install the renderers** (`glow` / `delta` / `bat`) so markdown, diffs, and code are
   styled instead of plain text — see [external renderers](renderers.md). The viewer works without
   them (plain-text fallback).

## Updating

herdr has no plugin auto-update, so the viewer tells you when a new release exists: open it
(`prefix+f`) and, if you're behind, a status line appears at the bottom naming the new version
and the command to update. Press `u` to dismiss it for the session.

To update, just re-run the install — it pulls the latest:

```bash
herdr plugin install smarzban/herdr-file-viewer
```

- You **don't** need `--ref` to stay current; it only *pins* a specific version (and a pin stays
  pinned until you change it).
- Want a heads-up the moment a release ships? On GitHub, **Watch → Custom → Releases**.
- Prefer no network check? Set `HERDR_FILE_VIEWER_NO_UPDATE_CHECK=1` in the pane's environment —
  the check (and banner) are disabled entirely. The check otherwise runs at most once per 24h,
  off the UI thread, over a read-only `git ls-remote`, and never blocks or fails the viewer when
  offline.
