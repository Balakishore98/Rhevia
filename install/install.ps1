<#
    Installs Rhevia for the current user.

    No admin rights required: everything goes under %LOCALAPPDATA%, which is
    also where a live-production tool belongs — a show should never depend on
    someone being able to elevate.

    Usage:
        .\install.ps1
        .\install.ps1 -Uninstall
#>
[CmdletBinding()]
param(
    [string]$Source,
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"

# $PSScriptRoot is not populated while parameter defaults are evaluated, so the
# fallback is resolved here rather than in the param block.
if (-not $Source) {
    $root = if ($PSScriptRoot) {
        $PSScriptRoot
    } else {
        Split-Path -Parent $MyInvocation.MyCommand.Path
    }
    $Source = Join-Path $root "../desktop/target/release"
}

$AppName   = "Rhevia Studio"
$InstallTo = Join-Path $env:LOCALAPPDATA "Programs\Rhevia"
$StartMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
$Shortcut  = Join-Path $StartMenu "$AppName.lnk"

if ($Uninstall) {
    if (Test-Path $Shortcut)  { Remove-Item $Shortcut -Force }
    if (Test-Path $InstallTo) { Remove-Item $InstallTo -Recurse -Force }
    Write-Host "Rhevia removed." -ForegroundColor Green
    return
}

# rhevia-vst3-validate is not optional. The studio runs it as a separate
# process to check each plugin before offering it, so that a plugin which
# faults takes down the validator rather than the show. Without it beside the
# studio, every plugin reads as "not checked" and none can be used.
$binaries = @(
    "rhevia-studio.exe",
    "rhevia-vst3-validate.exe",
    "rhevia-relay.exe",
    "rhevia-stream.exe"
)
foreach ($exe in $binaries) {
    if (-not (Test-Path (Join-Path $Source $exe))) {
        throw "$exe not found in $Source. Build first: cargo build --release"
    }
}

# A running copy holds its own file open, and the copy below then fails
# halfway through — leaving some binaries updated and others not, which is
# worse than not installing at all.
$running = Get-Process -Name "rhevia-studio","rhevia-relay","rhevia-stream" -ErrorAction SilentlyContinue
if ($running) {
    Write-Host "  stopping $($running.Count) running instance(s) first" -ForegroundColor Yellow
    $running | ForEach-Object {
        $_.CloseMainWindow() | Out-Null
    }
    Start-Sleep -Seconds 2
    Get-Process -Name "rhevia-studio","rhevia-relay","rhevia-stream" -ErrorAction SilentlyContinue |
        Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 1
}

New-Item -ItemType Directory -Force -Path $InstallTo | Out-Null
foreach ($exe in $binaries) {
    Copy-Item (Join-Path $Source $exe) $InstallTo -Force
    $size = [math]::Round((Get-Item (Join-Path $InstallTo $exe)).Length / 1MB, 1)
    Write-Host "  installed $exe ($size MB)"
}

# Start Menu entry.
$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut($Shortcut)
$link.TargetPath       = Join-Path $InstallTo "rhevia-studio.exe"
$link.WorkingDirectory = $InstallTo
$link.Description      = "Rhevia Studio - live production switcher"
$link.Save()

# The CLI tools are only useful from a terminal, so put them on PATH.
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$InstallTo*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$InstallTo", "User")
    Write-Host "  added to PATH (restart your terminal to pick it up)"
}

Write-Host ""
Write-Host "Rhevia installed to $InstallTo" -ForegroundColor Green
Write-Host "  Start Menu : $AppName"
Write-Host "  CLI        : rhevia-relay, rhevia-stream"

# What is present decides which optional inputs work. Reported here rather
# than discovered later in the middle of setting up a show.
Write-Host ""
Write-Host "Optional runtimes:"

$ffmpeg = Get-Command ffmpeg -ErrorAction SilentlyContinue
if ($ffmpeg) {
    Write-Host "  media files   yes  ($($ffmpeg.Source))" -ForegroundColor Green
} else {
    Write-Host "  media files   no   - install ffmpeg to play video and audio files" -ForegroundColor Yellow
}

$ndiDir = $env:NDI_RUNTIME_DIR_V6
if (-not $ndiDir) { $ndiDir = $env:NDI_RUNTIME_DIR_V5 }
if ($ndiDir -and (Test-Path (Join-Path $ndiDir "Processing.NDI.Lib.x64.dll"))) {
    Write-Host "  NDI           yes  ($ndiDir)" -ForegroundColor Green
} else {
    Write-Host "  NDI           no   - install NDI Tools from ndi.video for network video" -ForegroundColor Yellow
}

$vst3 = Join-Path $env:CommonProgramFiles "VST3"
if (Test-Path $vst3) {
    $count = (Get-ChildItem $vst3 -Filter *.vst3 -ErrorAction SilentlyContinue).Count
    Write-Host "  VST3 plugins  $count found in $vst3" -ForegroundColor Green
} else {
    Write-Host "  VST3 plugins  none - nothing installed in $vst3" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "Everything else - capture, mixing, encoding, RTMP and SRT - needs none of these."
Write-Host ""
Write-Host "Uninstall with: .\install.ps1 -Uninstall"
