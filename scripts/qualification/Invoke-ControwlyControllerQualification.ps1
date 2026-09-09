[CmdletBinding()]
param(
    [Parameter()]
    [string]$InventoryScriptPath = (Join-Path $PSScriptRoot 'Get-ControwlyControllerInventory.ps1'),

    [Parameter()]
    [string]$OutputDirectory = (Join-Path (Split-Path -Parent $PSScriptRoot) '..\target\controwly-qualification\windows-controller'),

    [Parameter()]
    [ValidateRange(0, 3600)]
    [int]$WaitSeconds = 0,

    [Parameter()]
    [ValidateSet('Any', 'Xbox', 'DualSense', 'DualShock4', 'Generic')]
    [string]$ExpectedFamily = 'Any',

    [Parameter()]
    [string]$ControllerId
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Get-ControllerFamily {
    param([Parameter(Mandatory = $true)][object]$Device)
    $name = [string]$Device.name
    if ($name -match '(?i)xbox|xinput') {
        return 'Xbox'
    }
    if ($name -match '(?i)dualsense|ps5|playstation 5') {
        return 'DualSense'
    }
    if ($name -match '(?i)dualshock(?:[ -]?4)?|ps4|playstation 4') {
        return 'DualShock4'
    }
    return 'Generic'
}

function Get-Inventory {
    param([Parameter(Mandatory = $true)][string]$Path)
    if (-not (Test-Path -LiteralPath $InventoryScriptPath -PathType Leaf)) {
        throw "inventory script not found: $InventoryScriptPath"
    }
    $powershell = Join-Path $env:WINDIR 'System32\WindowsPowerShell\v1.0\powershell.exe'
    if (-not (Test-Path -LiteralPath $powershell -PathType Leaf)) {
        throw "Windows PowerShell executable not found: $powershell"
    }
    & $powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $InventoryScriptPath -OutputPath $Path -IncludeRaw | Out-Null
    $inventoryExitCode = if ($null -eq $LASTEXITCODE) { 0 } else { [int]$LASTEXITCODE }
    if ($inventoryExitCode -ne 0) {
        throw "inventory script failed with exit code $inventoryExitCode"
    }
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "inventory script did not write its report: $Path"
    }
    return Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json
}

function Select-Controller {
    param([Parameter(Mandatory = $true)][object]$Inventory)
    $devices = @($Inventory.devices | Where-Object {
        $_.capabilities.gamepad -or $_.capabilities.joystick -or $_.capabilities.xinput
    })
    if ($ControllerId) {
        $devices = @($devices | Where-Object {
            ([string]$_.stableId -ceq $ControllerId) -or ([string]$_.instanceId -ceq $ControllerId)
        })
    }
    if ($ExpectedFamily -ne 'Any') {
        $devices = @($devices | Where-Object {
            (Get-ControllerFamily -Device $_) -eq $ExpectedFamily
        })
    }
    return $devices
}

$runId = [Guid]::NewGuid().ToString('N')
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$InventoryScriptPath = [IO.Path]::GetFullPath($InventoryScriptPath)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$baselinePath = Join-Path $OutputDirectory "inventory-$runId-baseline.json"
$finalPath = Join-Path $OutputDirectory "inventory-$runId-final.json"
$reportPath = Join-Path $OutputDirectory "qualification-$runId.json"
$observations = [System.Collections.Generic.List[object]]::new()
$exitCode = 0
$report = $null

try {
    $baseline = Get-Inventory -Path $baselinePath
    $baselineCandidates = @(Select-Controller -Inventory $baseline)
    [void]$observations.Add([pscustomobject]@{
        at = [DateTime]::UtcNow.ToString('o')
        report = $baselinePath
        controllerCount = $baselineCandidates.Count
        xinputConnectedSlotCount = @($baseline.xinput.connectedSlots).Count
    })

    $final = $baseline
    $deadline = [DateTime]::UtcNow.AddSeconds($WaitSeconds)
    while ($true) {
        $selected = @(Select-Controller -Inventory $final)
        if ($selected.Count -gt 0 -or [DateTime]::UtcNow -ge $deadline) {
            break
        }
        $remainingSeconds = [int][Math]::Ceiling(($deadline - [DateTime]::UtcNow).TotalSeconds)
        Start-Sleep -Seconds ([Math]::Min(3, [Math]::Max(1, $remainingSeconds)))
        $pollPath = Join-Path $OutputDirectory "inventory-$runId-$($observations.Count).json"
        $final = Get-Inventory -Path $pollPath
        [void]$observations.Add([pscustomobject]@{
            at = [DateTime]::UtcNow.ToString('o')
            report = $pollPath
            controllerCount = @((Select-Controller -Inventory $final)).Count
            xinputConnectedSlotCount = @($final.xinput.connectedSlots).Count
        })
    }
    $final | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $finalPath -Encoding UTF8
    $xinputSlotCount = @($final.xinput.connectedSlots).Count
    $controllerBehaviorProof = $false
    $outcome = if ($selected.Count -gt 0) {
        'observed-no-behavior-proof'
    } elseif ($xinputSlotCount -gt 0) {
        'xinput-slot-observed-no-device-identity'
    } else {
        'blocked-no-matching-controller'
    }
    $exitCode = 3

    $report = [ordered]@{
        schema = 1
        generatedAt = [DateTime]::UtcNow.ToString('o')
        platform = 'windows'
        runId = $runId
        outcome = $outcome
        controllerBehaviorProof = $controllerBehaviorProof
        injectionAvailable = $false
        proofMode = 'read-only-device-inventory'
        proofScope = 'read-only-device-inventory'
        xinputConnectedSlotCount = $xinputSlotCount
        expectedFamily = $ExpectedFamily
        requestedControllerId = if ($ControllerId) { $ControllerId } else { $null }
        waitSeconds = $WaitSeconds
        baselineInventory = $baselinePath
        finalInventory = $finalPath
        matchingControllers = @($selected | ForEach-Object {
            [ordered]@{
                stableId = [string]$_.stableId
                instanceId = [string]$_.instanceId
                name = [string]$_.name
                family = Get-ControllerFamily -Device $_
                connection = [string]$_.connection
                capabilities = $_.capabilities
                coverage = [string]$_.coverage
            }
        })
        observations = @($observations)
        injection = [ordered]@{
            status = 'unavailable'
            injectionAvailable = $false
            proofMode = 'read-only-inventory'
            driverInstallAttempted = $false
            physicalHardwareAttached = $false
            virtualFixtureDetected = $false
            reason = 'No controller injection is implemented. Qualification waits for a real, already-present HID/XInput capability and records only read-only evidence.'
        }
        applicationContract = [ordered]@{
            commands = @('get_state', 'set_selected', 'set_device_enabled', 'set_selected_enabled', 'restore_disabled')
            mutationsInvoked = $false
            note = 'Run the installed Controwly UI manually for selection/toggle proof after a matching device is observed; this harness deliberately does not send mutation commands.'
        }
        errors = @($final.errors)
    }
}
catch {
    $exitCode = 1
    $report = [ordered]@{
        schema = 1
        generatedAt = [DateTime]::UtcNow.ToString('o')
        platform = 'windows'
        runId = $runId
        injectionAvailable = $false
        proofMode = 'read-only-device-inventory'
        outcome = 'error'
        controllerBehaviorProof = $false
        proofScope = 'read-only-device-inventory'
        expectedFamily = $ExpectedFamily
        requestedControllerId = if ($ControllerId) { $ControllerId } else { $null }
        baselineInventory = $null
        finalInventory = $null
        matchingControllers = @()
        observations = @()
        xinputConnectedSlotCount = 0
        injection = [ordered]@{
            status = 'unavailable'
            injectionAvailable = $false
            proofMode = 'read-only-inventory'
            driverInstallAttempted = $false
            physicalHardwareAttached = $false
            virtualFixtureDetected = $false
            reason = 'The qualification harness failed before any controller mutation or injection could be attempted.'
        }
        errors = @($_.Exception.Message)
    }
}

$report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $reportPath -Encoding UTF8
[Console]::Out.WriteLine(($report | ConvertTo-Json -Depth 12))
[Console]::Out.WriteLine("qualification-report=$reportPath")
exit $exitCode
