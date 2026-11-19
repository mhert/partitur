# Check the Authenticode signature of a file on Windows: who signed it, whether the chain is
# trusted, and whether it carries a timestamp.
#
# Usage: scripts\check-signature.ps1 -Path <file> -Signer <CN> [-RequireTrusted] [-RequireTimestamp]
#
# The file must carry a signature whose status is Valid or UnknownError (a signature that is
# intact but whose root this machine does not trust, as with a test certificate). NotSigned,
# HashMismatch and every other status fail. With -RequireTrusted only Valid passes. The signer
# certificate's subject must contain the relative distinguished name `CN=<Signer>` exactly, so
# `CN=SignPath Foundation Test` does not satisfy `SignPath Foundation`. With -RequireTimestamp
# the signature must also carry a timestamp countersignature. An empty -Signer always fails,
# and its message prints the subject found, which is how the value to expect is learned.
#
# On success it prints `signature ok: <path> signed by <subject>` and exits 0. On the first
# check that fails it writes `check-signature: <path>: <reason>` to stderr and exits 1.
param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Signer,
    [switch]$RequireTrusted,
    [switch]$RequireTimestamp
)

$ErrorActionPreference = 'Stop'

function Fail([string]$Message) {
    [Console]::Error.WriteLine("check-signature: ${Path}: $Message")
    exit 1
}

if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    Fail 'not a file'
}

$signature = Get-AuthenticodeSignature -LiteralPath $Path
$status = [string]$signature.Status

if ($status -ne 'Valid' -and $status -ne 'UnknownError') {
    Fail "signature status is $status ($($signature.StatusMessage))"
}
if ($RequireTrusted -and $status -ne 'Valid') {
    Fail "signature is not trusted: status $status ($($signature.StatusMessage))"
}

$certificate = $signature.SignerCertificate
if ($null -eq $certificate) {
    Fail 'no signer certificate'
}
$subject = $certificate.Subject

if ([string]::IsNullOrEmpty($Signer)) {
    Fail "no expected signer given; the file is signed by $subject"
}
$pattern = '(^|,\s*)CN=' + [regex]::Escape($Signer) + '(\s*,|$)'
if ($subject -cnotmatch $pattern) {
    Fail "signed by $subject, expected CN=$Signer"
}

if ($RequireTimestamp -and $null -eq $signature.TimeStamperCertificate) {
    Fail 'signature carries no timestamp'
}

Write-Output "signature ok: $Path signed by $subject"
exit 0
