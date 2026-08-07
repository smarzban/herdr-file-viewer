#!/usr/bin/env bash
# Launcher for the file viewer as a herdr OVERLAY — a temporary zoomed cover over the active
# pane, which herdr closes back to the previous focus and zoom. Used by the
# `open-file-viewer-overlay` action and any herdr keybinding (a `[[keys.command]]` with
# `type = "shell"`). linux/macos only; there is no Windows variant of this launcher.
#
# SHARED TOGGLE, and its consequence. This reuses the split launcher's `--launch-decision` mode
# verbatim (`src/launch.rs::launch_decision`), which scopes by the pane label "Files" — the
# manifest `title`. A `pane list` row carries NO placement and no per-pane zoom flag (verified
# 2026-08-07 against herdr 0.8.0: `$defs.PaneInfo`), so an overlay viewer and a split viewer are
# indistinguishable. That is deliberate: ONE viewer per tab, whichever way it was summoned. With a
# split viewer already open in this tab, pressing the overlay key focuses THAT split (and it stays
# a split); the mirror also holds — pressing the SPLIT key while an unfocused overlay exists runs
# that script's zoom cycle and flattens the overlay into a plain split.
#
# THE FOCUS BRANCH DIFFERS FROM THE SPLIT LAUNCHER'S, on purpose. herdr implements an overlay as a
# 50/50 split whose new half is tab-zoomed (`pane layout --current` during a live overlay showed
# `splits:[{direction:"right",ratio:0.5}]` + `zoomed:true`; verified 2026-08-07, herdr 0.8.0). So
# the split launcher's focus cycle (`pane zoom <id> --on` then `--off`) *succeeds* and thereby
# strips the overlay's covering zoom, leaving a permanent ordinary split. `plugin pane focus <id>`
# pulls focus AND preserves `zoomed: true` (verified the same sitting), so that is the verb here.
# The CLOSE branch keeps the generic `pane close "$pid"` — verified against a live overlay: exit 0,
# focus and zoom restored, tab back to a single pane.
#
# The open argv was verified 2026-08-07 against herdr 0.8.0 via the BARE `herdr plugin pane` usage
# line (its `open --help` output is stale in 0.8.0 and under-reports the placement enum). Absent
# flags, each for a reason:
#   --cwd                      forbidden repo-wide (#139): herdr resolves the manifest's RELATIVE
#                              pane command against it, so it cannot spawn the viewer. The root
#                              comes from the focused pane's cwd in the context JSON, never a flag.
#   --target-pane, --workspace rejected by herdr for this placement ("overlay and popup plugin
#                              panes target the active pane", invalid_params, exit 1).
#   --width, --height          rejected too ("width and height are only supported when placement
#                              is popup") — popup-only sizing.
#   --direction                split-only in practice; nothing to split here.
# `--env HERDR_FILE_VIEWER_PLACEMENT=overlay` is the marker the viewer reads (`src/host.rs`) to
# skip the herdr pane zoom on `Z` — an overlay is already covering, and zooming it fights herdr's
# own restore-on-close.
set -uo pipefail

herdr_bin="${HERDR_BIN_PATH:-herdr}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
viewer_bin="$script_dir/../target/release/herdr-file-viewer"

open_pane() {
  exec "$herdr_bin" plugin pane open \
    --plugin herdr-file-viewer \
    --entrypoint file-viewer \
    --placement overlay \
    --focus \
    --env HERDR_FILE_VIEWER_PLACEMENT=overlay
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
    exec "$herdr_bin" plugin pane focus "$pid"
    ;;
  "CLOSE "*)
    pid="${decision#CLOSE }"
    exec "$herdr_bin" pane close "$pid"
    ;;
  *)
    open_pane
    ;;
esac
