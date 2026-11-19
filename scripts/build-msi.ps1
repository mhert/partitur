# Build Partitur's MSI from a staging folder, on Windows.
#
# Usage: scripts\build-msi.ps1 -Version <x.y.z> -Stage <dir> -Output <file.msi>
#
# -Stage must hold partitur.exe, LICENSE and README.md, the three files the MSI installs. The
# license page of the install dialogs shows RTF, and LICENSE is plain text, so LICENSE is first
# written as RTF into a temporary folder: monospaced, its backslashes and braces escaped, any
# character beyond ASCII as an RTF \u escape, and every line ending in \par. Then
# packaging\windows\partitur.wxs is built with the WiX 7 that scripts\install-wix.ps1 installs,
# for x64, accepting the WiX 7 Open Source Maintenance Fee EULA on the command line (the project
# is exempt from the fee). The output's folder is created if missing. It runs from any directory,
# removes its temporary folder whatever happens, and exits with status 1 when an input is missing
# or the build fails.
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][string]$Stage,
    [Parameter(Mandatory = $true)][string]$Output
)

$ErrorActionPreference = 'Stop'

function Fail([string]$Message) {
    [Console]::Error.WriteLine("build-msi: $Message")
    exit 1
}

if ($Version -notmatch '^\d+\.\d+\.\d+$') {
    Fail "version '$Version' is not of the form x.y.z"
}
if (-not (Test-Path -LiteralPath $Stage -PathType Container)) {
    Fail "$Stage is not a folder"
}
$stageDir = (Resolve-Path -LiteralPath $Stage).ProviderPath
foreach ($name in 'partitur.exe', 'LICENSE', 'README.md') {
    if (-not (Test-Path -LiteralPath (Join-Path $stageDir $name) -PathType Leaf)) {
        Fail "$stageDir holds no $name"
    }
}

$source = Join-Path $PSScriptRoot '..\packaging\windows\partitur.wxs'
$source = (Resolve-Path -LiteralPath $source).ProviderPath
$out = [IO.Path]::GetFullPath($Output, (Get-Location).ProviderPath)
$outDir = Split-Path -Parent $out
if (-not (Test-Path -LiteralPath $outDir)) {
    New-Item -ItemType Directory -Path $outDir | Out-Null
}

# LICENSE as the body of an RTF document, one \par per line of the text.
function ConvertTo-Rtf([string]$Text) {
    $body = [Text.StringBuilder]::new()
    $lines = @($Text -split "\r?\n")
    # The newline that ends the file is no line of its own.
    if ($lines.Count -gt 0 -and $lines[-1] -eq '') {
        $lines = @($lines | Select-Object -SkipLast 1)
    }
    foreach ($line in $lines) {
        foreach ($char in $line.ToCharArray()) {
            $code = [int]$char
            if ($char -eq '\' -or $char -eq '{' -or $char -eq '}') {
                [void]$body.Append('\').Append($char)
            } elseif ($code -gt 127) {
                # \uN takes a signed 16-bit number; the ? is what readers without Unicode show.
                $signed = if ($code -gt 32767) { $code - 65536 } else { $code }
                # Braces: ? is a legal character of a variable name.
                [void]$body.Append("\u${signed}?")
            } else {
                [void]$body.Append($char)
            }
        }
        [void]$body.Append("\par`r`n")
    }
    "{\rtf1\ansi\deff0{\fonttbl{\f0 Consolas;}}\f0\fs18`r`n" + $body.ToString() + "}`r`n"
}

$work = Join-Path ([IO.Path]::GetTempPath()) ("partitur-msi-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $rtf = Join-Path $work 'LICENSE.rtf'
    $text = [IO.File]::ReadAllText((Join-Path $stageDir 'LICENSE'))
    [IO.File]::WriteAllText($rtf, (ConvertTo-Rtf $text), [Text.Encoding]::ASCII)

    & wix build $source -arch x64 -acceptEula wix7 -ext WixToolset.UI.wixext `
        -d "Version=$Version" -d "Stage=$stageDir" -d "LicenseRtf=$rtf" -o $out
    if ($LASTEXITCODE) { Fail "wix build exited with status $LASTEXITCODE" }
} finally {
    Remove-Item -Recurse -Force -LiteralPath $work -ErrorAction SilentlyContinue
}

if (-not (Test-Path -LiteralPath $out -PathType Leaf)) {
    Fail "wix build wrote no $out"
}
Write-Output "build-msi: $out (Partitur $Version)"
exit 0
