# open-file-viewer.ps1 -- Windows sibling of scripts/open-file-viewer.sh.
#
# Idempotent launcher for the file viewer -- used by the `open-file-viewer-windows` action and a
# herdr keybinding. "Launch-or-focus, toggle on repeat", scoped to the current tab:
#   - no Files pane in the current tab      -> open a split (focused)
#   - a Files pane exists but isn't focused -> focus it
#   - the focused pane IS the Files pane    -> close it ("hide"; herdr has no hide-without-close,
#                                              and reopening just re-walks the tree -- cheap)
#
# WHY THIS DIVERGES FROM THE UNIX .sh (verified on real Windows, herdr 0.7.1-preview, GH #58):
# the unix launcher opens the viewer with `plugin pane open --entrypoint file-viewer`, letting herdr
# spawn the manifest's RELATIVE pane command. That does NOT work on Windows: herdr passes the
# relative program name to CreateProcessW, which resolves it against herdr's OWN directory (not any
# cwd we pass), so it fails with ERROR_PATH_NOT_FOUND (os error 3). herdr also stores the plugin
# root as a `\\?\` verbatim path. So on Windows we instead spawn the viewer BY ABSOLUTE PATH:
# `pane split` an empty pane, then `pane run` the absolute `.exe` into it, and `pane rename` it to
# "Files" so the toggle (below) can find it again. We root the tree at the user's focused-pane cwd
# (the viewer, launched via `pane run`, gets no HERDR_PLUGIN_CONTEXT_JSON, so it roots from its cwd).
#
# The OPEN/FOCUS/CLOSE decision is computed in-process by the viewer binary itself
# (`herdr-file-viewer.exe --launch-decision`, fed the `pane list` JSON on stdin) -- so it is
# unit-tested (src/launch.rs) and the pane id it returns is validated flag-safe. Any failure
# degrades to OPEN. herdr has no focus-by-id, so a focus is a `zoom <id> --on/--off` cycle.

$ErrorActionPreference = 'Continue'

# PowerShell 5.1 otherwise decodes herdr's UTF-8 JSON with the legacy console code page;
# non-ASCII pane titles or paths can corrupt the JSON and trigger the plugin-root fallback.
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[Console]::OutputEncoding = $Utf8NoBom
$OutputEncoding = $Utf8NoBom

$HerdrBin = if ($env:HERDR_BIN_PATH) { $env:HERDR_BIN_PATH } else { 'herdr' }

# Plugin root as a NORMAL absolute path (strip herdr's `\\?\` verbatim prefix). `$PSScriptRoot` is
# `<root>\scripts`, so the parent is the plugin root; the viewer binary is an ABSOLUTE path under it.
function Strip-Verbatim([string]$p) {
    if ($p -and $p.StartsWith('\\?\')) { return $p.Substring(4) }
    return $p
}
$PluginRoot = Strip-Verbatim (Split-Path -Parent $PSScriptRoot)
$ViewerBin = Join-Path $PluginRoot 'target\release\herdr-file-viewer.exe'

# The directory to root the tree at: the focused pane's cwd (the user's work pane) at invocation
# time. `pane list` prints JSON by default (it rejects a `--json` flag).
function Get-UserCwd {
    try {
        $focused = (& $HerdrBin pane list | ConvertFrom-Json).result.panes |
            Where-Object { $_.focused } | Select-Object -First 1
        if ($focused -and $focused.cwd) { return Strip-Verbatim $focused.cwd }
    } catch {}
    return $PluginRoot
}

# Extract the first `pane_id` from a herdr CLI JSON reply.
function Get-PaneId([string]$json) {
    return ([regex]'"pane_id":"([^"]+)"').Match($json).Groups[1].Value
}

# The plugin's config directory, to pass as HERDR_PLUGIN_CONFIG_DIR. herdr injects that variable
# only into a pane IT spawns from the manifest, and these launchers spawn the viewer themselves
# (see the note at the top). Without it the viewer falls back to $XDG_CONFIG_HOME / $HOME, neither
# of which Windows sets, lands on a RELATIVE path, and refuses to read it — so config.toml was
# silently ignored on Windows. Empty on failure: the launch proceeds with built-in defaults.
function Get-ConfigDir {
    try {
        $d = (& $HerdrBin plugin config-dir herdr-file-viewer | Out-String).Trim()
        if ($d) { return Strip-Verbatim $d }
    } catch {}
    return ''
}

# The `open_direction` config key, read through the binary's `--open-direction` probe (the shell
# cannot parse TOML). Anything unexpected — binary missing, probe failure, a future value —
# degrades to `right`, today's layout. HERDR_PLUGIN_CONFIG_DIR is passed for the same reason
# Open-Pane passes it: without it the probe cannot find config.toml on Windows and would always
# answer the default.
function Get-OpenDirection {
    try {
        $cfg = Get-ConfigDir
        if ($cfg) { $env:HERDR_PLUGIN_CONFIG_DIR = $cfg }
        $d = (& $ViewerBin --open-direction 2>$null | Out-String).Trim()
        if ($d -eq 'down') { return 'down' }
    } catch {}
    return 'right'
}

function Open-Pane {
    $cwd = Get-UserCwd
    $direction = Get-OpenDirection
    $splitArgs = @('pane', 'split', '--direction', $direction, '--cwd', $cwd, '--focus')
    $cfg = Get-ConfigDir
    if ($cfg) { $splitArgs += @('--env', "HERDR_PLUGIN_CONFIG_DIR=$cfg") }
    $out = (& $HerdrBin @splitArgs | Out-String)
    $np = Get-PaneId $out
    if ($np) {
        # Run the viewer by ABSOLUTE path via the PowerShell CALL OPERATOR. herdr types <command>
        # into the pane's shell (PowerShell on Windows); a bare or plain-quoted path splits on a
        # space in the install path (e.g. C:\Users\First Last\...) and the viewer never starts.
        # `& "<path>"` executes it; the `\"` escaping survives Windows PowerShell 5.1's native-arg
        # quote-stripping so herdr receives the quotes intact. (GH #58 — confirmed live on Windows.)
        & $HerdrBin pane run $np "& \`"$ViewerBin\`""
        # Label it so a later invocation's launch-decision recognises it (best-effort).
        & $HerdrBin pane rename $np Files *> $null
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
