[CmdletBinding()]
param(
    [Parameter()]
    [string]$OutputPath,

    [Parameter()]
    [ValidateRange(1, 4096)]
    [int]$MaxDevices = 512,

    [Parameter()]
    [switch]$IncludeRaw,

    [Parameter()]
    [switch]$RequireController
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($OutputPath) {
    $OutputPath = [IO.Path]::GetFullPath($OutputPath)
}

$script:errors = [System.Collections.Generic.List[string]]::new()

function Add-InventoryError {
    param([Parameter(Mandatory = $true)][string]$Message)
    [void]$script:errors.Add($Message)
}

function Get-StringArray {
    param([AllowNull()][object]$Value)
    if ($null -eq $Value) {
        return @()
    }
    return @($Value | ForEach-Object { [string]$_ } | Where-Object { $_.Length -gt 0 })
}

function Get-PropertyValue {
    param(
        [Parameter(Mandatory = $true)][object]$Object,
        [Parameter(Mandatory = $true)][string]$Name
    )
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) {
        return $null
    }
    return $property.Value
}

function Get-ConnectionKind {
    param([Parameter(Mandatory = $true)][string]$InstanceId)
    if ($InstanceId -match '(?i)^(USB\\|USBSTOR\\)') {
        return 'USB'
    }
    if ($InstanceId -match '(?i)^(BTH\\|BTHENUM\\|Bluetooth\\)') {
        return 'Bluetooth'
    }
    return 'Unknown'
}

function Get-StableId {
    param([Parameter(Mandatory = $true)][string]$InstanceId)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($InstanceId.ToUpperInvariant())
        $digest = [System.BitConverter]::ToString($sha.ComputeHash($bytes)).Replace('-', '').ToLowerInvariant()
        return "windows-$($digest.Substring(0, 32))"
    }
    finally {
        $sha.Dispose()
    }
}

function Get-PnpRecords {
    $records = @()
    try {
        $records = @(Get-PnpDevice -PresentOnly -ErrorAction Stop | Where-Object {
            $_.Class -in @('HIDClass', 'Bluetooth', 'USB', 'GamePort') -or
            $_.FriendlyName -match '(?i)game[ -]?pad|joystick|joypad|controller|xbox|playstation|dualshock|dualsense'
        })
    }
    catch {
        Add-InventoryError "Get-PnpDevice failed: $($_.Exception.Message)"
    }

    if ($records.Count -eq 0) {
        try {
            $records = @(Get-CimInstance -ClassName Win32_PnPEntity -ErrorAction Stop | Where-Object {
                $_.PNPClass -in @('HIDClass', 'Bluetooth', 'USB', 'GamePort') -or
                $_.Name -match '(?i)game[ -]?pad|joystick|joypad|controller|xbox|playstation|dualshock|dualsense'
            } | ForEach-Object {
                [pscustomobject]@{
                    Status = if ($_.ConfigManagerErrorCode -eq 0) { 'OK' } else { 'Error' }
                    Class = $_.PNPClass
                    FriendlyName = $_.Name
                    InstanceId = $_.PNPDeviceID
                    Problem = $_.ConfigManagerErrorCode
                }
            })
        }
        catch {
            Add-InventoryError "Win32_PnPEntity fallback failed: $($_.Exception.Message)"
        }
    }
    return @($records | Select-Object -First $MaxDevices)
}

function Get-PnpDetails {
    $details = @{}
    try {
        foreach ($item in @(Get-CimInstance -ClassName Win32_PnPEntity -ErrorAction Stop)) {
            $id = [string]$item.PNPDeviceID
            if ($id.Length -gt 0) {
                $details[$id] = $item
            }
        }
    }
    catch {
        Add-InventoryError "Win32_PnPEntity details unavailable: $($_.Exception.Message)"
    }
    return $details
}

function Get-DeviceProperty {
    param(
        [Parameter(Mandatory = $true)][string]$InstanceId,
        [Parameter(Mandatory = $true)][string]$KeyName
    )
    try {
        $property = Get-PnpDeviceProperty -InstanceId $InstanceId -KeyName $KeyName -ErrorAction Stop
        return $property.Data
    }
    catch {
        return $null
    }
}

function Get-XInputEvidence {
    $result = [ordered]@{
        api = 'xinput1_4.dll'
        available = $false
        connectedSlots = @()
        error = $null
    }

    try {
        if (-not ('ControwlyXInputProbe' -as [type])) {
            Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class ControwlyXInputProbe {
    [StructLayout(LayoutKind.Sequential)]
    public struct XInputGamepad {
        public ushort wButtons;
        public byte bLeftTrigger;
        public byte bRightTrigger;
        public short sThumbLX;
        public short sThumbLY;
        public short sThumbRX;
        public short sThumbRY;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct XInputState {
        public uint dwPacketNumber;
        public XInputGamepad Gamepad;
    }
    [DllImport("xinput1_4.dll", EntryPoint = "XInputGetState")]
    public static extern uint GetState(uint dwUserIndex, ref XInputState pState);
}
'@ -ErrorAction Stop
        }

        $result.available = $true
        $connected = [System.Collections.Generic.List[int]]::new()
        for ($slot = 0; $slot -lt 4; $slot++) {
            $state = New-Object 'ControwlyXInputProbe+XInputState'
            $code = [ControwlyXInputProbe]::GetState([uint32]$slot, [ref]$state)
            if ($code -eq 0) {
                [void]$connected.Add($slot)
            }
        }
        $result.connectedSlots = @($connected)
    }
    catch {
        $result.error = $_.Exception.Message
    }
    return [pscustomobject]$result
}

function Get-SessionEvidence {
    try {
        $output = @(quser 2>&1 | Out-String).Trim()
        return $output
    }
    catch {
        Add-InventoryError "Unable to query Windows sessions: $($_.Exception.Message)"
        return $null
    }
}

$report = $null
$exitCode = 0
try {
    $os = Get-CimInstance -ClassName Win32_OperatingSystem -ErrorAction Stop
    $computer = Get-CimInstance -ClassName Win32_ComputerSystem -ErrorAction Stop
    $pnpDetails = Get-PnpDetails
    $records = Get-PnpRecords
    $devices = [System.Collections.Generic.List[object]]::new()

    foreach ($record in $records) {
        $instanceId = [string](Get-PropertyValue -Object $record -Name 'InstanceId')
        if ($instanceId.Length -eq 0) {
            continue
        }
        $detail = $null
        if ($pnpDetails.ContainsKey($instanceId)) {
            $detail = $pnpDetails[$instanceId]
        }
        $friendlyName = [string](Get-PropertyValue -Object $record -Name 'FriendlyName')
        if ($friendlyName.Length -eq 0 -and $null -ne $detail) {
            $friendlyName = [string]$detail.Name
        }
        $class = [string](Get-PropertyValue -Object $record -Name 'Class')
        if ($class.Length -eq 0 -and $null -ne $detail) {
            $class = [string]$detail.PNPClass
        }
        $hardwareIds = @()
        $compatibleIds = @()
        if ($null -ne $detail) {
            $hardwareIds = @(Get-StringArray (Get-PropertyValue -Object $detail -Name 'HardwareID'))
            $compatibleIds = @(Get-StringArray (Get-PropertyValue -Object $detail -Name 'CompatibleID'))
        }
        if ($compatibleIds.Count -eq 0) {
            $compatible = Get-DeviceProperty -InstanceId $instanceId -KeyName 'DEVPKEY_Device_CompatibleIds'
            $compatibleIds = @(Get-StringArray $compatible)
        }

        $connection = Get-ConnectionKind -InstanceId $instanceId
        $isHid = $class -eq 'HIDClass' -or $instanceId -match '(?i)^HID\\'
        $nameSignal = $friendlyName -match '(?i)game[ -]?pad|joystick|joypad|controller|xbox|playstation|dualshock|dualsense'
        $usageSignal = ($compatibleIds -join ';') -match '(?i)HID_DEVICE_SYSTEM_GAME|HID_DEVICE_UP:0001_U:0004|HID_DEVICE_UP:0001_U:0005'
        $gamepadCandidate = $isHid -and ($nameSignal -or $usageSignal)
        $isJoystick = $gamepadCandidate -and ($friendlyName -match '(?i)joystick|joypad' -or $usageSignal)

        $device = [ordered]@{
            stableId = Get-StableId -InstanceId $instanceId
            name = if ($friendlyName) { $friendlyName } else { 'Unnamed HID/USB device' }
            connection = $connection
            status = [string](Get-PropertyValue -Object $record -Name 'Status')
            problemCode = Get-PropertyValue -Object $record -Name 'Problem'
            instanceId = $instanceId
            class = $class
            capabilities = [ordered]@{
                hid = [bool]$isHid
                gamepad = [bool]$gamepadCandidate
                joystick = [bool]$isJoystick
                xinput = $false
                usb = $connection -eq 'USB'
                bluetooth = $connection -eq 'Bluetooth'
                fullControl = $false
            }
            coverage = 'Uncontrolled'
            evidence = [ordered]@{
                friendlyNameSignal = [bool]$nameSignal
                usagePageSignal = [bool]$usageSignal
                hardwareIdCount = $hardwareIds.Count
                compatibleIdCount = $compatibleIds.Count
            }
        }
        if ($IncludeRaw) {
            $device.evidence.hardwareIds = $hardwareIds
            $device.evidence.compatibleIds = $compatibleIds
        }
        [void]$devices.Add([pscustomobject]$device)
    }

    $xinput = Get-XInputEvidence
    $controllerDevices = @($devices | Where-Object {
        $_.capabilities.gamepad -or $_.capabilities.joystick -or $_.capabilities.xinput
    })
    if ($RequireController -and $controllerDevices.Count -eq 0) {
        $exitCode = 3
    }

    $report = [ordered]@{
        schema = 1
        generatedAt = [DateTime]::UtcNow.ToString('o')
        platform = 'windows'
        computer = [string]$computer.Name
        user = [Environment]::UserName
        os = [string]$os.Caption
        osVersion = [string]$os.Version
        sessions = Get-SessionEvidence
        devices = @($devices)
        controllerCount = $controllerDevices.Count
        xinput = $xinput
        xinputConnectedSlotCount = @($xinput.connectedSlots).Count
        controllerBehaviorProof = $false
        injectionAvailable = $false
        proofMode = 'read-only-device-inventory'
        injection = [ordered]@{
            status = 'unavailable'
            injectionAvailable = $false
            proofMode = 'read-only-device-inventory'
            driverInstallAttempted = $false
            physicalHardwareAttached = $false
            virtualFixtureDetected = $false
            reason = 'This inventory is read-only. No controller injection, virtual HID creation, driver installation, or hardware attachment is attempted.'
        }
        applicationContract = [ordered]@{
            get_state = 'not invoked; run against an installed Controwly build'
            set_selected = 'not invoked; mutation requires an explicit app qualification step'
            set_device_enabled = 'not invoked; mutation requires an explicit app qualification step'
            restore_disabled = 'not invoked; mutation requires an explicit app qualification step'
        }
        errors = @($script:errors)
    }
}
catch {
    Add-InventoryError $_.Exception.Message
    $exitCode = 1
    $report = [ordered]@{
        schema = 1
        generatedAt = [DateTime]::UtcNow.ToString('o')
        platform = 'windows'
        devices = @()
        controllerCount = 0
        xinputConnectedSlotCount = 0
        xinput = [ordered]@{
            available = $false
            connectedSlots = @()
            error = 'inventory failed before XInput evidence was collected'
        }
        sessions = @()
        controllerBehaviorProof = $false
        injectionAvailable = $false
        proofMode = 'read-only-device-inventory'
        injection = [ordered]@{
            status = 'unavailable'
            injectionAvailable = $false
            proofMode = 'read-only-device-inventory'
            driverInstallAttempted = $false
            physicalHardwareAttached = $false
            virtualFixtureDetected = $false
            reason = 'Inventory failed before device mutation could be considered.'
        }
        errors = @($script:errors)
    }
}

$json = $report | ConvertTo-Json -Depth 12
if ($OutputPath) {
    $parent = Split-Path -Parent $OutputPath
    if ($parent) {
        New-Item -ItemType Directory -Force -Path $parent | Out-Null
    }
    Set-Content -LiteralPath $OutputPath -Value $json -Encoding UTF8
}
[Console]::Out.WriteLine($json)
exit $exitCode
