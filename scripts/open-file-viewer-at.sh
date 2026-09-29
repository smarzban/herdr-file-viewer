#!/usr/bin/env bash
# Launcher for "open the file viewer at a chosen directory" — used by the `open-file-viewer-at`
# action and a herdr keybinding (e.g. `prefix+d`). It only opens the manifest's `root-picker`
# pane as a herdr popup; the picker (`herdr-file-viewer --pick-root`) asks for the directory and
# opens a NEW viewer tab rooted there via `plugin pane open --env HERDR_FILE_VIEWER_ROOT=<dir>`.
# No `--cwd` anywhere: the manifest pane commands are relative, so a foreign cwd would break or
# redirect the spawn (#139). Popups need herdr >= 0.7.5 (the manifest's min_herdr_version).
set -uo pipefail

herdr_bin="${HERDR_BIN_PATH:-herdr}"

exec "$herdr_bin" plugin pane open \
  --plugin herdr-file-viewer \
  --entrypoint root-picker \
  --placement popup
