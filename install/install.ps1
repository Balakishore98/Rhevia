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

$binaries = @("rhevia-studio.exe", "rhevia-relay.exe", "rhevia-stream.exe")
foreach ($exe in $binaries) {
    if (-not (Test-Path (Join-Path $Source $exe))) {
        throw "$exe not found in $Source. Build first: cargo build --release"
    }
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
Write-Host ""
Write-Host "Uninstall with: .\install.ps1 -Uninstall"
