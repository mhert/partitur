# Install the pinned WiX Toolset that builds Partitur's MSI, on Windows.
#
# Usage: scripts\install-wix.ps1
#
# It installs `wix` 7.0.1 as a global dotnet tool and the WixToolset.UI.wixext 7.0.1 extension
# (the stock install dialogs) into WiX's global extension cache, then checks that `wix --version`
# starts with 7.0.1. It is idempotent: a tool or extension already at the pinned version is left
# as it is, and a wix of another version is replaced by the pinned one. The dotnet tools folder
# is put on PATH for this process, and on a GitHub runner also for the job's later steps. It
# needs the .NET SDK on PATH, as GitHub's Windows runners have it. Exits with status 1 on the
# first step that fails.
$ErrorActionPreference = 'Stop'

$version = '7.0.1'

function Fail([string]$Message) {
    [Console]::Error.WriteLine("install-wix: $Message")
    exit 1
}

# Global dotnet tools land here, and a fresh install does not put it on this process's PATH.
$tools = Join-Path $env:USERPROFILE '.dotnet\tools'
if (-not (($env:PATH -split ';') -contains $tools)) {
    $env:PATH = "$tools;$env:PATH"
}
if ($env:GITHUB_PATH) {
    Add-Content -LiteralPath $env:GITHUB_PATH -Value $tools
}

if (-not (Get-Command dotnet -ErrorAction SilentlyContinue)) {
    Fail 'dotnet is not on PATH; the .NET SDK is needed to install wix'
}

# `dotnet tool list -g` prints one row per tool: package id, version, commands.
$listed = (& dotnet tool list --global) -join "`n"
if ($LASTEXITCODE) { Fail "dotnet tool list --global exited with status $LASTEXITCODE" }
$row = ($listed -split "`n") | Where-Object { $_ -match '^\s*wix\s' } | Select-Object -First 1
if ($row -and $row -match "^\s*wix\s+$([regex]::Escape($version))\s") {
    Write-Output "install-wix: wix $version is already installed"
} else {
    if ($row) {
        & dotnet tool uninstall --global wix
        if ($LASTEXITCODE) { Fail "dotnet tool uninstall --global wix exited with status $LASTEXITCODE" }
    }
    & dotnet tool install --global wix --version $version
    if ($LASTEXITCODE) { Fail "dotnet tool install --global wix --version $version exited with status $LASTEXITCODE" }
}

if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
    Fail "wix is not on PATH after the install (looked in $tools too)"
}
$reported = ((& wix --version) -join '').Trim()
if ($LASTEXITCODE) { Fail "wix --version exited with status $LASTEXITCODE" }
if (-not $reported.StartsWith($version)) {
    Fail "wix --version reports '$reported', expected $version"
}

$extension = "WixToolset.UI.wixext"
$extensions = (& wix extension list --global) -join "`n"
if ($LASTEXITCODE) { Fail "wix extension list --global exited with status $LASTEXITCODE" }
# Printed so the log shows what the check below matched against.
Write-Output '--- wix extension list --global ---'
Write-Output $extensions
Write-Output '--- end of list ---'
if ($extensions -match "$([regex]::Escape($extension))\s+$([regex]::Escape($version))") {
    Write-Output "install-wix: $extension $version is already installed"
} else {
    & wix extension add --global "$extension/$version"
    if ($LASTEXITCODE) { Fail "wix extension add --global $extension/$version exited with status $LASTEXITCODE" }
}

Write-Output "install-wix: wix $reported with $extension $version"
exit 0
