# open-file-viewer-overlay.ps1 -- Windows sibling of scripts/open-file-viewer-overlay.sh.
#
# Idempotent launcher for the file viewer as an OVERLAY over the current pane -- used by the
# `open-file-viewer-overlay-windows` action and a herdr keybinding (e.g. `prefix+alt+f`). Same
# "launch-or-focus, toggle on repeat" as open-file-viewer.ps1, scoped to the current tab:
#   - no Files pane in the current tab      -> open the viewer as an overlay (focused)
#   - a Files pane exists but isn't focused -> bring it back up as the overlay
#   - the focused pane IS the Files pane    -> close it (the tab goes back to its layout)
#
# Sibling of scripts/open-file-viewer.ps1 -- see its header for WHY the Windows launchers spawn the
# viewer by ABSOLUTE path (`pane split` + `pane run`) instead of `plugin pane open --entrypoint`.
# That also means we can't ask herdr for `--placement overlay`, so Open-Overlay builds the same
# state by hand: an overlay is a split with the tab zoomed onto it, hence split, run the viewer,
# then `pane zoom <id> --on`. For the same reason the FOCUS branch is `zoom --on` alone (no
# trailing `--off`) -- see open-file-viewer-overlay.sh.
#
# The OPEN/FOCUS/CLOSE decision is the split launcher's (`herdr-file-viewer.exe --launch-decision`,
# fed `pane list` JSON on stdin) -- unit-tested (src/launch.rs), pane id validated flag-safe. Any
# failure degrades to OPEN.

$ErrorActionPreference = 'Continue'

# PowerShell 5.1 otherwise decodes herdr's UTF-8 JSON with the legacy console code page;
# non-ASCII pane titles or paths can corrupt the JSON and trigger the plugin-root fallback.
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[Console]::OutputEncoding = $Utf8NoBom
$OutputEncoding = $Utf8NoBom

$HerdrBin = if ($env:HERDR_BIN_PATH) { $env:HERDR_BIN_PATH } else { 'herdr' }

function Strip-Verbatim([string]$p) {
    if ($p -and $p.StartsWith('\\?\')) { return $p.Substring(4) }
    return $p
}
$PluginRoot = Strip-Verbatim (Split-Path -Parent $PSScriptRoot)
$ViewerBin = Join-Path $PluginRoot 'target\release\herdr-file-viewer.exe'

# Root the tree at the focused pane's cwd (the user's work pane). `pane list` prints JSON by default.
function Get-UserCwd {
    try {
        $focused = (& $HerdrBin pane list | ConvertFrom-Json).result.panes |
            Where-Object { $_.focused } | Select-Object -First 1
        if ($focused -and $focused.cwd) { return Strip-Verbatim $focused.cwd }
    } catch {}
    return $PluginRoot
}

function Get-PaneId([string]$json) {
    return ([regex]'"pane_id":"([^"]+)"').Match($json).Groups[1].Value
}

# The plugin's config directory, to pass as HERDR_PLUGIN_CONFIG_DIR -- herdr injects it only into a
# pane IT spawns from the manifest; these launchers spawn the viewer themselves. Empty on failure.
function Get-ConfigDir {
    try {
        $d = (& $HerdrBin plugin config-dir herdr-file-viewer | Out-String).Trim()
        if ($d) { return Strip-Verbatim $d }
    } catch {}
    return ''
}

function Open-Overlay {
    $cwd = Get-UserCwd
    $splitArgs = @('pane', 'split', '--direction', 'right', '--cwd', $cwd, '--focus')
    $cfg = Get-ConfigDir
    if ($cfg) { $splitArgs += @('--env', "HERDR_PLUGIN_CONFIG_DIR=$cfg") }
    $out = (& $HerdrBin @splitArgs | Out-String)
    $np = Get-PaneId $out
    if ($np) {
        # Call operator + quoted absolute path so a spaced install path still launches -- see
        # open-file-viewer.ps1's Open-Pane for the full why. (GH #58 -- confirmed live on Windows.)
        & $HerdrBin pane run $np "& \`"$ViewerBin\`""
        # Label it so a later invocation's launch-decision recognises it (best-effort).
        & $HerdrBin pane rename $np Files *> $null
        # Zoom the tab onto it: that is what makes the split an overlay.
        & $HerdrBin pane zoom $np --on *> $null
    }
    exit 0
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
    # No `--off` here (see the header).
    $PaneId = $Decision.Substring(6)
    & $HerdrBin pane zoom $PaneId --on
    exit $LASTEXITCODE
} elseif ($Decision -like 'CLOSE *') {
    $PaneId = $Decision.Substring(6)
    & $HerdrBin pane close $PaneId
    exit $LASTEXITCODE
} else {
    Open-Overlay
}
