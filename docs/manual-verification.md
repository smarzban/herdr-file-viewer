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
   Build the binary, then register the checkout (a linked plugin does **not** run the
   `[[build]]` step, so build first):

   ```bash
   cargo build --release
   herdr plugin link /path/to/herdr-file-viewer
   herdr plugin list                       # confirm `herdr-file-viewer` is listed + enabled
   ```

   (Or `herdr plugin install <owner>/<repo>` from a published repo, which runs the build.)

2. **Open a workspace and note the current pane.**
   Start herdr in (or `cd` to) the directory you want to browse. Note which pane currently
   has focus — call it the *origin* pane.

3. **Invoke the action.**

   ```bash
   herdr plugin action invoke open-file-viewer --plugin herdr-file-viewer
   ```

   (Or press the key you bound to the action.)
   - ✅ **AC-17:** the viewer opens in a **new split pane beside** the origin pane (not full-
     screen, not replacing it). The origin pane is still visible. The viewer shows the file
     tree on the left and a content pane on the right.

4. **Exercise it briefly (sanity, not exhaustive).**
   - Navigate with `j`/`k`; the content pane updates to the selected file.
   - If this is a git worktree: confirm colored status markers (`M`/`A`/`D`/`?` — changed
     red, new green, folders-with-changes red), press `c` to filter to changed files, and
     select a changed file to see its diff. Press `v` to cycle the view: the compact diff →
     the **full-file diff** (the whole file with a line-number gutter and the changes shown
     inline) → syntax-highlighted content, then back.
   - Press `Tab` to focus the content pane, then scroll with the arrows (`←`/`→` horizontally,
     `↑`/`↓` vertically), `w` to toggle wrapping, and `<`/`>` to resize the split.
   - Press `z` to zoom: the tree disappears and the content pane fills the whole frame (focus
     moves to it, so `↑`/`↓` scroll the file); press `z` again to restore the two columns.
   - Press `Enter` on a folder → it expands/collapses; press `Enter` on a file → it opens in
     zoom mode (content full-screen). `z` — or `q`/`Esc` — returns to the two columns; pressing
     `q`/`Esc` again (now un-zoomed) closes the viewer.
   - Press `e` on a file (with `$EDITOR` set) and confirm your editor opens on that file, then
     exit the editor and confirm the viewer redraws cleanly.
   - Resize the pane (e.g. `herdr pane resize`) and confirm the layout reflows to the new size.

5. **Close the viewer.**
   Press `q` (or `Esc`).
   - ✅ **AC-20:** the viewer pane closes and **focus returns to the origin pane**, with the
     origin pane's content intact. Control is back where it started.

6. **Idempotent launcher / keybinding (launch-or-focus-or-toggle).**
   The action and any keybinding share `scripts/open-file-viewer.sh`. With the viewer open,
   focus the origin pane and invoke the action again, then once more:
   - Invoke with the viewer open-but-unfocused → the **existing** viewer is focused (no second
     viewer pane spawns).
   - Invoke again with the viewer focused → it **closes**.
   - Bind it to a key (README `[[keys.command]]` snippet) and confirm the key drives the same
     open → focus → close cycle.

6b. **Tab launcher (open-or-switch-or-toggle across tabs).**
   Invoke `open-file-viewer-tab` (or bind `prefix+shift+f`):
   - With no viewer open → it opens in a **new tab** (focused), not a split.
   - From a different tab while a viewer tab exists → it **switches to** the viewer tab (no
     second viewer spawns).
   - On the viewer tab with the viewer focused → invoking again **closes** it (the now-empty
     tab disappears).

7. **Mouse (the feel — needs a human; can't be driven over the CLI).** With the viewer open:
   - **Click** a tree row → it selects (the content pane updates). **Click** in the content
     column → it takes focus.
   - **Double-click** a folder → it expands / collapses. **Double-click** a file → it opens in
     **zoom mode** (content full-screen, same as `Enter`); the editor is the `e` key. Single-
     clicking neither toggles nor zooms.
   - **Wheel** over the content pane → it scrolls; over the tree → the selection moves.
   - **Drag** the divider between the columns → the split resizes and tracks the cursor.
   - **`Shift`+drag** to select text → your terminal's native select-and-copy still works (the
     viewer does not eat `Shift`+mouse).
   - Confirm the double-click timing and the divider drag feel responsive — tune
     `DOUBLE_CLICK` / `WHEEL_STEP` in `src/controller.rs` if not.

## Pass criteria

- [ ] Step 3 — the viewer opened in a **split** pane beside the current work (AC-17).
- [ ] Step 5 — the close key closed the viewer and returned focus to the origin pane (AC-20).
- [ ] Step 6 — repeated invocation focuses the existing viewer (no duplicate panes) and toggles
      it closed; a bound key drives the same cycle.
- [ ] Step 6b — `open-file-viewer-tab` opens the viewer in its own tab, switches to it from
      another tab (no duplicate), and toggles it closed when on it.
- [ ] Step 7 — click selects, double-click activates (folder toggle / file zoom), the wheel
      scrolls, the divider drags, and `Shift`+drag still selects text in the terminal.

If either fails, capture the herdr version (`herdr --version`), the platform, and the manifest
in use, and file an issue — these are the host-integration points most sensitive to herdr
version changes.
