# open-file-viewer-tab.ps1 -- Windows sibling of scripts/open-file-viewer-tab.sh.
#
# Idempotent launcher for the file viewer in its own TAB -- used by the `open-file-viewer-tab`
# action and a herdr keybinding (e.g. `prefix+shift+f`). "Open-or-switch, toggle on repeat",
# scoped across tabs:
#   - no Files pane anywhere                  -> open the viewer in a new tab (focused)
#   - a Files pane lives in another tab       -> switch to that tab (no duplicate viewer)
#   - a Files pane is in the current tab,
#     but not focused                         -> focus it in place
#   - the focused pane IS the Files pane      -> close it ("toggle off"; herdr auto-closes the
#                                               now-empty tab -- verified)
#
# Sibling of scripts/open-file-viewer.ps1 (the split-pane variant). The OPEN/SWITCHTAB/FOCUS/
# CLOSE decision is computed in-process by the viewer binary
# (`herdr-file-viewer.exe --launch-decision-tab`, fed the `pane list` JSON on stdin), so it is
# unit-tested (src/launch.rs) and the pane/tab ids it returns are validated flag-safe
# (option-injection guard). Any failure degrades to OPEN (open a fresh viewer tab). herdr has no
# focus-by-id, so an in-tab focus is a `zoom <id> --on/--off` cycle; a cross-tab switch is
# `tab focus <tab_id>`.

$ErrorActionPreference = 'Continue'

$HerdrBin = if ($env:HERDR_BIN_PATH) { $env:HERDR_BIN_PATH } else { 'herdr' }

# See open-file-viewer.ps1: a NORMAL absolute plugin root (strip any `\\?\` prefix) so herdr resolves
# the pane's relative command against a normalizable `--cwd`, not the `\\?\` server cwd.
$PluginRoot = Split-Path -Parent $PSScriptRoot
if ($PluginRoot.StartsWith('\\?\')) { $PluginRoot = $PluginRoot.Substring(4) }
$ViewerBin = Join-Path $PluginRoot 'target\release\herdr-file-viewer.exe'

function Open-Tab {
    & $HerdrBin plugin pane open --plugin herdr-file-viewer --entrypoint file-viewer-windows --cwd $PluginRoot --placement tab --focus
    exit $LASTEXITCODE
}

$Decision = 'OPEN'
if (Test-Path $ViewerBin) {
    $panes = & $HerdrBin pane list 2>$null
    if ($LASTEXITCODE -ne 0) { $panes = $null }
    if ($panes) {
        $panesText = ($panes -join "`n")
        $Decision = ($panesText | & $ViewerBin --launch-decision-tab 2>$null)
        if ($LASTEXITCODE -ne 0 -or -not $Decision) { $Decision = 'OPEN' }
    }
}

if ($Decision -like 'SWITCHTAB *') {
    $TabId = $Decision.Substring(10)
    # If the target tab vanished between the pane-list snapshot and now (a race -- the viewer
    # tab was closed in between), fall back to opening a fresh viewer tab rather than leaving the
    # keypress a silent no-op.
    & $HerdrBin tab focus $TabId
    if ($LASTEXITCODE -ne 0) { Open-Tab } else { exit 0 }
} elseif ($Decision -like 'FOCUS *') {
    $PaneId = $Decision.Substring(6)
    & $HerdrBin pane zoom $PaneId --on *> $null
    & $HerdrBin pane zoom $PaneId --off
    exit $LASTEXITCODE
} elseif ($Decision -like 'CLOSE *') {
    $PaneId = $Decision.Substring(6)
    & $HerdrBin pane close $PaneId
    exit $LASTEXITCODE
} else {
    Open-Tab
}
