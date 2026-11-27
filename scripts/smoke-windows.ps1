# Prove an installed or unpacked `partitur.exe` starts, carries the patched adapter and opens its
# window on Windows.
#
# Usage: scripts\smoke-windows.ps1 <partitur.exe> [expected-version] [-VersionOnly]
#
# With a second argument, `--version` must print exactly `partitur <expected-version>`; without
# one it is only printed. The widget engine helper, partitur-widget-engine.exe, must lie beside
# partitur.exe. The adapter in adapter\ beside partitur.exe is then run with the
# node.exe it carries by scripts\adapter-initialize.mjs, and its answer to `initialize` must
# announce the widgets; that needs no Claude Code, so the check runs whether or not one is
# installed. The window run is `partitur --smoke`, which opens the real window on
# throwaway configuration and data folders and exits after the first frame. It passes only if it
# exits with status 0 within 30 s and its log says `opening the window`; its log is printed
# either way. -VersionOnly stops after the `--version` and adapter checks, for a second copy of
# the same build whose window was already proven.
#
# partitur.exe is a GUI program with no console of its own: it writes to stdout and stderr only
# when they are redirected, so every run here goes through Start-Process with both redirected
# to files. The window draws in software (ICED_BACKEND=tiny-skia): the machines this runs on
# have no usable GPU. The release workflow runs it on the installed program and on the zip's;
# it runs by hand on any Windows machine too, and leaves the calling shell's environment as it
# found it. Exits with status 1 on the first check that fails.
param(
    [Parameter(Mandatory = $true, Position = 0)][string]$Exe,
    [Parameter(Position = 1)][string]$ExpectedVersion = '',
    [switch]$VersionOnly
)

$ErrorActionPreference = 'Stop'

function Fail([string]$Message) {
    [Console]::Error.WriteLine("smoke: $Message")
    exit 1
}

# Runs $Exe with $Arguments, stdout and stderr redirected to files, and waits at most
# $TimeoutSeconds. A run still going then is killed with its process tree and counts as failed.
# Returns the exit code (or $null after a timeout) and both outputs.
function Invoke-Bounded([string[]]$Arguments, [int]$TimeoutSeconds) {
    $work = Join-Path ([IO.Path]::GetTempPath()) ("partitur-smoke-" + [Guid]::NewGuid())
    New-Item -ItemType Directory -Path $work | Out-Null
    $out = Join-Path $work 'stdout.txt'
    $err = Join-Path $work 'stderr.txt'
    try {
        $process = Start-Process -FilePath $Exe -ArgumentList $Arguments -NoNewWindow -PassThru `
            -RedirectStandardOutput $out -RedirectStandardError $err
        # Reading the handle once keeps it open, so ExitCode is still there after the exit.
        $null = $process.Handle
        $code = $null
        if ($process.WaitForExit($TimeoutSeconds * 1000)) {
            $process.WaitForExit()
            $code = $process.ExitCode
        } else {
            # taskkill ends the whole tree; Stop-Process is the fallback where it is missing.
            try { & taskkill.exe /T /F /PID $process.Id 2>&1 | Out-Null } catch { }
            if (-not $process.HasExited) {
                Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
            }
            $process.WaitForExit(5000) | Out-Null
        }
        [pscustomobject]@{
            Code   = $code
            Stdout = [string](Get-Content -Raw -LiteralPath $out)
            Stderr = [string](Get-Content -Raw -LiteralPath $err)
        }
    } finally {
        Remove-Item -Recurse -Force -LiteralPath $work -ErrorAction SilentlyContinue
    }
}

if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) {
    Fail "$Exe is not a file"
}

$run = Invoke-Bounded @('--version') 30
if ($null -eq $run.Code) {
    Fail "$Exe --version did not exit within 30 s"
}
if ($run.Code -ne 0) {
    [Console]::Error.Write($run.Stderr)
    Fail "$Exe --version exited with status $($run.Code)"
}
$version = $run.Stdout.Trim()
Write-Output $version
if ($ExpectedVersion -and $version -cne "partitur $ExpectedVersion") {
    Fail "$Exe reports '$version', expected 'partitur $ExpectedVersion'"
}
if ($ExpectedVersion) {
    Write-Output "smoke ok: --version is $ExpectedVersion"
} else {
    Write-Output 'smoke ok: --version runs (no version expected)'
}

$engine = Join-Path (Split-Path -Parent $Exe) 'partitur-widget-engine.exe'
if (-not (Test-Path -LiteralPath $engine -PathType Leaf)) {
    Fail "$(Split-Path -Parent $Exe) carries no partitur-widget-engine.exe"
}
Write-Output 'smoke ok: the widget engine is beside the program'

$adapter = Join-Path (Split-Path -Parent $Exe) 'adapter'
$node = Join-Path $adapter 'node\node.exe'
if (-not (Test-Path -LiteralPath $node -PathType Leaf)) {
    Fail "$adapter carries no node\node.exe"
}
if (Get-Command claude -ErrorAction SilentlyContinue) {
    Write-Output 'smoke: Claude Code is installed; the adapter check does not use it'
} else {
    Write-Output 'smoke: Claude Code is not installed; the adapter check does not need it'
}
& $node (Join-Path $PSScriptRoot 'adapter-initialize.mjs') $adapter
if ($LASTEXITCODE -ne 0) {
    Fail "the adapter in $adapter failed its initialize check"
}
Write-Output 'smoke ok: the adapter announces the widgets'

if ($VersionOnly) {
    exit 0
}

# Debug logging so a failed start says where it stopped. Set for the run only: a smoke started
# by hand from an interactive shell hands that shell back unchanged.
$saved = @{ RUST_LOG = $env:RUST_LOG; ICED_BACKEND = $env:ICED_BACKEND }
$env:RUST_LOG = 'partitur=debug'
$env:ICED_BACKEND = 'tiny-skia'
try {
    $run = Invoke-Bounded @('--smoke') 30
} finally {
    $env:RUST_LOG = $saved.RUST_LOG
    $env:ICED_BACKEND = $saved.ICED_BACKEND
}

[Console]::Error.WriteLine('--- partitur --smoke log ---')
[Console]::Error.Write($run.Stderr)
[Console]::Error.WriteLine('--- end of log ---')

if ($null -eq $run.Code) {
    Fail "$Exe --smoke did not exit within 30 s"
}
if ($run.Code -ne 0) {
    Fail "$Exe --smoke exited with status $($run.Code)"
}
if (-not $run.Stderr.Contains('opening the window')) {
    Fail "$Exe --smoke exited without logging 'opening the window'"
}
Write-Output 'smoke ok: the window opened and closed'
# Said outright: nothing above ran a native command on success, so a caller reading
# $LASTEXITCODE would otherwise find nothing there.
exit 0
