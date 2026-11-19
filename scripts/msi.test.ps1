# Tests for Partitur's MSI (packaging\windows\partitur.wxs) and scripts\windows-install.psm1, on
# Windows.
#
# Usage: pwsh -NoProfile -File scripts\msi.test.ps1
#
# It must run elevated, as GitHub's Windows runners do: every case installs into Program Files.
# It stages a fake partitur.exe (a small PE built as a library from a one-line C# class and
# copied to that name; it is never run) next to the repository's LICENSE and README.md, installs
# the pinned WiX with scripts\install-wix.ps1, and builds old.msi at 0.0.1 and both new.msi and
# new2.msi at 0.0.2 with scripts\build-msi.ps1. Each case then starts from a machine with no
# Partitur installed and uninstalls whatever it left behind, however it ended:
#   - a fresh install puts everything in place and an uninstall takes all of it away again;
#   - 0.0.2 over 0.0.1 upgrades in place, leaving a single entry;
#   - a second build of 0.0.2 (new2.msi, a new product code) replaces the first, a single entry;
#   - 0.0.1 over 0.0.2 is refused and leaves 0.0.2 installed;
#   - an NSIS install's entry blocks the install with the migration message in the log;
#   - an uninstall leaves files that are not Partitur's, in its folder and in %APPDATA%.
# Every msiexec run logs into a partitur-msi-test-logs folder under $env:RUNNER_TEMP (the system
# temp folder when that is unset) that the script leaves behind, so CI can upload it. When a case
# fails, the log of its first failed step (the lines around the first "Return value 3") is printed
# to stderr, or else its last log in full.
# The NSIS case refuses to run where a real NSIS entry exists, since it deletes that key. One line
# is printed per case; the script exits with status 1 if any case failed.
$ErrorActionPreference = 'Stop'

$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$work = Join-Path ([IO.Path]::GetTempPath()) ("partitur-msitest-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null
$failures = 0
$logRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$logDir = Join-Path $logRoot 'partitur-msi-test-logs'
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$script:caseLogs = @()
$script:logCount = 0

$nsisKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\mhert.partitur'
$migration = 'An earlier Partitur, installed with the previous setup, is still installed. Remove it in Settings › Apps first, then run this installer again.'
$folder = Join-Path $env:ProgramFiles 'Partitur'

function Fail([string]$Message) {
    [Console]::Error.WriteLine("msi.test: $Message")
    exit 1
}

function Expect([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Test-Succeeded([int]$Code) {
    $Code -eq 0 -or $Code -eq 3010
}

function Open-Hklm64 {
    [Microsoft.Win32.RegistryKey]::OpenBaseKey(
        [Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
}

# A fresh msiexec log path in the log folder, remembered for the case running now.
function New-CaseLog([string]$Kind) {
    $script:logCount++
    $path = Join-Path $logDir ('{0:D3}-{1}.log' -f $script:logCount, $Kind)
    $script:caseLogs += $path
    $path
}

function Install-Logged([string]$Path) {
    Install-PartiturMsi -Path $Path -Log (New-CaseLog 'install')
}

function Uninstall-Logged {
    Uninstall-PartiturMsi -Log (New-CaseLog 'uninstall')
}

# Uninstalls every Partitur MSI still installed, so the next case starts from none.
function Reset-Machine {
    for ($i = 0; $i -lt 3 -and $null -ne (Get-PartiturEntry); $i++) {
        $null = Uninstall-Logged
    }
    if ($null -ne (Get-PartiturEntry)) {
        throw 'Partitur is still installed after three uninstalls'
    }
}

# Prints the log that explains a failed case to stderr: the ~40 lines around the first
# "Return value 3" of the first log that has one, else the last log whole.
function Write-FailureLog {
    $logs = @($script:caseLogs | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf })
    foreach ($log in $logs) {
        $lines = @(@(Read-Log $log)[0] -split "\r?\n")
        $at = -1
        for ($i = 0; $i -lt $lines.Count; $i++) {
            if ($lines[$i].Contains('Return value 3')) { $at = $i; break }
        }
        if ($at -ge 0) {
            $from = [Math]::Max(0, $at - 20)
            $to = [Math]::Min($lines.Count - 1, $at + 20)
            [Console]::Error.WriteLine("msi.test: ${log}, lines $($from + 1) to $($to + 1):")
            [Console]::Error.WriteLine(($lines[$from..$to] -join "`n"))
            return
        }
    }
    if ($logs.Count -gt 0) {
        $last = $logs[-1]
        [Console]::Error.WriteLine("msi.test: ${last} (no Return value 3), in full:")
        [Console]::Error.WriteLine(@(Read-Log $last)[0])
    }
}

function Invoke-Case([string]$Name, [scriptblock]$Body) {
    $script:caseLogs = @()
    try {
        Reset-Machine
        # Only the logs of the case's own steps explain a failure, not the cleanup's.
        $script:caseLogs = @()
        & $Body
        Write-Output "ok   $Name"
    } catch {
        Write-Output "FAIL ${Name}: $($_.Exception.Message)"
        $script:failures++
        try { Write-FailureLog } catch {
            [Console]::Error.WriteLine("msi.test: could not read the log: $($_.Exception.Message)")
        }
    } finally {
        try { Reset-Machine } catch {
            Write-Output "FAIL ${Name}: cleaning up: $($_.Exception.Message)"
            $script:failures++
        }
    }
}

# The text of an msiexec log, in every encoding it may be in: UTF-16 when it starts with that
# byte order mark, else UTF-8 and the ANSI code page 1252.
function Read-Log([string]$Path) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -ge 2 -and $bytes[0] -eq 0xFF -and $bytes[1] -eq 0xFE) {
        return @([Text.Encoding]::Unicode.GetString($bytes))
    }
    $texts = @([Text.Encoding]::UTF8.GetString($bytes))
    try {
        [Text.Encoding]::RegisterProvider([Text.CodePagesEncodingProvider]::Instance)
        $texts += [Text.Encoding]::GetEncoding(1252).GetString($bytes)
    } catch { }
    $texts
}

function Test-FreshInstall {
    $code = Install-Logged $newMsi
    Expect ($code -eq 0) "install of new.msi exited with $code"
    Assert-PartiturInstalled -Version '0.0.2'
    $code = Uninstall-Logged
    Expect ($code -eq 0) "uninstall exited with $code"
    Assert-PartiturRemoved
    Expect (-not (Test-Path -LiteralPath $folder)) "$folder is still there"
}

function Test-Upgrade {
    $code = Install-Logged $oldMsi
    Expect ($code -eq 0) "install of old.msi exited with $code"
    $code = Install-Logged $newMsi
    Expect ($code -eq 0) "install of new.msi over old.msi exited with $code"
    # Get-PartiturEntry throws when there is more than one entry.
    $entry = Get-PartiturEntry
    Expect ($null -ne $entry) 'no entry after the upgrade'
    Expect ($entry.DisplayVersion -ceq '0.0.2') "the entry has version $($entry.DisplayVersion), expected 0.0.2"
}

function Test-SameVersion {
    $code = Install-Logged $newMsi
    Expect (Test-Succeeded $code) "install of new.msi exited with $code"
    $code = Install-Logged $new2Msi
    Expect (Test-Succeeded $code) "install of new2.msi (also 0.0.2) over new.msi exited with $code"
    # Get-PartiturEntry throws when there is more than one entry.
    $entry = Get-PartiturEntry
    Expect ($null -ne $entry) 'no entry after the same-version reinstall'
    Expect ($entry.DisplayVersion -ceq '0.0.2') "the entry has version $($entry.DisplayVersion), expected 0.0.2"
    Assert-PartiturInstalled -Version '0.0.2'
}

function Test-Downgrade {
    $code = Install-Logged $newMsi
    Expect ($code -eq 0) "install of new.msi exited with $code"
    $code = Install-Logged $oldMsi
    Expect (-not (Test-Succeeded $code)) "install of old.msi over new.msi exited with $code, expected a refusal"
    Assert-PartiturInstalled -Version '0.0.2'
}

function Test-NsisBlocks {
    $base = Open-Hklm64
    try {
        $existing = $base.OpenSubKey($nsisKey)
        if ($null -ne $existing) {
            $existing.Dispose()
            throw "a real NSIS install's entry HKLM\$nsisKey exists; remove Partitur from Settings > Apps first"
        }
        $key = $base.CreateSubKey($nsisKey)
        try {
            $key.SetValue('UninstallString', '"C:\nowhere\uninstall.exe"')
        } finally { $key.Dispose() }
        try {
            $code = Install-Logged $newMsi
            $log = $script:caseLogs[-1]
            Expect (-not (Test-Succeeded $code)) "install with the NSIS entry present exited with $code, expected a refusal"
            Expect ($null -eq (Get-PartiturEntry)) 'Partitur got installed although the NSIS entry is present'
            $logged = @(Read-Log $log | Where-Object { $_.Contains($migration) })
            Expect ($logged.Count -gt 0) "the msiexec log $log does not contain the migration message"
        } finally {
            $base.DeleteSubKeyTree($nsisKey, $false)
        }
    } finally { $base.Dispose() }
}

function Test-UninstallKeepsForeign {
    $mine = Join-Path $folder 'mine.txt'
    $dataRoot = Join-Path $env:APPDATA 'mhert'
    $data = Join-Path $dataRoot 'partitur'
    $probe = Join-Path $data 'probe.txt'
    $createdRoot = -not (Test-Path -LiteralPath $dataRoot)
    $createdData = -not (Test-Path -LiteralPath $data)
    try {
        $code = Install-Logged $newMsi
        Expect ($code -eq 0) "install of new.msi exited with $code"
        Set-Content -LiteralPath $mine -Value 'not Partitur''s'
        New-Item -ItemType Directory -Force -Path $data | Out-Null
        Set-Content -LiteralPath $probe -Value 'user data'
        $code = Uninstall-Logged
        Expect ($code -eq 0) "uninstall exited with $code"
        Assert-PartiturRemoved
        Expect (Test-Path -LiteralPath $mine -PathType Leaf) "$mine was removed"
        Expect (Test-Path -LiteralPath $probe -PathType Leaf) "$probe was removed"
    } finally {
        Remove-Item -Force -LiteralPath $mine -ErrorAction SilentlyContinue
        # The folder stayed only for mine.txt; with Partitur gone it is the test's to remove.
        if ($null -eq (Get-PartiturEntry) -and (Test-Path -LiteralPath $folder) -and
            -not (Get-ChildItem -Force -LiteralPath $folder)) {
            Remove-Item -Force -LiteralPath $folder
        }
        Remove-Item -Force -LiteralPath $probe -ErrorAction SilentlyContinue
        if ($createdData) { Remove-Item -Recurse -Force -LiteralPath $data -ErrorAction SilentlyContinue }
        if ($createdRoot) { Remove-Item -Recurse -Force -LiteralPath $dataRoot -ErrorAction SilentlyContinue }
    }
}

try {
    $stage = Join-Path $work 'stage'
    New-Item -ItemType Directory -Path $stage | Out-Null
    $fake = Join-Path $work 'fake.dll'
    Add-Type -OutputAssembly $fake -OutputType Library `
        -TypeDefinition 'public static class P { public static int Answer() { return 42; } }'
    Copy-Item -LiteralPath $fake -Destination (Join-Path $stage 'partitur.exe')
    Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination $stage
    Copy-Item -LiteralPath (Join-Path $root 'README.md') -Destination $stage

    & (Join-Path $PSScriptRoot 'install-wix.ps1')
    if ($LASTEXITCODE) { Fail "install-wix.ps1 exited with status $LASTEXITCODE" }

    $oldMsi = Join-Path $work 'old.msi'
    $newMsi = Join-Path $work 'new.msi'
    & (Join-Path $PSScriptRoot 'build-msi.ps1') -Version '0.0.1' -Stage $stage -Output $oldMsi
    if ($LASTEXITCODE) { Fail "build-msi.ps1 for 0.0.1 exited with status $LASTEXITCODE" }
    & (Join-Path $PSScriptRoot 'build-msi.ps1') -Version '0.0.2' -Stage $stage -Output $newMsi
    if ($LASTEXITCODE) { Fail "build-msi.ps1 for 0.0.2 exited with status $LASTEXITCODE" }
    # A second build of the same version, as a release re-run makes: a new product code.
    $new2Msi = Join-Path $work 'new2.msi'
    & (Join-Path $PSScriptRoot 'build-msi.ps1') -Version '0.0.2' -Stage $stage -Output $new2Msi
    if ($LASTEXITCODE) { Fail "build-msi.ps1 for the second 0.0.2 exited with status $LASTEXITCODE" }

    Import-Module (Join-Path $PSScriptRoot 'windows-install.psm1') -Force

    Invoke-Case 'fresh install and uninstall' { Test-FreshInstall }
    Invoke-Case 'upgrade from 0.0.1 to 0.0.2' { Test-Upgrade }
    Invoke-Case 'a second build of 0.0.2 replaces the first' { Test-SameVersion }
    Invoke-Case 'downgrade from 0.0.2 to 0.0.1 is refused' { Test-Downgrade }
    Invoke-Case 'an NSIS install blocks the MSI' { Test-NsisBlocks }
    Invoke-Case 'uninstall keeps files that are not Partitur''s' { Test-UninstallKeepsForeign }
} finally {
    Remove-Item -Recurse -Force -LiteralPath $work -ErrorAction SilentlyContinue
}

if ($failures -gt 0) {
    [Console]::Error.WriteLine("msi.test: $failures case(s) failed")
    exit 1
}
Write-Output 'msi.test: all cases passed'
exit 0
