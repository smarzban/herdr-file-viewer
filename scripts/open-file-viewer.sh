#!/usr/bin/env bash
# Idempotent launcher for the file viewer — used by both the `open-file-viewer` action and a
# herdr keybinding (a `[[keys.command]]` with `type = "shell"`). "Launch-or-focus, toggle on
# repeat", scoped to the current tab:
#   - no Files pane in the current tab      -> open a split (focused)
#   - a Files pane exists but isn't focused  -> focus it
#   - the focused pane IS the Files pane     -> close it ("hide"; herdr has no hide-without-close,
#                                               and reopening just re-walks the tree — cheap)
#
# herdr actions/keybindings run a command (no declarative "open this pane" field), so this shells
# out to the herdr CLI via $HERDR_BIN_PATH (herdr injects it; fall back to `herdr` on PATH).
#
# The OPEN/FOCUS/CLOSE decision is computed in-process by the viewer binary itself
# (`herdr-file-viewer --launch-decision`, fed the `pane list` JSON on stdin) — so it is unit-
# tested and the pane id it returns is already validated as flag-safe (option-injection guard).
# Any failure (binary missing, parse error, no focused pane) degrades to OPEN, preserving the
# original always-open behavior. herdr has no focus-by-id, so a focus is a `zoom <id> --on/--off`
# cycle: `--on` focuses (and maximizes) the pane, `--off` un-maximizes while keeping it focused.
set -uo pipefail

herdr_bin="${HERDR_BIN_PATH:-herdr}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
viewer_bin="$script_dir/../target/release/herdr-file-viewer"

open_pane() {
  exec "$herdr_bin" plugin pane open \
    --plugin herdr-file-viewer \
    --entrypoint file-viewer \
    --placement split \
    --direction right \
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
    pid="${decision#FOCUS }"
    "$herdr_bin" pane zoom "$pid" --on >/dev/null 2>&1 || true
    exec "$herdr_bin" pane zoom "$pid" --off
    ;;
  "CLOSE "*)
    pid="${decision#CLOSE }"
    exec "$herdr_bin" pane close "$pid"
    ;;
  *)
    open_pane
    ;;
esac
