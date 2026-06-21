#!/usr/bin/env bash
# Launcher for the `open-file-viewer` action: open the viewer's pane as a split
# beside the current work. herdr actions run a command (they have no declarative
# "open this pane" field), so the action shells out to the herdr CLI here, using
# $HERDR_BIN_PATH (herdr injects it; fall back to `herdr` on PATH).
set -euo pipefail

herdr_bin="${HERDR_BIN_PATH:-herdr}"

exec "$herdr_bin" plugin pane open \
  --plugin herdr-file-viewer \
  --entrypoint file-viewer \
  --placement split \
  --direction right \
  --focus
