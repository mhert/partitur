# Install, uninstall and inspect Partitur's MSI on Windows, for the MSI tests and the release
# workflow.
#
# Usage: Import-Module scripts\windows-install.psm1
#
# Windows Installer lists an installed MSI in Settings > Apps under a key named by its product
# code, in the 64-bit view of HKLM\Software\Microsoft\Windows\CurrentVersion\Uninstall. Every
# function here finds Partitur's install through that entry: Get-PartiturEntry returns it,
# Install-PartiturMsi and Uninstall-PartiturMsi run msiexec silently and hand back its exit code
# (0 and 3010, success with a restart pending, are success; the callers decide), and
# Assert-PartiturInstalled and Assert-PartiturRemoved check the entry, the three named files and
# libcef.dll and adapter\node\node.exe under Program Files\Partitur, the all-users Start menu shortcut and the HKLM\Software\Partitur
# value that the shortcut's component is keyed by. The registry is opened in its
# 64-bit view explicitly, so the answers are the same from a 32-bit PowerShell. The functions
# throw with a message saying what is wrong; none of them changes anything but through msiexec.
# They need an elevated session, as GitHub's Windows runners are.
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$uninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall'
# The key path of the shortcut's component (packaging\windows\partitur.wxs).
$shortcutKey = 'Software\Partitur'
$shortcutValue = 'StartMenuShortcut'

function Open-Hklm64 {
    [Microsoft.Win32.RegistryKey]::OpenBaseKey(
        [Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
}

# The three named files, two that stand for CEF's runtime and the adapter in its subfolder, and
# the shortcut a Partitur install puts on the machine.
function Get-PartiturPaths {
    $folder = Join-Path $env:ProgramFiles 'Partitur'
    @(
        (Join-Path $folder 'partitur.exe'),
        (Join-Path $folder 'LICENSE'),
        (Join-Path $folder 'README.md'),
        (Join-Path $folder 'libcef.dll'),
        (Join-Path $folder 'adapter\node\node.exe'),
        (Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\Partitur.lnk')
    )
}

# A writable log path: the one given, or a fresh one in the temp folder.
function Resolve-MsiLog([string]$Log, [string]$Kind) {
    if ($Log) {
        return [IO.Path]::GetFullPath($Log, (Get-Location).ProviderPath)
    }
    Join-Path ([IO.Path]::GetTempPath()) ("partitur-msi-$Kind-" + [Guid]::NewGuid() + '.log')
}

# Runs msiexec with $Arguments and returns its exit code. Another install still finishing
# (1618, ERROR_INSTALL_ALREADY_RUNNING) is waited out for up to a minute.
function Invoke-Msiexec([string]$Arguments) {
    for ($attempt = 1; ; $attempt++) {
        $process = Start-Process -FilePath 'msiexec.exe' -ArgumentList $Arguments -Wait -PassThru
        if ($process.ExitCode -ne 1618 -or $attempt -ge 12) {
            return $process.ExitCode
        }
        Start-Sleep -Seconds 5
    }
}

<#
.SYNOPSIS
Returns the Settings > Apps entry of the installed Partitur MSI, or $null when there is none.
.DESCRIPTION
The entry is the 64-bit HKLM uninstall key, named by a product code, whose DisplayName is
Partitur. It comes back with ProductCode (the key's name), DisplayVersion, Publisher,
InstallLocation, NoModify and NoRepair. Keys not named by a product code (the NSIS install's
mhert.partitur) are not MSI installs and are skipped. Throws if more than one entry is found.
#>
function Get-PartiturEntry {
    $base = Open-Hklm64
    $found = @()
    try {
        $list = $base.OpenSubKey($uninstallKey)
        if ($null -eq $list) { return $null }
        try {
            foreach ($name in $list.GetSubKeyNames()) {
                if ($name -notmatch '^\{[0-9A-Fa-f-]{36}\}$') { continue }
                $key = $list.OpenSubKey($name)
                if ($null -eq $key) { continue }
                try {
                    if ($key.GetValue('DisplayName') -ceq 'Partitur') {
                        $found += [pscustomobject]@{
                            ProductCode     = $name
                            DisplayVersion  = $key.GetValue('DisplayVersion')
                            Publisher       = $key.GetValue('Publisher')
                            InstallLocation = $key.GetValue('InstallLocation')
                            NoModify        = $key.GetValue('NoModify')
                            NoRepair        = $key.GetValue('NoRepair')
                        }
                    }
                } finally { $key.Dispose() }
            }
        } finally { $list.Dispose() }
    } finally { $base.Dispose() }

    if ($found.Count -gt 1) {
        throw "Partitur is installed $($found.Count) times: $(($found | ForEach-Object { "$($_.ProductCode) $($_.DisplayVersion)" }) -join ', ')"
    }
    if ($found.Count -eq 0) { return $null }
    $found[0]
}

<#
.SYNOPSIS
Installs an MSI silently and returns msiexec's exit code.
.DESCRIPTION
Runs msiexec.exe /i <Path> /qn /norestart /l*v <Log>. The log goes to -Log, or to a fresh file
in the temp folder. 0 and 3010 are success.
#>
function Install-PartiturMsi {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [string]$Log = ''
    )
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Path is not a file"
    }
    $msi = (Resolve-Path -LiteralPath $Path).ProviderPath
    $logPath = Resolve-MsiLog $Log 'install'
    Invoke-Msiexec "/i `"$msi`" /qn /norestart /l*v `"$logPath`""
}

<#
.SYNOPSIS
Uninstalls the installed Partitur MSI silently and returns msiexec's exit code.
.DESCRIPTION
Runs msiexec.exe /x <ProductCode> /qn /norestart /l*v <Log> for the entry Get-PartiturEntry
finds. The log goes to -Log, or to a fresh file in the temp folder. Throws if Partitur is not
installed.
#>
function Uninstall-PartiturMsi {
    param([string]$Log = '')
    $entry = Get-PartiturEntry
    if ($null -eq $entry) {
        throw 'Partitur is not installed: there is no uninstall entry to remove'
    }
    $logPath = Resolve-MsiLog $Log 'uninstall'
    Invoke-Msiexec "/x $($entry.ProductCode) /qn /norestart /l*v `"$logPath`""
}

<#
.SYNOPSIS
Throws unless Partitur <Version> is installed as the MSI installs it.
.DESCRIPTION
Checks the uninstall entry (that version, publisher Mathias Hertlein, InstallLocation
$env:ProgramFiles\Partitur with or without a trailing backslash, NoModify and NoRepair 1),
partitur.exe, LICENSE, README.md, libcef.dll and adapter\node\node.exe under
$env:ProgramFiles\Partitur, and the all-users Start menu shortcut Partitur.lnk.
#>
function Assert-PartiturInstalled {
    param([Parameter(Mandatory = $true)][string]$Version)
    $entry = Get-PartiturEntry
    if ($null -eq $entry) {
        throw 'Partitur is not installed: no uninstall entry'
    }
    $folder = Join-Path $env:ProgramFiles 'Partitur'
    $location = [string]$entry.InstallLocation
    if ($location.TrimEnd('\') -ne $folder) {
        throw "the uninstall entry has InstallLocation '$location', expected '$folder\'"
    }
    if ($entry.DisplayVersion -cne $Version) {
        throw "the uninstall entry has version '$($entry.DisplayVersion)', expected '$Version'"
    }
    if ($entry.Publisher -cne 'Mathias Hertlein') {
        throw "the uninstall entry has publisher '$($entry.Publisher)', expected 'Mathias Hertlein'"
    }
    if ($entry.NoModify -ne 1) {
        throw "the uninstall entry has NoModify '$($entry.NoModify)', expected 1"
    }
    if ($entry.NoRepair -ne 1) {
        throw "the uninstall entry has NoRepair '$($entry.NoRepair)', expected 1"
    }
    foreach ($path in Get-PartiturPaths) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "$path is missing"
        }
    }
}

<#
.SYNOPSIS
Throws if anything a Partitur install puts on the machine is still there.
.DESCRIPTION
Checks that there is no uninstall entry, no all-users Start menu shortcut Partitur.lnk, none
of partitur.exe, LICENSE, README.md, libcef.dll and adapter\node\node.exe under
$env:ProgramFiles\Partitur, and neither the 64-bit
HKLM\Software\Partitur key nor its StartMenuShortcut value, the shortcut component's key
path. Other files in that folder are not Partitur's and are not checked.
#>
function Assert-PartiturRemoved {
    $entry = Get-PartiturEntry
    if ($null -ne $entry) {
        throw "Partitur $($entry.DisplayVersion) is still installed ($($entry.ProductCode))"
    }
    $base = Open-Hklm64
    try {
        $key = $base.OpenSubKey($shortcutKey)
        if ($null -ne $key) {
            try {
                if ($null -ne $key.GetValue($shortcutValue)) {
                    throw "the value HKLM\$shortcutKey\$shortcutValue is still there"
                }
            } finally { $key.Dispose() }
            throw "the key HKLM\$shortcutKey is still there"
        }
    } finally { $base.Dispose() }
    foreach ($path in Get-PartiturPaths) {
        if (Test-Path -LiteralPath $path) {
            throw "$path is still there"
        }
    }
}

Export-ModuleMember -Function Get-PartiturEntry, Install-PartiturMsi, Uninstall-PartiturMsi,
    Assert-PartiturInstalled, Assert-PartiturRemoved
