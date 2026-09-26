<#
    Builds the single-file Rhevia, the one that works on a machine with
    nothing installed on it.

    Everything Rhevia does itself is native. Media playback is the one
    exception: it drives ffmpeg to decode whatever someone drops on it. That
    is fine on a machine where ffmpeg is installed and useless on one where
    it is not -- and "install ffmpeg first" is not an answer for someone who
    has been handed a file and told it works.

    So this build carries its own copy inside the executable and lays it out
    on first run. The copy is LGPL, is run as a separate program rather than
    linked, and is carried unmodified with its licence beside it.

    Usage:
        .\pack.ps1
#>
[CmdletBinding()]
param([string]$Out)

$ErrorActionPreference = "Stop"

$root = if ($PSScriptRoot) { $PSScriptRoot } else { Split-Path -Parent $MyInvocation.MyCommand.Path }
$desktop = Join-Path $root "../desktop" | Resolve-Path
$vendor = Join-Path $desktop "vendor/ffmpeg"
if (-not $Out) { $Out = Join-Path $root "../dist" }

if (-not (Test-Path (Join-Path $vendor "tools.xz"))) {
    Write-Host "The ffmpeg to carry is not here yet. Fetching it." -ForegroundColor Yellow
    python (Join-Path $root "fetch-ffmpeg.py")
    if ($LASTEXITCODE -ne 0) { throw "could not fetch ffmpeg" }
}

# A running copy holds its own file open, and the build then fails partway
# with a permission error that looks like something else entirely.
$names = @("rhevia-studio")
$running = Get-Process -Name $names -ErrorAction SilentlyContinue
if ($running) {
    Write-Host "  stopping $($running.Count) running instance(s) first" -ForegroundColor Yellow
    $running | Stop-Process -Force
    $waited = 0
    while ((Get-Process -Name $names -ErrorAction SilentlyContinue) -and $waited -lt 100) {
        Start-Sleep -Milliseconds 100
        $waited++
    }
}

Write-Host "Building the single-file Rhevia."
Push-Location $desktop
try {
    cargo build --release -p rhevia-studio --features packed
    if ($LASTEXITCODE -ne 0) { throw "the build failed" }
} finally {
    Pop-Location
}

New-Item -ItemType Directory -Force -Path $Out | Out-Null
$built = Join-Path $desktop "target/release/rhevia-studio.exe"
$packed = Join-Path $Out "Rhevia-Studio.exe"

# Retried rather than slept past: Windows can hold an image open for a moment
# after the process that ran it has gone.
$copied = $false
for ($try = 0; $try -lt 40 -and -not $copied; $try++) {
    try { Copy-Item $built $packed -Force -ErrorAction Stop; $copied = $true }
    catch { Start-Sleep -Milliseconds 250 }
}
if (-not $copied) { throw "could not copy the build; something still has it open" }

# The licence travels with it, for anyone who receives the file on its own.
Copy-Item (Join-Path $vendor "LICENSE.txt") (Join-Path $Out "ffmpeg-LICENSE.txt") -Force
$manifest = Get-Content (Join-Path $vendor "manifest.json") -Raw | ConvertFrom-Json

$size = [math]::Round((Get-Item $packed).Length / 1MB, 1)
Write-Host ""
Write-Host "Packed: $packed  ($size MB)" -ForegroundColor Green
Write-Host "  carries $($manifest.release) ($($manifest.licence))"
Write-Host "  unpacks to %LOCALAPPDATA%\Rhevia\runtime on first run"
Write-Host ""
Write-Host "One file. Copy it anywhere and run it -- nothing else needs installing."
Write-Host "NDI inputs and VST3 plugins still need their own runtimes; everything"
Write-Host "else, including media files, works from this alone."
