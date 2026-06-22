#!/usr/bin/env bash
# Idempotent launcher for the file viewer in its own TAB — used by the `open-file-viewer-tab`
# action and a herdr keybinding (e.g. `prefix+shift+f`). "Open-or-switch, toggle on repeat",
# scoped across tabs:
#   - no Files pane anywhere                 -> open the viewer in a new tab (focused)
#   - a Files pane lives in another tab      -> switch to that tab (no duplicate viewer)
#   - a Files pane is in the current tab,
#     but not focused                        -> focus it in place
#   - the focused pane IS the Files pane      -> close it ("toggle off"; herdr auto-closes the
#                                               now-empty tab — verified)
#
# Sibling of scripts/open-file-viewer.sh (the split-pane variant). The OPEN/SWITCHTAB/FOCUS/
# CLOSE decision is computed in-process by the viewer binary (`--launch-decision-tab`, fed the
# `pane list` JSON on stdin), so it is unit-tested and the pane/tab ids it returns are validated
# flag-safe (option-injection guard). Any failure degrades to OPEN (open a fresh viewer tab).
# herdr has no focus-by-id, so an in-tab focus is a `zoom <id> --on/--off` cycle; a cross-tab
# switch is `tab focus <tab_id>`.
set -uo pipefail

herdr_bin="${HERDR_BIN_PATH:-herdr}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
viewer_bin="$script_dir/../target/release/herdr-file-viewer"

open_tab() {
  exec "$herdr_bin" plugin pane open \
    --plugin herdr-file-viewer \
    --entrypoint file-viewer \
    --placement tab \
    --focus
}

decision="OPEN"
if [ -x "$viewer_bin" ]; then
  panes="$("$herdr_bin" pane list 2>/dev/null || true)"
  if [ -n "$panes" ]; then
    decision="$(printf '%s' "$panes" | "$viewer_bin" --launch-decision-tab 2>/dev/null || echo OPEN)"
  fi
fi

case "$decision" in
  "SWITCHTAB "*)
    tid="${decision#SWITCHTAB }"
    exec "$herdr_bin" tab focus "$tid"
    ;;
  "FOCUS "*)
    pid="${decision#FOCUS }"
    "$herdr_bin" pane zoom "$pid" --on >/dev/null 2>&1 || true
    exec "$herdr_bin" pane zoom "$pid" --off
    ;;
  "CLOSE "*)
    pid="${decision#CLOSE }"
    exec "$herdr_bin" pane close "$pid"
    ;;
  *)
    open_tab
    ;;
esac
