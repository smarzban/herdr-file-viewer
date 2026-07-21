# open-file-viewer-tab.ps1 -- Windows sibling of scripts/open-file-viewer-tab.sh.
#
# Idempotent launcher for the file viewer in its own TAB -- used by the `open-file-viewer-tab-windows`
# action and a herdr keybinding (e.g. `prefix+shift+f`). "Open-or-switch, toggle on repeat",
# scoped across the tabs of the CURRENT WORKSPACE:
#   - no Files pane in this workspace         -> open the viewer in a new tab (focused)
#   - a Files pane in another tab of this
#     workspace                               -> switch to that tab (no duplicate viewer)
#   - a Files pane is in the current tab,
#     but not focused                         -> focus it in place
#   - the focused pane IS the Files pane      -> close it ("toggle off"; herdr auto-closes the
#                                               now-empty tab -- verified)
# A viewer open in a DIFFERENT workspace is left alone and a fresh one opens here (never switches
# you across workspaces).
#
# Sibling of scripts/open-file-viewer.ps1 (the split-pane variant) -- see its header for WHY the
# Windows launchers spawn the viewer by ABSOLUTE PATH (`tab create` + `pane run`) instead of
# `plugin pane open --entrypoint`, and for pane-shell / shim / glow-style notes. The
# OPEN/SWITCHTAB/FOCUS/CLOSE decision is computed in-process by the viewer binary
# (`--launch-decision-tab`, fed `pane list` JSON on stdin) and is unit-tested (src/launch.rs).
# Any failure degrades to OPEN (a fresh viewer tab).

$ErrorActionPreference = 'Continue'

# PowerShell 5.1 otherwise decodes herdr's UTF-8 JSON with the legacy console code page;
# non-ASCII pane titles or paths can corrupt the JSON and trigger the plugin-root fallback.
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[Console]::OutputEncoding = $Utf8NoBom
$OutputEncoding = $Utf8NoBom

$HerdrBin = if ($env:HERDR_BIN_PATH) { $env:HERDR_BIN_PATH } else { 'herdr' }

function Strip-Verbatim([string]$p) {
    # Use -like (not StartsWith('\\?\')): editors that treat \' as an escape
    # misparse the EndsWith-backslash single-quoted form and report a bogus '}'.
    if ($p -like '\\?\*') { return $p.Substring(4) }
    return $p
}
$PluginRoot = Strip-Verbatim (Split-Path -Parent $PSScriptRoot)
$ViewerBin = Join-Path $PluginRoot 'target\release\herdr-file-viewer.exe'

function Get-HerdrDefaultShell {
    if ($env:APPDATA) {
        $cfg = Join-Path $env:APPDATA 'herdr\config.toml'
        if (Test-Path -LiteralPath $cfg) {
            foreach ($line in Get-Content -LiteralPath $cfg -ErrorAction SilentlyContinue) {
                if ($line -match '^\s*default_shell\s*=\s*"(.*)"\s*$') {
                    return $Matches[1].Trim()
                }
            }
        }
    }
    return 'powershell.exe'
}

function Test-UnixLikeShell([string]$shell) {
    if ([string]::IsNullOrWhiteSpace($shell)) { return $false }
    $leaf = [IO.Path]::GetFileNameWithoutExtension($shell).ToLowerInvariant()
    if ($leaf -in @('bash', 'zsh', 'sh', 'fish', 'dash', 'nu')) { return $true }
    if ($shell -match '(?i)[\\/](bash|zsh|sh)(\.exe)?$') { return $true }
    return $false
}

function Ensure-ViewerShim {
    $shimDir = Join-Path $env:USERPROFILE 'bin'
    $shim = Join-Path $shimDir 'hfv.exe'
    if (-not (Test-Path -LiteralPath $ViewerBin)) { return $null }
    New-Item -ItemType Directory -Force -Path $shimDir | Out-Null
    $needsCopy = $true
    if (Test-Path -LiteralPath $shim) {
        try {
            $src = Get-Item -LiteralPath $ViewerBin
            $dst = Get-Item -LiteralPath $shim
            if ($src.Length -eq $dst.Length -and $src.LastWriteTimeUtc -eq $dst.LastWriteTimeUtc) {
                $needsCopy = $false
            }
        } catch {}
    }
    if ($needsCopy) {
        Copy-Item -LiteralPath $ViewerBin -Destination $shim -Force
    }

    $styleSrc = Join-Path $PluginRoot 'assets\markdown-style.json'
    $styleDstDir = Join-Path $shimDir 'assets'
    $styleDst = Join-Path $styleDstDir 'markdown-style.json'
    if (Test-Path -LiteralPath $styleSrc) {
        $needsStyle = $true
        if (Test-Path -LiteralPath $styleDst) {
            try {
                $ss = Get-Item -LiteralPath $styleSrc
                $sd = Get-Item -LiteralPath $styleDst
                if ($ss.Length -eq $sd.Length -and $ss.LastWriteTimeUtc -eq $sd.LastWriteTimeUtc) {
                    $needsStyle = $false
                }
            } catch {}
        }
        if ($needsStyle) {
            New-Item -ItemType Directory -Force -Path $styleDstDir | Out-Null
            Copy-Item -LiteralPath $styleSrc -Destination $styleDst -Force
        }
    }
    return $shim
}

function Invoke-ViewerInPane([string]$paneId, [string]$shimPath) {
    if (Test-UnixLikeShell (Get-HerdrDefaultShell)) {
        $fwd = ($shimPath -replace '\\', '/')
        & $HerdrBin pane run $paneId "`"$fwd`""
    } else {
        & $HerdrBin pane run $paneId "& \`"$shimPath\`""
    }
}

# Root the tree at the focused pane's cwd (the user's work pane). Prefer HERDR_ACTIVE_PANE_CWD
# (injected when the keybinding fires), then pane list.
function Get-UserCwd {
    if ($env:HERDR_ACTIVE_PANE_CWD) {
        $fromEnv = Strip-Verbatim $env:HERDR_ACTIVE_PANE_CWD
        if ($fromEnv -and (Test-Path -LiteralPath $fromEnv -PathType Container)) {
            return $fromEnv
        }
    }
    try {
        $focused = (& $HerdrBin pane list | ConvertFrom-Json).result.panes |
            Where-Object { $_.focused } | Select-Object -First 1
        if ($focused -and $focused.cwd) {
            $fromPane = Strip-Verbatim $focused.cwd
            if ($fromPane -and (Test-Path -LiteralPath $fromPane -PathType Container)) {
                return $fromPane
            }
        }
    } catch {}
    return $PluginRoot
}

function Get-PaneId([string]$json) {
    return ([regex]'"pane_id":"([^"]+)"').Match($json).Groups[1].Value
}

function Open-Tab {
    $cwd = Get-UserCwd
    # `tab create` makes a new tab with a shell pane (its `root_pane`); run the viewer into it by
    # absolute path and label the pane "Files" so a later launch-decision recognises it.
    $out = (& $HerdrBin tab create --cwd $cwd --label Files --focus | Out-String)
    $np = Get-PaneId $out
    if ($np) {
        $shim = Ensure-ViewerShim
        if (-not $shim) { $shim = $ViewerBin }
        Invoke-ViewerInPane $np $shim
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
