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
# herdr has no focus-by-id, so a focus is done with a zoom on/off cycle: `zoom <id> --on` focuses
# (and maximizes) the pane, and `--off` un-maximizes while *keeping* it focused (verified). The
# decision is computed from a single `pane list` so repeated key presses are deterministic.
set -uo pipefail

herdr_bin="${HERDR_BIN_PATH:-herdr}"

open_pane() {
  exec "$herdr_bin" plugin pane open \
    --plugin herdr-file-viewer \
    --entrypoint file-viewer \
    --placement split \
    --direction right \
    --focus
}

# Decide OPEN / "FOCUS <id>" / "CLOSE <id>" from the current pane layout. Any parse/tool failure
# (e.g. python3 absent) degrades to OPEN, preserving the original always-open behavior.
panes_json="$("$herdr_bin" pane list 2>/dev/null || true)"
decision="$(printf '%s' "$panes_json" | python3 -c '
import json, sys
try:
    panes = json.load(sys.stdin)["result"]["panes"]
except Exception:
    print("OPEN"); sys.exit(0)
focused = next((p for p in panes if p.get("focused")), None)
tab = focused.get("tab_id") if focused else None
files = next((p for p in panes
              if p.get("label") == "Files" and (tab is None or p.get("tab_id") == tab)), None)
if not files:
    print("OPEN")
elif focused and files.get("pane_id") == focused.get("pane_id"):
    print("CLOSE " + files["pane_id"])
else:
    print("FOCUS " + files["pane_id"])
' 2>/dev/null || echo OPEN)"

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
