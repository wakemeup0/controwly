[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateScript({
        if (-not (Test-Path -LiteralPath $_ -PathType Leaf)) {
            throw "installer not found: $_"
        }
        $true
    })]
    [string]$InstallerPath,

    [Parameter()]
    [string]$ExpectedVersion,

    [Parameter()]
[string]$InstallDirectory = (Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'Controwly'),

    [Parameter()]
    [string]$ExecutablePath,

    [Parameter()]
    [string]$OutputPath = (Join-Path $env:USERPROFILE 'Downloads\Controwly-Qualification\installer-install.json'),

    [Parameter()]
    [string]$ProductName = 'Controwly',

    [Parameter()]
    [string[]]$InstallerArgument = @('/S'),

    [Parameter()]
    [switch]$RequireSignature,

    [Parameter()]
    [switch]$RequireElevation
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
function Test-AdministratorToken {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}


function Get-OptionalProperty {
    param(
        [Parameter(Mandatory = $true)][object]$Object,
        [Parameter(Mandatory = $true)][string]$Name
    )
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    return $property.Value
}

function Get-UninstallRecord {
    param([Parameter(Mandatory = $true)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    try {
        return Get-ItemProperty -LiteralPath $Path -ErrorAction Stop
    }
    catch {
        return [pscustomobject]@{ Error = $_.Exception.Message }
    }
}

function Get-ProcessSnapshot {
    param([Parameter(Mandatory = $true)][string]$Name)
    try {
        return @(Get-Process -Name $Name -ErrorAction SilentlyContinue | Select-Object Id,ProcessName,Path,MainWindowTitle)
    }
    catch {
        return @()
    }
}

$installer = (Resolve-Path -LiteralPath $InstallerPath).Path
$installRoot = [IO.Path]::GetFullPath($InstallDirectory)
$programFilesRoot = [IO.Path]::GetFullPath([Environment]::GetFolderPath('ProgramFiles')).TrimEnd('\') + '\'
$requiresElevation = $installRoot.StartsWith($programFilesRoot, [StringComparison]::OrdinalIgnoreCase)
$isAdministrator = Test-AdministratorToken
if (-not $ExecutablePath) {
    $ExecutablePath = Join-Path $installRoot 'Controwly.exe'
}
else {
    $ExecutablePath = [IO.Path]::GetFullPath($ExecutablePath)
}
$outputParent = Split-Path -Parent $OutputPath
if ($outputParent) {
    New-Item -ItemType Directory -Force -Path $outputParent | Out-Null
}

$uninstallPaths = @(
    'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Controwly',
    'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Controwly',
    'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Controwly'
)
$existingProcesses = Get-ProcessSnapshot -Name 'Controwly'
$before = [ordered]@{
    installDirectoryPresent = Test-Path -LiteralPath $installRoot
    executablePresent = Test-Path -LiteralPath $ExecutablePath
    appDataPresent = Test-Path -LiteralPath (Join-Path $env:APPDATA 'Controwly')
    localAppDataPresent = Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA 'Controwly')
    uninstallRecords = @($uninstallPaths | ForEach-Object {
        [ordered]@{ path = $_; present = Test-Path -LiteralPath $_ }
    })
    existingProcesses = @($existingProcesses)
    elevation = [ordered]@{
        administratorToken = $isAdministrator
        installPathRequiresElevation = $requiresElevation
        requiredBySwitch = $RequireElevation.IsPresent
    }
}
$effectiveInstallerArgument = @($InstallerArgument)

$exitCode = 0
$report = $null
try {
if (($RequireElevation -or $requiresElevation) -and -not $isAdministrator) {
    throw "an elevated administrator token is required for this per-machine install path; run this script from an elevated SSH PowerShell session or an approved /RL HIGHEST task"
    }
    $hasInstallDirectoryArgument = @($effectiveInstallerArgument | Where-Object {
        [string]$_ -match '(?i)^/D='
    }).Count -gt 0
    if (-not $hasInstallDirectoryArgument) {
        $effectiveInstallerArgument += "/D=`"$installRoot`""
    }
    $start = Get-Date
    $process = Start-Process -FilePath $installer -ArgumentList $effectiveInstallerArgument -Wait -PassThru -WindowStyle Hidden
    $installerExitCode = $process.ExitCode
    if ($installerExitCode -ne 0) {
        throw "installer exited with code $installerExitCode"
    }

    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    while (-not (Test-Path -LiteralPath $ExecutablePath) -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Seconds 1
    }
    if (-not (Test-Path -LiteralPath $ExecutablePath -PathType Leaf)) {
        throw "installed executable not found at $ExecutablePath"
    }

    $fileVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo($ExecutablePath)
    $actualVersion = [string]$fileVersion.FileVersion
    if ($ExpectedVersion -and $actualVersion -ne $ExpectedVersion) {
        throw "installed version '$actualVersion' does not match expected '$ExpectedVersion'"
    }

    $signature = Get-AuthenticodeSignature -LiteralPath $ExecutablePath
    if ($RequireSignature -and $signature.Status -ne 'Valid') {
        throw "required Authenticode signature is not valid: $($signature.Status)"
    }

    $after = [ordered]@{
        installDirectoryPresent = Test-Path -LiteralPath $installRoot
        executablePresent = Test-Path -LiteralPath $ExecutablePath
        executablePath = $ExecutablePath
        fileVersion = $actualVersion
        productVersion = [string]$fileVersion.ProductVersion
        signatureStatus = [string]$signature.Status
        signerSubject = if ($null -ne $signature.SignerCertificate) { [string]$signature.SignerCertificate.Subject } else { $null }
        uninstallRecords = @($uninstallPaths | ForEach-Object {
            $record = Get-UninstallRecord -Path $_
            [ordered]@{
                path = $_
                present = $null -ne $record
                displayName = if ($null -ne $record) { [string](Get-OptionalProperty -Object $record -Name 'DisplayName') } else { $null }
                displayVersion = if ($null -ne $record) { [string](Get-OptionalProperty -Object $record -Name 'DisplayVersion') } else { $null }
                installLocation = if ($null -ne $record) { [string](Get-OptionalProperty -Object $record -Name 'InstallLocation') } else { $null }
            }
        })
        appDataPresent = Test-Path -LiteralPath (Join-Path $env:APPDATA 'Controwly')
        localAppDataPresent = Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA 'Controwly')
    }

    $report = [ordered]@{
        schema = 1
        generatedAt = [DateTime]::UtcNow.ToString('o')
        platform = 'windows'
        outcome = 'installed'
        productName = $ProductName
        installer = $installer
        requestedInstallerArguments = @($InstallerArgument)
        installerArguments = @($effectiveInstallerArgument)
        expectedVersion = if ($ExpectedVersion) { $ExpectedVersion } else { $null }
        installDirectory = $installRoot
        executable = $ExecutablePath
        installerExitCode = $installerExitCode
        elevation = $before.elevation
        startedAt = $start.ToUniversalTime().ToString('o')
        before = $before
        after = $after
        preservation = [ordered]@{
            unrelatedApplicationsStopped = $false
            userDataRemoved = $false
            uninstallInvoked = $false
            note = 'Only this installer was invoked; this script never stops processes, removes user data, changes default devices, or uninstalls software.'
        }
    }
}
catch {
    $exitCode = 1
    $report = [ordered]@{
        schema = 1
        generatedAt = [DateTime]::UtcNow.ToString('o')
        platform = 'windows'
        outcome = 'error'
        productName = $ProductName
        installer = $installer
        requestedInstallerArguments = @($InstallerArgument)
        installerArguments = @($effectiveInstallerArgument)
        expectedVersion = if ($ExpectedVersion) { $ExpectedVersion } else { $null }
        installDirectory = $installRoot
        executable = $ExecutablePath
        before = $before
        elevation = $before.elevation
        preservation = [ordered]@{
            unrelatedApplicationsStopped = $false
            userDataRemoved = $false
            uninstallInvoked = $false
        }
        error = $_.Exception.Message
    }
}

$json = $report | ConvertTo-Json -Depth 12
Set-Content -LiteralPath $OutputPath -Value $json -Encoding UTF8
[Console]::Out.WriteLine($json)
[Console]::Out.WriteLine("report=$OutputPath")
exit $exitCode
