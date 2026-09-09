[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateScript({
        if (-not (Test-Path -LiteralPath $_ -PathType Leaf)) {
            throw "executable not found: $_"
        }
        $true
    })]
    [string]$ExecutablePath,

    [Parameter()]
    [ValidateRange(1, 300)]
    [int]$WaitSeconds = 12,

    [Parameter()]
    [string]$OutputDirectory = (Join-Path $env:USERPROFILE 'Downloads\Controwly-Qualification\interactive'),

    [Parameter()]
    [switch]$NoScreenshot,

    [Parameter()]
    [switch]$KeepTask
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Get-ActiveSessionText {
    try {
        return (@(quser 2>&1 | Out-String).Trim())
    }
    catch {
        return $null
    }
}

function Assert-InteractiveSession {
    $sessionText = Get-ActiveSessionText
    if (-not $sessionText -or $sessionText -notmatch '(?im)\bActive\b') {
        throw 'no active interactive Windows session is available; schtasks /IT would not produce a visible launch'
    }
    return $sessionText
}

function ConvertTo-PowerShellLiteral {
    param([Parameter(Mandatory = $true)][string]$Value)
    return "'$(($Value -replace "'", "''"))'"
}

$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$exe = (Resolve-Path -LiteralPath $ExecutablePath).Path
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$runId = [Guid]::NewGuid().ToString('N')
$taskName = "ControwlyQualification-$runId"
$payloadPath = Join-Path $OutputDirectory "launch-$runId.ps1"
$payloadResultPath = Join-Path $OutputDirectory "launch-$runId-result.json"
$reportPath = Join-Path $OutputDirectory "interactive-$runId.json"
$capturedScreenshotPath = Join-Path $OutputDirectory "interactive-$runId.png"
$exeLiteral = ConvertTo-PowerShellLiteral -Value $exe
$resultLiteral = ConvertTo-PowerShellLiteral -Value $payloadResultPath
$screenshotLiteral = ConvertTo-PowerShellLiteral -Value $capturedScreenshotPath
$takeScreenshot = (-not $NoScreenshot).ToString().ToLowerInvariant()
$payload = @"
`$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
`$exe = $exeLiteral
`$resultPath = $resultLiteral
`$screenshotPath = $screenshotLiteral
`$capture = [bool]::Parse('$takeScreenshot')
`$started = Get-Date
`$proc = `$null
`$launchMode = 'started'
`$screenshotStatus = 'skipped'
`$screenshotError = `$null
try {
    `$existing = @(Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension(`$exe)) -ErrorAction SilentlyContinue | Where-Object {
        try { `$_.Path -eq `$exe } catch { `$false }
    })
    if (`$existing.Count -gt 0) {
        `$proc = `$existing[0]
        `$launchMode = 'existing-process'
    }
    else {
        `$proc = Start-Process -FilePath `$exe -PassThru
    }
    Start-Sleep -Seconds $WaitSeconds
    `$proc.Refresh()
    `$handle = [IntPtr]::Zero
    if (`$proc.MainWindowHandle -ne 0) {
        `$handle = [IntPtr]`$proc.MainWindowHandle
    }
    `$observedOutcome = if (`$proc.HasExited) { 'exited-before-observation' } elseif (`$handle -eq [IntPtr]::Zero) { 'started-no-window' } else { 'launched' }
    if (`$capture -and `$handle -ne [IntPtr]::Zero) {
        try {
            Add-Type -AssemblyName System.Drawing
            Add-Type -TypeDefinition @'
using System;
using System.Drawing;
using System.Runtime.InteropServices;
public static class ControwlyWindowCapture {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
}
'@ -ErrorAction Stop
            [ControwlyWindowCapture]::SetForegroundWindow(`$handle) | Out-Null
            Start-Sleep -Milliseconds 250
            `$rect = New-Object ControwlyWindowCapture+RECT
            if (-not [ControwlyWindowCapture]::GetWindowRect(`$handle, [ref]`$rect)) {
                throw 'GetWindowRect failed'
            }
            `$width = `$rect.Right - `$rect.Left
            `$height = `$rect.Bottom - `$rect.Top
            if (`$width -lt 64 -or `$height -lt 64 -or `$width -gt 8192 -or `$height -gt 8192) {
                throw "window bounds are not capturable: `$width x `$height"
            }
            `$bitmap = New-Object Drawing.Bitmap(`$width, `$height)
            `$graphics = [Drawing.Graphics]::FromImage(`$bitmap)
            try {
                `$graphics.CopyFromScreen(`$rect.Left, `$rect.Top, 0, 0, `$bitmap.Size)
                `$bitmap.Save(`$screenshotPath, [Drawing.Imaging.ImageFormat]::Png)
            }
            finally {
                `$graphics.Dispose()
                `$bitmap.Dispose()
            }
            `$screenshotStatus = 'captured'
        }
        catch {
            `$screenshotStatus = 'error'
            `$screenshotError = `$_.Exception.Message
        }
    }
    elseif (`$capture) {
        `$screenshotStatus = 'no-window-handle'
    }
    [ordered]@{
        outcome = `$observedOutcome
        launchMode = `$launchMode
        pid = `$proc.Id
        hasExited = `$proc.HasExited
        mainWindowTitle = [string]`$proc.MainWindowTitle
        mainWindowHandle = `$handle.ToInt64()
        screenshotStatus = `$screenshotStatus
        screenshotPath = if (`$screenshotStatus -eq 'captured') { `$screenshotPath } else { `$null }
        screenshotError = `$screenshotError
        startedAt = `$started.ToUniversalTime().ToString('o')
        observedAt = [DateTime]::UtcNow.ToString('o')
    } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath `$resultPath -Encoding UTF8
}
catch {
    [ordered]@{
        outcome = 'error'
        launchMode = `$launchMode
        error = `$_.Exception.Message
        startedAt = `$started.ToUniversalTime().ToString('o')
        observedAt = [DateTime]::UtcNow.ToString('o')
    } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath `$resultPath -Encoding UTF8
}
"@
Set-Content -LiteralPath $payloadPath -Value $payload -Encoding UTF8

$exitCode = 0
$taskCreated = $false
$taskDeleted = $false
$report = $null
$sessionText = $null
try {
    $sessionText = Assert-InteractiveSession
    $runAt = (Get-Date).AddMinutes(1)
    $runAtDate = $runAt.ToString('MM/dd/yyyy')
    $runAtTime = $runAt.ToString('HH:mm')
    $taskAction = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$payloadPath`""
    & schtasks.exe /Create /TN $taskName /TR $taskAction /SC ONCE /SD $runAtDate /ST $runAtTime /IT /RL HIGHEST /F | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "schtasks /Create failed with exit code $LASTEXITCODE"
    }
    $taskCreated = $true
    & schtasks.exe /Run /TN $taskName | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "schtasks /Run failed with exit code $LASTEXITCODE"
    }

    $deadline = [DateTime]::UtcNow.AddSeconds($WaitSeconds + 45)
    while (-not (Test-Path -LiteralPath $payloadResultPath) -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Seconds 1
    }
    if (-not (Test-Path -LiteralPath $payloadResultPath -PathType Leaf)) {
        throw "interactive task produced no result within the timeout: $payloadResultPath"
    }
    $payloadResult = Get-Content -LiteralPath $payloadResultPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ([string]$payloadResult.outcome -ne 'launched') {
        throw "interactive task did not expose a live Controwly window: $($payloadResult.outcome)"
    }
    if (-not $KeepTask) {
        & schtasks.exe /Delete /TN $taskName /F | Out-Null
        $taskDeleted = ($LASTEXITCODE -eq 0)
    }
    $report = [ordered]@{
        schema = 1
        generatedAt = [DateTime]::UtcNow.ToString('o')
        platform = 'windows'
        outcome = 'launched'
        executable = $exe
        waitSeconds = $WaitSeconds
        taskName = $taskName
        taskCreated = $taskCreated
        runLevel = 'HIGHEST'
        taskDeleted = $taskDeleted
        activeSessions = $sessionText
        taskPayload = $payloadPath
        taskResult = $payloadResultPath
        taskResultData = $payloadResult
        screenshot = if ([string]$payloadResult.screenshotStatus -eq 'captured') { $capturedScreenshotPath } else { $null }
        preservation = [ordered]@{
            unrelatedProcessesStopped = $false
            existingControwlyProcessStopped = $false
            taskOnlyCleanup = (-not $KeepTask)
            note = 'The isolated task launches or observes only the requested executable. It never stops a process, changes a controller, or removes user data.'
        }
    }
}
catch {
    $exitCode = if ($_.Exception.Message -like '*no active interactive*' -or $_.Exception.Message -like '*did not expose a live*') { 3 } else { 1 }
    if ($taskCreated -and -not $KeepTask) {
        & schtasks.exe /Delete /TN $taskName /F | Out-Null
        $taskDeleted = ($LASTEXITCODE -eq 0)
    }
    $report = [ordered]@{
        schema = 1
        generatedAt = [DateTime]::UtcNow.ToString('o')
        platform = 'windows'
        outcome = if ($exitCode -eq 3) { 'blocked-no-visible-window' } else { 'error' }
        executable = $exe
        taskName = $taskName
        taskCreated = $taskCreated
        runLevel = 'HIGHEST'
        taskDeleted = $taskDeleted
        activeSessions = $sessionText
        taskPayload = $payloadPath
        taskResult = if (Test-Path -LiteralPath $payloadResultPath) { $payloadResultPath } else { $null }
        screenshot = if (Test-Path -LiteralPath $capturedScreenshotPath) { $capturedScreenshotPath } else { $null }
        preservation = [ordered]@{
            unrelatedProcessesStopped = $false
            existingControwlyProcessStopped = $false
            userDataRemoved = $false
        }
        error = $_.Exception.Message
    }
}

$report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $reportPath -Encoding UTF8
[Console]::Out.WriteLine(($report | ConvertTo-Json -Depth 12))
[Console]::Out.WriteLine("report=$reportPath")
exit $exitCode
