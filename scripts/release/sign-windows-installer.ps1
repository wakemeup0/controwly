[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$InstallerRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$required = $env:WINDOWS_AUTHENTICODE_REQUIRED -eq "true"
$certificate = $env:WINDOWS_SIGNING_CERTIFICATE_BASE64
$password = $env:WINDOWS_SIGNING_CERTIFICATE_PASSWORD
$hasCertificate = -not [string]::IsNullOrWhiteSpace($certificate)
$hasPassword = -not [string]::IsNullOrWhiteSpace($password)

if (-not $hasCertificate -and -not $hasPassword) {
    if ($required) {
        Write-Error "Authenticode signing is required but secrets are missing. Configure WINDOWS_SIGNING_CERTIFICATE_BASE64 and WINDOWS_SIGNING_CERTIFICATE_PASSWORD (a trusted production PFX), then retry; never use a self-signed certificate."
        exit 2
    }
    Write-Warning "Authenticode signing is disabled for this release. The NSIS installer will be honestly unsigned and may show a SmartScreen/Unknown publisher warning; Tauri updater payload signatures remain mandatory. Set repository variable WINDOWS_AUTHENTICODE_REQUIRED=true only after the production PFX secrets are configured."
    exit 0
}
if (-not $hasCertificate -or -not $hasPassword) {
    Write-Error "Authenticode signing configuration is incomplete. Supply both WINDOWS_SIGNING_CERTIFICATE_BASE64 and WINDOWS_SIGNING_CERTIFICATE_PASSWORD, or remove both when unsigned publication is intentional."
    exit 2
}

$root = (Resolve-Path -LiteralPath $InstallerRoot).Path
$installers = @(Get-ChildItem -LiteralPath $root -Filter "*.exe" -File)
if ($installers.Count -ne 1) {
    Write-Error "Expected exactly one NSIS installer in $root, found $($installers.Count)."
    exit 1
}

$signtool = Get-Command signtool.exe -ErrorAction SilentlyContinue
if ($null -eq $signtool) {
    Write-Error "signtool.exe is unavailable on this runner. Install the Windows SDK or disable WINDOWS_AUTHENTICODE_REQUIRED; do not publish a claimed signed installer without verification."
    exit 1
}

$tempCertificate = Join-Path ([IO.Path]::GetTempPath()) ("controwly-signing-{0}.pfx" -f ([guid]::NewGuid()))
try {
    try {
        [IO.File]::WriteAllBytes($tempCertificate, [Convert]::FromBase64String($certificate))
    }
    catch {
        Write-Error "WINDOWS_SIGNING_CERTIFICATE_BASE64 is not valid base64 for a production PFX: $($_.Exception.Message)"
        exit 2
    }
    if ((Get-Item -LiteralPath $tempCertificate).Length -eq 0) {
        Write-Error "The decoded Authenticode certificate is empty."
        exit 2
    }

    $timestampUrl = if ([string]::IsNullOrWhiteSpace($env:WINDOWS_SIGNING_TIMESTAMP_URL)) {
        "http://timestamp.digicert.com"
    } else {
        $env:WINDOWS_SIGNING_TIMESTAMP_URL
    }
    if ($timestampUrl -notmatch '^https?://[^\s]+$') {
        Write-Error "WINDOWS_SIGNING_TIMESTAMP_URL must be an HTTPS or HTTP RFC3161 timestamp URL."
        exit 2
    }

    foreach ($installer in $installers) {
        Write-Host "Signing NSIS installer $($installer.Name) with the configured production certificate."
        & $signtool.Source sign /fd sha256 /f $tempCertificate /p $password /tr $timestampUrl /td sha256 $installer.FullName
        if ($LASTEXITCODE -ne 0) {
            Write-Error "signtool failed to sign $($installer.Name) with exit code $LASTEXITCODE."
            exit $LASTEXITCODE
        }
        & $signtool.Source verify /pa /all /tw $installer.FullName
        if ($LASTEXITCODE -ne 0) {
            Write-Error "signtool verification failed for $($installer.Name); refusing to publish."
            exit $LASTEXITCODE
        }
    }
    Write-Host "Authenticode signature verified for the NSIS installer."
}
finally {
    if (Test-Path -LiteralPath $tempCertificate) {
        Remove-Item -LiteralPath $tempCertificate -Force -ErrorAction SilentlyContinue
    }
}
