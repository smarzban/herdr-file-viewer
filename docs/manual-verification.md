# Manual verification — live herdr launch (AC-17, AC-20)

This is a written, repeatable procedure for the parts of **AC-17** (the viewer opens in a
**split** pane in the current workspace) and **AC-20** (the close key returns control to the
prior pane) that cannot be reliably automated in CI, because they depend on a **live herdr**
host performing the actual pane split and focus hand-back.

What CI already covers automatically, so you do **not** need to re-check by hand:

- `tests/manifest.rs` asserts the manifest statically declares the viewer as a `[[panes]]`
  entry with `placement = "split"`, an `[[actions]]` to summon it, and **no** `[[events]]`
  hook (AC-17 declaration, AC-N4).
- `tests/cli_smoke.rs` and `tests/e2e_keyboard.rs` drive the real binary over a pty and assert
  it draws the tree and exits cleanly on the close key (AC-18, AC-20 at the process level).

This procedure verifies the remaining **live-host** behavior.

## Prerequisites

- herdr **0.7.0+** installed and runnable, on Linux or macOS.
- This plugin built: `cargo build --release` (or installed through herdr, which runs the
  `[[build]]` step). Confirm `./target/release/herdr-file-viewer` exists.
- A directory to browse — ideally a git worktree with some uncommitted changes, so the
  status markers and diff view are exercised. A plain (non-git) directory also works (it
  degrades to a file browser).

## Procedure

1. **Install / link the plugin into herdr.**
   Register this plugin's directory with your herdr installation (per your herdr plugin-
   install docs) so herdr reads `herdr-plugin.toml`. Bind the `open-file-viewer` action to a
   key in your herdr config.

2. **Open a workspace and note the current pane.**
   Start herdr in (or `cd` to) the directory you want to browse. Note which pane currently
   has focus — call it the *origin* pane.

3. **Invoke the action.**
   Press the key you bound to `open-file-viewer`.
   - ✅ **AC-17:** the viewer opens in a **new split pane beside** the origin pane (not full-
     screen, not replacing it). The origin pane is still visible. The viewer shows the file
     tree on the left and a content pane on the right.

4. **Exercise it briefly (sanity, not exhaustive).**
   - Navigate with `j`/`k`; the content pane updates to the selected file.
   - If this is a git worktree: confirm status markers (`M`/`A`/`D`/`?`) appear, press `c`
     to filter to changed files, and select a changed file to see its diff.
   - Press `e` on a file (with `$EDITOR` set) and confirm your editor opens on that file, then
     exit the editor and confirm the viewer redraws cleanly.

5. **Close the viewer.**
   Press `q` (or `Esc`).
   - ✅ **AC-20:** the viewer pane closes and **focus returns to the origin pane**, with the
     origin pane's content intact. Control is back where it started.

## Pass criteria

- [ ] Step 3 — the viewer opened in a **split** pane beside the current work (AC-17).
- [ ] Step 5 — the close key closed the viewer and returned focus to the origin pane (AC-20).

If either fails, capture the herdr version (`herdr --version`), the platform, and the manifest
in use, and file an issue — these are the host-integration points most sensitive to herdr
version changes.
