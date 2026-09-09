#!/usr/bin/env bash
# Read-only Linux controller inventory. It never opens uinput, writes evdev,
# unbinds drivers, changes permissions, or disables a physical device.
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
OUTPUT_PATH=${CONTROWLY_LINUX_INVENTORY_OUTPUT:-$REPO_ROOT/target/controwly-qualification/linux-controller/inventory-$(date -u +%Y%m%dT%H%M%SZ).json}
REQUIRE_CONTROLLER=0
INCLUDE_RAW=0

usage() {
    cat <<'USAGE'
Usage: get-controwly-controller-inventory.sh [options]

Collect a read-only Linux HID/evdev controller inventory. Capability signals
come from udev/sysfs, not a vendor allowlist. USB, Bluetooth, Xbox, PlayStation,
and generic HID devices are represented when the host exposes them.

Options:
  --output PATH            JSON evidence path.
  --require-controller     Exit 3 when no controller capability is present.
  --include-raw            Include raw udev properties in evidence.
  --help                   Show this help.

No option injects input, opens /dev/uinput, unbinds a driver, or changes a
physical device. A present /dev/uinput node is reported but never used.
USAGE
}

fail() {
    printf 'get-controwly-controller-inventory: ERROR: %s\n' "$*" >&2
    exit 2
}

while (($# > 0)); do
    case "$1" in
        --output)
            (($# >= 2)) || fail '--output requires a path'
            OUTPUT_PATH=$2
            shift 2
            ;;
        --require-controller)
            REQUIRE_CONTROLLER=1
            shift
            ;;
        --include-raw)
            INCLUDE_RAW=1
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            fail "unknown option: $1 (use --help)"
            ;;
    esac
done

command -v python3 >/dev/null 2>&1 || fail 'python3 is required for deterministic JSON evidence'
if [[ "$OUTPUT_PATH" != /* ]]; then
    OUTPUT_PATH="$PWD/$OUTPUT_PATH"
fi
mkdir -p -- "$(dirname -- "$OUTPUT_PATH")"

python3 - "$OUTPUT_PATH" "$REQUIRE_CONTROLLER" "$INCLUDE_RAW" <<'PY'
import datetime as _dt
import hashlib
import json
import os
import pathlib
import platform
import re
import shutil
import subprocess
import sys

output_path = pathlib.Path(sys.argv[1])
require_controller = sys.argv[2] == "1"
include_raw = sys.argv[3] == "1"
errors = []


def read_text(path):
    try:
        return pathlib.Path(path).read_text(encoding="utf-8", errors="replace").strip()
    except (OSError, UnicodeError):
        return ""


def udev_properties(devnode):
    if not shutil.which("udevadm"):
        if "udevadm not found" not in errors:
            errors.append("udevadm is unavailable; only sysfs properties were inspected")
        return {}
    try:
        completed = subprocess.run(
            ["udevadm", "info", "--query=property", "--name", devnode],
            check=False,
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        errors.append(f"udevadm failed for {devnode}: {exc}")
        return {}
    if completed.returncode != 0:
        errors.append(f"udevadm returned {completed.returncode} for {devnode}")
        return {}
    values = {}
    for line in completed.stdout.splitlines():
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value
    return values


def connection_kind(properties, sysfs_target):
    bus = properties.get("ID_BUS", "").lower()
    path = properties.get("ID_PATH", "").lower()
    if bus == "usb" or "usb-" in path or "/usb" in sysfs_target.lower():
        return "USB"
    if bus in {"bluetooth", "bt"} or "bluetooth" in path or "bth" in sysfs_target.lower():
        return "Bluetooth"
    return "Unknown"


def family_for(name):
    lowered = name.lower()
    if re.search(r"xbox|xinput", lowered):
        return "Xbox"
    if re.search(r"dualsense|ps5|playstation 5", lowered):
        return "DualSense"
    if re.search(r"dualshock(?:[ -]?4)?|ps4|playstation 4", lowered):
        return "DualShock4"
    return "Generic"


def stable_id(instance):
    digest = hashlib.sha256(instance.upper().encode("utf-8", errors="replace")).hexdigest()
    return "linux-" + digest[:32]


def os_release():
    values = {}
    for line in pathlib.Path("/etc/os-release").read_text(encoding="utf-8", errors="replace").splitlines() if pathlib.Path("/etc/os-release").exists() else []:
        if "=" in line and not line.startswith("#"):
            key, value = line.split("=", 1)
            values[key] = value.strip().strip('"')
    return values


def device_record(class_name):
    sysfs = pathlib.Path("/sys/class/input") / class_name
    target = ""
    try:
        target = os.path.realpath(sysfs)
    except OSError:
        target = str(sysfs)
    try:
        input_device_target = os.path.realpath(sysfs / "device")
    except OSError:
        input_device_target = target
    devnode = pathlib.Path("/dev/input") / class_name
    properties = udev_properties(str(devnode))
    name = read_text(sysfs / "device" / "name") or properties.get("NAME", "") or class_name
    vendor = read_text(sysfs / "device" / "id" / "vendor")
    product = read_text(sysfs / "device" / "id" / "product")
    bus_code = read_text(sysfs / "device" / "id" / "bustype")
    connection = connection_kind(properties, target)
    is_js = class_name.startswith("js")
    hid = properties.get("ID_INPUT_HID") == "1" or "/hid" in target.lower() or is_js
    gamepad_signal = properties.get("ID_INPUT_GAMEPAD") == "1"
    joystick_signal = properties.get("ID_INPUT_JOYSTICK") == "1" or is_js
    name_signal = bool(re.search(r"(?i)game[ -]?pad|joystick|joypad|controller|xbox|playstation|dualshock|dualsense", name))
    candidate = bool(gamepad_signal or joystick_signal or name_signal)
    instance = properties.get("ID_PATH") or input_device_target or target or str(sysfs)
    record = {
        "stableId": stable_id(instance),
        "name": name,
        "node": str(devnode),
        "kind": "joystick" if is_js else "event",
        "connection": connection,
        "present": devnode.exists(),
        "capabilities": {
            "hid": bool(hid),
            "gamepad": bool(gamepad_signal or (is_js and name_signal)),
            "joystick": bool(joystick_signal),
            "xinput": False,
            "usb": connection == "USB",
            "bluetooth": connection == "Bluetooth",
            "fullControl": False,
        },
        "controllerCandidate": candidate,
        "family": family_for(name) if candidate else None,
        "coverage": "Uncontrolled",
        "instanceId": instance,
        "vendorId": vendor or properties.get("ID_VENDOR_ID") or None,
        "productId": product or properties.get("ID_MODEL_ID") or None,
        "evidence": {
            "virtualSysfs": "/virtual/" in target.lower() or "/uhid/" in target.lower(),
            "udevGamepadSignal": bool(gamepad_signal),
            "udevJoystickSignal": bool(joystick_signal),
            "nameSignal": bool(name_signal),
            "sysfsTarget": target,
            "busTypeCode": bus_code or None,
        },
    }
    if include_raw:
        record["evidence"]["udev"] = properties
    return record


input_root = pathlib.Path("/sys/class/input")
if input_root.exists():
    try:
        classes = sorted(
            (
                entry.name
                for entry in input_root.iterdir()
                if re.fullmatch(r"(?:event|js)[0-9]+", entry.name)
            ),
            key=lambda value: (
                value.rstrip("0123456789"),
                int(re.search(r"[0-9]+$", value).group(0)),
            ),
        )
    except OSError as exc:
        errors.append(f"unable to enumerate {input_root}: {exc}")
        classes = []
else:
    classes = []
devices = [device_record(class_name) for class_name in classes]
controllers = [device for device in devices if device["controllerCandidate"]]
controller_instances = {device["stableId"] for device in controllers}
virtual_fixture_detected = any(
    device["controllerCandidate"] and device["evidence"].get("virtualSysfs", False)
    for device in devices
)
release = os_release()
report = {
    "schema": 1,
    "generatedAt": _dt.datetime.now(_dt.timezone.utc).isoformat(),
    "platform": "linux",
    "computer": platform.node(),
    "user": os.environ.get("USER") or os.environ.get("LOGNAME"),
    "kernel": platform.release(),
    "distribution": release.get("PRETTY_NAME") or release.get("NAME"),
    "devices": devices,
    "controllerCount": len(controller_instances),
    "controllerNodeCount": len(controllers),
    "controllerBehaviorProof": False,
    "injectionAvailable": False,
    "proofMode": "read-only-device-inventory",
    "proofScope": "read-only-device-inventory",
    "injection": {
        "status": "unavailable",
        "injectionAvailable": False,
        "proofMode": "read-only-device-inventory",
        "driverInstallAttempted": False,
        "physicalHardwareAttached": False,
        "virtualFixtureDetected": virtual_fixture_detected,
        "uinputNodePresent": pathlib.Path("/dev/uinput").exists(),
        "reason": "This inventory is read-only. It never opens uinput, writes evdev, injects events, unbinds drivers, or attaches hardware; a uinput node alone is not treated as a trusted fixture.",
    },
    "applicationContract": {
        "commands": ["get_state", "set_selected", "set_device_enabled", "set_selected_enabled", "restore_disabled"],
        "mutationsInvoked": False,
        "note": "Run the installed Controwly UI manually for selection/toggle proof after a matching device is observed.",
    },
    "errors": errors,
}
with output_path.open("w", encoding="utf-8") as handle:
    json.dump(report, handle, indent=2, sort_keys=True)
    handle.write("\n")
print(json.dumps(report, indent=2, sort_keys=True))
if require_controller and not controllers:
    sys.exit(3)
PY
