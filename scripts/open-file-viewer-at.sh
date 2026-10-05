#!/usr/bin/env bash
# Launcher for "open the file viewer at a chosen directory" — used by the `open-file-viewer-at`
# (split) and `open-file-viewer-at-tab` (tab) actions and their herdr keybindings (e.g. `prefix+d`
# and `prefix+alt+d`).
# It only opens the manifest's `root-picker` pane as a herdr popup, telling it where the viewer
# goes (`$1`: `split`, the default, or `tab`). The picker (`herdr-file-viewer --pick-root`) asks
# for the directory and opens the viewer there, handing the directory over as
# HERDR_FILE_VIEWER_ROOT=<dir>.
# The root never travels as a cwd flag: the manifest pane commands are relative, so a foreign
# cwd would break or redirect the spawn (#139). Popups need
# herdr >= 0.7.5 (the manifest's min_herdr_version).
set -uo pipefail

herdr_bin="${HERDR_BIN_PATH:-herdr}"

case "${1:-split}" in
  tab) placement="tab" ;;
  *) placement="split" ;;
esac

exec "$herdr_bin" plugin pane open \
  --plugin herdr-file-viewer \
  --entrypoint root-picker \
  --placement popup \
  --env "HERDR_FILE_VIEWER_PICK_PLACEMENT=$placement"
