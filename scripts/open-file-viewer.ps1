# open-file-viewer.ps1 -- Windows sibling of scripts/open-file-viewer.sh.
#
# Idempotent launcher for the file viewer -- used by both the `open-file-viewer` action and a
# herdr keybinding (a `[[keys.command]]` with `type = "shell"`, run via PowerShell on Windows).
# "Launch-or-focus, toggle on repeat", scoped to the current tab:
#   - no Files pane in the current tab      -> open a split (focused)
#   - a Files pane exists but isn't focused -> focus it
#   - the focused pane IS the Files pane    -> close it ("hide"; herdr has no hide-without-close,
#                                              and reopening just re-walks the tree -- cheap)
#
# herdr actions/keybindings run a command (no declarative "open this pane" field), so this shells
# out to the herdr CLI via $env:HERDR_BIN_PATH (herdr injects it; fall back to `herdr` on PATH).
#
# The OPEN/FOCUS/CLOSE decision is computed in-process by the viewer binary itself
# (`herdr-file-viewer.exe --launch-decision`, fed the `pane list` JSON on stdin) -- so it is
# unit-tested (src/launch.rs) and the pane id it returns is already validated as flag-safe
# (option-injection guard). Any failure (binary missing, parse error, no focused pane) degrades
# to OPEN, preserving the original always-open behavior. herdr has no focus-by-id, so a focus is
# a `zoom <id> --on/--off` cycle: `--on` focuses (and maximizes) the pane, `--off` un-maximizes
# while keeping it focused.

$ErrorActionPreference = 'Continue'

$HerdrBin = if ($env:HERDR_BIN_PATH) { $env:HERDR_BIN_PATH } else { 'herdr' }
$ViewerBin = Join-Path $PSScriptRoot '..\target\release\herdr-file-viewer.exe'

function Open-Pane {
    & $HerdrBin plugin pane open --plugin herdr-file-viewer --entrypoint file-viewer --placement split --direction right --focus
    exit $LASTEXITCODE
}

$Decision = 'OPEN'
if (Test-Path $ViewerBin) {
    $panes = & $HerdrBin pane list 2>$null
    if ($LASTEXITCODE -ne 0) { $panes = $null }
    if ($panes) {
        $panesText = ($panes -join "`n")
        $Decision = ($panesText | & $ViewerBin --launch-decision 2>$null)
        if ($LASTEXITCODE -ne 0 -or -not $Decision) { $Decision = 'OPEN' }
    }
}

if ($Decision -like 'FOCUS *') {
    $PaneId = $Decision.Substring(6)
    & $HerdrBin pane zoom $PaneId --on *> $null
    & $HerdrBin pane zoom $PaneId --off
    exit $LASTEXITCODE
} elseif ($Decision -like 'CLOSE *') {
    $PaneId = $Decision.Substring(6)
    & $HerdrBin pane close $PaneId
    exit $LASTEXITCODE
} else {
    Open-Pane
}
