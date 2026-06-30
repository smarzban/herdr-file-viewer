# fetch-or-build.ps1 -- herdr [[build]] step for herdr-file-viewer (Windows).
#
# Windows PowerShell 5.1 (Desktop) compatible -- the in-box shell on Windows 10/11/Server, so
# installing the viewer needs no extra tooling. The Windows sibling of fetch-or-build.sh: same
# behaviour, different shell.
#
# Fast path: download the prebuilt binary that matches THIS source's declared version + platform
# from the GitHub release, verify its SHA-256, and install it at target\release\herdr-file-viewer.exe.
# The match is by VERSION, not by exact commit: a checkout that is AHEAD of the matching release
# (e.g. main has merged work that isn't tagged yet) still uses that released binary, so landing a
# PR no longer forces new users to compile while a release is pending. Integrity is unchanged -- the
# binary is still SHA-256 verified -- and a version with no published release still 404s to source.
# Fallback: on ANY miss (no asset for this version, network/download error, checksum mismatch,
# unmapped platform, no cargo) print a clear notice and build from source with cargo -- identical to
# the pre-prebuilt behavior, so installing never gets harder than before.
#
# Paths and the release base URL are overridable via env (FV_REPO_ROOT / FV_CARGO_TOML / FV_OUT /
# FV_BASE_URL) so this logic is exercised by a hermetic test with a mocked Invoke-WebRequest and a
# stubbed cargo (mirrors fetch-or-build.sh's PATH-stubbed curl/cargo).
#
# v1 targets x86_64-pc-windows-msvc only (no aarch64 Windows -- AC-N4); no code-signing (AC-N5);
# no Windows renderer-install (AC-N6).

$ErrorActionPreference = 'Stop'

$Repo = 'smarzban/herdr-file-viewer'

$RepoRoot = if ($env:FV_REPO_ROOT) { $env:FV_REPO_ROOT } else { Join-Path $PSScriptRoot '..' }
$CargoToml = if ($env:FV_CARGO_TOML) { $env:FV_CARGO_TOML } else { Join-Path $RepoRoot 'Cargo.toml' }
$Out = if ($env:FV_OUT) { $env:FV_OUT } else { Join-Path $RepoRoot 'target\release\herdr-file-viewer.exe' }
$BaseUrl = if ($env:FV_BASE_URL) { $env:FV_BASE_URL } else { "https://github.com/$Repo/releases/download" }

# Build from source -- the original, unconditional behavior. A missing cargo gets a clear message
# pointing at rustup, exactly like the sh script's fallback.
function Build-FromSource {
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargo) {
        [Console]::Error.WriteLine("herdr-file-viewer needs Rust 1.96+ to build, but cargo was not found. Install Rust from https://rustup.rs then re-run: herdr plugin install $Repo")
        exit 1
    }
    & cargo build --release
    exit $LASTEXITCODE
}

function Invoke-FvFallback {
    param([string]$Reason)
    [Console]::Error.WriteLine("herdr-file-viewer: $Reason - building from source instead.")
    if ($script:TmpDir -and (Test-Path $script:TmpDir)) {
        Remove-Item -Recurse -Force $script:TmpDir -ErrorAction SilentlyContinue
    }
    Build-FromSource
}

# Hex SHA-256 of a file. Get-FileHash is primary; certutil is the fallback (hedges the
# constrained-environment Get-FileHash gap herdr's own installer hit), mirroring the sh script's
# sha256sum/shasum preference order. $null on total failure (no usable tool / unreadable file).
function Get-FvSha256Hex {
    param([string]$Path)
    try {
        return (Get-FileHash -Algorithm SHA256 -Path $Path -ErrorAction Stop).Hash.ToLowerInvariant()
    } catch {
        try {
            $out = & certutil -hashfile $Path SHA256 2>$null
        } catch {
            return $null
        }
        if ($LASTEXITCODE -ne 0 -or -not $out) { return $null }
        $hashLine = $out | Where-Object { $_ -match '^[0-9a-fA-F]{64}$' } | Select-Object -First 1
        if ($hashLine) { return $hashLine.Trim().ToLowerInvariant() }
        return $null
    }
}

# A thin Invoke-WebRequest wrapper -- the hermetic test shadows Invoke-WebRequest itself (a
# PowerShell function in the calling scope takes precedence over a cmdlet of the same name, the
# same mechanism Pester's mocking uses), so this wrapper needs no extra seam beyond that.
function Invoke-FvDownload {
    param([string]$Url, [string]$Dest)
    try {
        Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing -ErrorAction Stop
        return $true
    } catch {
        return $false
    }
}

# --- resolve the target triple from the platform --------------------------------------------
$Arch = $env:PROCESSOR_ARCHITECTURE
$Triple = $null
if ($Arch -eq 'AMD64') { $Triple = 'x86_64-pc-windows-msvc' }
if (-not $Triple) { Invoke-FvFallback "no prebuilt binary for Windows/$Arch" }

# --- read the version this source declares ---------------------------------------------------
$Version = $null
if (Test-Path $CargoToml) {
    $match = Select-String -Path $CargoToml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
    if ($match) { $Version = $match.Matches[0].Groups[1].Value }
}
if (-not $Version) { Invoke-FvFallback "could not read version from $CargoToml" }

$Asset = "herdr-file-viewer-$Triple.exe"

$script:TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("fv-fob-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $script:TmpDir -Force | Out-Null

# --- version-only match + transparency "ahead" note (best-effort, never a failure) -----------
# For transparency only: if this is a git work tree and we can read both HEAD and the release's
# published COMMIT marker, note when the checkout is ahead -- the binary is the released
# v$Version while the working tree may carry newer, unreleased source. Wrapped so any failure
# here (git absent, no network) never blocks the main install flow.
$AheadNote = ''
try {
    $git = Get-Command git -ErrorAction SilentlyContinue
    if ($git) {
        & git -C $RepoRoot rev-parse --is-inside-work-tree *> $null
        if ($LASTEXITCODE -eq 0) {
            $headRev = (& git -C $RepoRoot rev-parse HEAD 2>$null)
            if (-not $headRev) { $headRev = 'nohead' }
            $commitFile = Join-Path $script:TmpDir 'COMMIT'
            if (Invoke-FvDownload "$BaseUrl/v$Version/COMMIT" $commitFile) {
                $releaseCommit = (Get-Content $commitFile -Raw -ErrorAction SilentlyContinue)
                if ($releaseCommit) { $releaseCommit = $releaseCommit.Trim() }
                if ($releaseCommit -and ($headRev -ne $releaseCommit)) {
                    $AheadNote = " Note: this checkout ($headRev) is ahead of the v$Version release commit ($releaseCommit), so newer unreleased source is not in this binary."
                }
            }
        }
    }
} catch {
    # Best-effort only -- never block the install on a transparency note.
}

$BinUrl = "$BaseUrl/v$Version/$Asset"
$SumsUrl = "$BaseUrl/v$Version/SHA256SUMS"
$TmpBin = Join-Path $script:TmpDir $Asset
$TmpSums = Join-Path $script:TmpDir 'SHA256SUMS'

if (-not (Invoke-FvDownload $BinUrl $TmpBin)) { Invoke-FvFallback "prebuilt binary not available for v$Version ($Asset)" }
if (-not (Invoke-FvDownload $SumsUrl $TmpSums)) { Invoke-FvFallback "checksums not available for v$Version" }

# Expected hash = the SHA256SUMS line for our asset filename.
$Expected = $null
$assetPattern = [regex]::Escape($Asset)
# The separator before the filename is `  ` (two spaces, coreutils TEXT mode) on Linux/macOS, but
# ` *` (one space + `*`, BINARY mode) when the line is produced by Git-for-Windows `sha256sum` on
# the release runner — accept either marker (`[ *]`), or every Windows install would miss the
# prebuilt and fall back to a source build (defeats AC-11/12/13).
Get-Content $TmpSums | ForEach-Object {
    if (-not $Expected -and $_ -match "^([0-9a-fA-F]{64}) [ *]${assetPattern}`$") {
        $Expected = $Matches[1].ToLowerInvariant()
    }
}
if (-not $Expected) { Invoke-FvFallback "no checksum listed for $Asset" }

$Actual = Get-FvSha256Hex $TmpBin
if (-not $Actual) { Invoke-FvFallback 'no SHA-256 tool (Get-FileHash/certutil) available' }
if ($Actual -ne $Expected) { Invoke-FvFallback "checksum mismatch for $Asset (expected $Expected, got $Actual)" }

# Verified -- move it into place.
$OutDir = Split-Path -Parent $Out
if ($OutDir -and -not (Test-Path $OutDir)) {
    New-Item -ItemType Directory -Path $OutDir -Force | Out-Null
}
Move-Item -Force $TmpBin $Out
[Console]::Out.WriteLine("herdr-file-viewer: installed prebuilt v$Version ($Triple), verified SHA-256.$AheadNote")
Remove-Item -Recurse -Force $script:TmpDir -ErrorAction SilentlyContinue
exit 0
