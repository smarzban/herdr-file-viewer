#!/usr/bin/env bash
# Idempotent launcher for the file viewer as an OVERLAY over the current pane — used by the
# `open-file-viewer-overlay` action and a herdr keybinding (e.g. `prefix+alt+f`). Same
# "launch-or-focus, toggle on repeat" as scripts/open-file-viewer.sh, scoped to the current tab:
#   - no Files pane in the current tab      -> open the viewer as an overlay (focused)
#   - a Files pane exists but isn't focused  -> bring it back up as the overlay
#   - the focused pane IS the Files pane     -> close it (the tab goes back to its layout)
#
# herdr's `overlay` placement is a split beside the active pane with the tab zoomed onto it
# (herdr 0.8.2). So the FOCUS branch is `zoom --on` alone: the split launcher follows it with
# `zoom --off` to un-maximize back into the split, which here would flatten the overlay into a
# plain 50/50 split.
#
# The OPEN/FOCUS/CLOSE decision is the split launcher's (`herdr-file-viewer --launch-decision`,
# fed the `pane list` JSON on stdin). It only asks whether a "Files" pane exists in the focused
# pane's tab, so it doesn't care how that pane was placed — a viewer opened by any launcher is
# found, never duplicated. Unit-tested (src/launch.rs); the pane id it returns is validated
# flag-safe. Any failure degrades to OPEN.
set -uo pipefail

herdr_bin="${HERDR_BIN_PATH:-herdr}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
viewer_bin="$script_dir/../target/release/herdr-file-viewer"

open_overlay() {
  exec "$herdr_bin" plugin pane open \
    --plugin herdr-file-viewer \
    --entrypoint file-viewer \
    --placement overlay \
    --focus
}

decision="OPEN"
if [ -x "$viewer_bin" ]; then
  panes="$("$herdr_bin" pane list 2>/dev/null || true)"
  if [ -n "$panes" ]; then
    decision="$(printf '%s' "$panes" | "$viewer_bin" --launch-decision 2>/dev/null || echo OPEN)"
  fi
fi

case "$decision" in
  "FOCUS "*)
    # No `--off` here (see the header).
    pid="${decision#FOCUS }"
    exec "$herdr_bin" pane zoom "$pid" --on
    ;;
  "CLOSE "*)
    pid="${decision#CLOSE }"
    exec "$herdr_bin" pane close "$pid"
    ;;
  *)
    open_overlay
    ;;
esac
