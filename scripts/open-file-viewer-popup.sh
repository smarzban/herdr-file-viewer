#!/usr/bin/env bash
# Launcher for the file viewer in a herdr POPUP — a floating, session-modal terminal. Used by the
# `open-file-viewer-popup` action and any herdr keybinding (a `[[keys.command]]` with
# `type = "shell"`). Needs herdr >= 0.7.4. linux/macos only; there is no Windows variant.
#
# OPEN-ONLY, deliberately — no `pane list`, no `--launch-decision`, no toggle, no idempotency.
# A popup is not a pane: the open response is a bare `{"type":"ok"}` with no pane data at all, it
# never appears in `pane list` / `pane layout` / `api snapshot`, it emits no pane lifecycle events,
# and herdr 0.8.0 exposes no CLI verb that closes one (`popup.close` exists on the socket API but
# nothing reaches it from the command line). So there is nothing for a launcher to address after
# the fact. You dismiss the popup by exiting the viewer inside it: `Esc` (the un-remappable close
# floor) or the default `q`; with annotations held, `Esc` opens the discard confirm and `q` (or `y`
# to copy first) finishes it.
#
# A second press while a popup is live is herdr's business, not ours: it answers
# `{"error":{"code":"plugin_pane_open_failed","message":"popup already open"}}` on stderr with
# exit 1 and leaves the first popup untouched (verified 2026-08-07, herdr 0.8.0). That is the right
# feedback, so this script does not retry, poll or swallow it — and the same goes for `ui_busy`
# ("popup panes can only open from the normal workspace view"): it surfaces as herdr's own non-zero
# exit and message.
#
# The argv was verified 2026-08-07 against herdr 0.8.0 via the BARE `herdr plugin pane` usage line;
# the `open --help` output and `herdr completion zsh` are both stale in 0.8.0 and omit `popup`,
# `--width` and `--height` entirely. No `--cwd` (forbidden repo-wide, #139 — the viewed root comes
# from the focused pane's cwd in the context JSON, never from a flag); no `--target-pane` and no
# `--workspace` (herdr rejects them: "overlay and popup plugin panes target the active pane"); no
# `--focus` (accepted but unnecessary — a session-modal surface takes the keyboard).
#
# Sizing is fixed here rather than configurable (YAGNI): `src/presenter.rs`'s NARROW_SPLIT = 80
# collapses the viewer to one column below 80 columns, and on this 167-column tab area an unsized
# popup measured 80x25 (herdr's half-size default — exactly on the boundary, with nothing to
# spare) against 147x43 at 90%/85%. Users who want another size bind a raw `type = "shell"`
# keybinding carrying their own argv (see docs/summoning.md).
set -uo pipefail

herdr_bin="${HERDR_BIN_PATH:-herdr}"

exec "$herdr_bin" plugin pane open \
  --plugin herdr-file-viewer \
  --entrypoint file-viewer \
  --placement popup \
  --width 90% --height 85% \
  --env HERDR_FILE_VIEWER_PLACEMENT=popup
