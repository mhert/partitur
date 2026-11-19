# Tests for scripts\check-signature.ps1, on Windows.
#
# Usage: pwsh -NoProfile -File scripts\check-signature.test.ps1
#
# It creates two self-signed code-signing certificates in Cert:\CurrentUser\My and an unsigned
# PE file (a small .dll) built from a one-line C# program (Windows' own binaries are already signed). It
# signs copies of that executable without a timestamp, runs `pwsh -File check-signature.ps1`
# over them and compares each exit code with the expected one. Because the certificates' root
# is not trusted, a valid signature reports UnknownError, which is the status -RequireTrusted
# rejects. Both certificates are removed again whatever happens. One line is printed per case;
# the script exits with status 1 if any case came out wrong.
$ErrorActionPreference = 'Stop'

$checker = Join-Path $PSScriptRoot 'check-signature.ps1'
$work = Join-Path ([IO.Path]::GetTempPath()) ("partitur-sigtest-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null
$certificates = @()
$failures = 0

# Runs the checker with $Arguments over $File; returns its exit code and stderr.
function Invoke-Check([string]$File, [string[]]$Arguments) {
    $err = Join-Path $work 'stderr.txt'
    & pwsh -NoProfile -NonInteractive -File $checker -Path $File @Arguments 2> $err | Out-Null
    [pscustomobject]@{
        Code   = $LASTEXITCODE
        Stderr = [string](Get-Content -Raw -LiteralPath $err)
    }
}

function Assert-Case([string]$Name, [string]$File, [string[]]$Arguments, [int]$Expected, [string]$StderrContains = '') {
    $run = Invoke-Check $File $Arguments
    $ok = ($run.Code -eq $Expected)
    if ($ok -and $StderrContains -and -not $run.Stderr.Contains($StderrContains)) {
        $ok = $false
    }
    if ($ok) {
        Write-Output "ok   $Name (exit $($run.Code))"
    } else {
        Write-Output "FAIL ${Name}: exit $($run.Code), expected $Expected; stderr: $($run.Stderr.Trim())"
        $script:failures++
    }
}

try {
    $good = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=Partitur Signature Test' `
        -CertStoreLocation Cert:\CurrentUser\My
    $certificates += $good
    $other = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=Partitur Signature Test Other' `
        -CertStoreLocation Cert:\CurrentUser\My
    $certificates += $other

    $unsigned = Join-Path $work 'unsigned.dll'
    Add-Type -OutputAssembly $unsigned -OutputType Library `
        -TypeDefinition 'public static class P { public static int Answer() { return 42; } }'

    function New-Signed([string]$Name, $Certificate) {
        $copy = Join-Path $work $Name
        Copy-Item -LiteralPath $unsigned -Destination $copy
        $result = Set-AuthenticodeSignature -LiteralPath $copy -Certificate $Certificate
        if ($result.Status -ne 'Valid' -and $result.Status -ne 'UnknownError') {
            throw "could not sign ${Name}: $($result.Status) $($result.StatusMessage)"
        }
        $copy
    }

    $signed = New-Signed 'signed.dll' $good
    $signedOther = New-Signed 'signed-other.dll' $other
    $tampered = New-Signed 'tampered.dll' $good
    # Offset 0x4E is the text of the DOS stub, inside the hashed range: change it to another byte.
    $stream = [IO.File]::Open($tampered, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite)
    try {
        $null = $stream.Seek(0x4E, [IO.SeekOrigin]::Begin)
        $byte = $stream.ReadByte()
        $null = $stream.Seek(0x4E, [IO.SeekOrigin]::Begin)
        $stream.WriteByte([byte](($byte + 1) % 256))
    } finally { $stream.Dispose() }

    $name = 'Partitur Signature Test'
    Assert-Case 'unsigned file' $unsigned @('-Signer', $name) 1
    Assert-Case 'signed by the test certificate' $signed @('-Signer', $name) 0
    Assert-Case 'untrusted root with -RequireTrusted' $signed @('-Signer', $name, '-RequireTrusted') 1
    Assert-Case 'no timestamp with -RequireTimestamp' $signed @('-Signer', $name, '-RequireTimestamp') 1
    Assert-Case 'signed by another certificate' $signedOther @('-Signer', $name) 1
    Assert-Case 'signer that is only a prefix' $signed @('-Signer', 'Partitur Signature') 1
    Assert-Case 'one hashed byte changed after signing' $tampered @('-Signer', $name) 1
    Assert-Case 'empty signer prints the subject found' $signed @('-Signer', '') 1 "CN=$name"
} finally {
    foreach ($certificate in $certificates) {
        Remove-Item -LiteralPath "Cert:\CurrentUser\My\$($certificate.Thumbprint)" -DeleteKey -ErrorAction SilentlyContinue
    }
    Remove-Item -Recurse -Force -LiteralPath $work -ErrorAction SilentlyContinue
}

if ($failures -gt 0) {
    [Console]::Error.WriteLine("check-signature.test: $failures case(s) failed")
    exit 1
}
Write-Output 'check-signature.test: all cases passed'
exit 0
