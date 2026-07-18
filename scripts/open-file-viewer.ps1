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
# PANE SHELLS: herdr types `pane run` text into the NEW pane's interactive shell
# (`terminal.default_shell`, else PowerShell on Windows). PowerShell needs the call operator
# (`& "path"`); Git Bash/zsh need a plain quoted path (`"path"`). We read default_shell from
# %APPDATA%\herdr\config.toml and emit the matching form. A short %USERPROFILE%\bin\hfv.exe shim
# keeps typing fast; assets/markdown-style.json is mirrored beside it so glow still finds its
# style when current_exe() is the shim (otherwise headings fall back to raw `##`).
#
# SPLIT SIDE: herdr only accepts `pane split --direction right|down`. We split right, then
# `pane swap --direction left` so the Files pane opens on the left of the work pane.
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

# pane run types into the shell; long absolute plugin paths are slow to echo. Keep a short shim
# at %USERPROFILE%\bin\hfv.exe so we only need to type a short command.
#
# Also mirror assets/markdown-style.json next to the shim. The viewer resolves glow's `-s`
# style by walking ancestors of current_exe(); from ...\bin\hfv.exe that only finds
# ...\bin\assets\…. Without it, glow falls back to built-in `dark`, which leaves `##` markers
# on headings in the rendered markdown preview.
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

# Invoke the shim with a form the pane shell understands. PowerShell needs `& "path"` (GH #58);
# Git Bash/zsh need a plain quoted path (the call operator is a parse error there).
function Invoke-ViewerInPane([string]$paneId, [string]$shimPath) {
    if (Test-UnixLikeShell (Get-HerdrDefaultShell)) {
        $fwd = ($shimPath -replace '\\', '/')
        & $HerdrBin pane run $paneId "`"$fwd`""
    } else {
        # Call-operator + quoted path; `\"` survives Windows PowerShell 5.1 native-arg stripping.
        & $HerdrBin pane run $paneId "& \`"$shimPath\`""
    }
}

# The directory to root the tree at: the focused pane's cwd (the user's work pane) at invocation
# time. Prefer HERDR_ACTIVE_PANE_CWD (injected when the keybinding fires), then pane list.
# `pane list` prints JSON by default (it rejects a `--json` flag).
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

# Extract the first `pane_id` from a herdr CLI JSON reply.
function Get-PaneId([string]$json) {
    return ([regex]'"pane_id":"([^"]+)"').Match($json).Groups[1].Value
}

function Open-Pane {
    $cwd = Get-UserCwd
    # herdr pane split only accepts right|down. Split right, then swap left so Files lands on the left.
    $out = (& $HerdrBin pane split --direction right --cwd $cwd --focus | Out-String)
    $np = Get-PaneId $out
    if ($np) {
        & $HerdrBin pane swap --direction left --pane $np *> $null
        $shim = Ensure-ViewerShim
        if (-not $shim) { $shim = $ViewerBin }
        Invoke-ViewerInPane $np $shim
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
