#!/usr/bin/env bash
# Read-only eventual Linux controller qualification. It waits for a real device
# to appear; it never injects input, opens uinput, unbinds drivers, or changes a
# physical host controller.
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
INVENTORY_SCRIPT=${CONTROWLY_LINUX_INVENTORY_SCRIPT:-$SCRIPT_DIR/get-controwly-controller-inventory.sh}
OUTPUT_DIR=${CONTROWLY_LINUX_QUALIFICATION_OUTPUT:-$REPO_ROOT/target/controwly-qualification/linux-controller}
WAIT_SECONDS=${CONTROWLY_LINUX_WAIT_SECONDS:-0}
EXPECTED_FAMILY=Any
CONTROLLER_ID=''

usage() {
    cat <<'USAGE'
Usage: qualify-controwly-linux.sh [options]

Poll a read-only Linux HID/evdev inventory for a controller capability. The
family labels are evidence hints, not a vendor allowlist. USB and Bluetooth
are distinguished using udev/sysfs bus properties.

Options:
  --wait-seconds N          Poll for at most N seconds (default: 0).
  --expected-family NAME    Any, Xbox, DualSense, DualShock4, or Generic.
  --controller-id ID        Match stableId or instanceId exactly.
  --output-dir PATH         Evidence directory.
  --help                    Show this help.

Exit status 3 means no matching controller was observed or a matching device
was observed without application behavior proof; 1 means collection failed.
No status performs input injection or device mutation.
USAGE
}

fail() {
    printf 'qualify-controwly-linux: ERROR: %s\n' "$*" >&2
    exit 1
}

while (($# > 0)); do
    case "$1" in
        --wait-seconds)
            (($# >= 2)) || fail '--wait-seconds requires a value'
            WAIT_SECONDS=$2
            shift 2
            ;;
        --expected-family)
            (($# >= 2)) || fail '--expected-family requires a value'
            EXPECTED_FAMILY=$2
            shift 2
            ;;
        --controller-id)
            (($# >= 2)) || fail '--controller-id requires a value'
            CONTROLLER_ID=$2
            shift 2
            ;;
        --output-dir)
            (($# >= 2)) || fail '--output-dir requires a path'
            OUTPUT_DIR=$2
            shift 2
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

[[ $WAIT_SECONDS =~ ^[0-9]+$ ]] || fail '--wait-seconds must be a non-negative integer'
case "$EXPECTED_FAMILY" in
    Any|Xbox|DualSense|DualShock4|Generic) ;;
    *) fail '--expected-family must be Any, Xbox, DualSense, DualShock4, or Generic' ;;
esac
command -v python3 >/dev/null 2>&1 || fail 'python3 is required'
[[ -x "$INVENTORY_SCRIPT" ]] || fail "inventory script is not executable: $INVENTORY_SCRIPT"
mkdir -p -- "$OUTPUT_DIR"
INVENTORY_SCRIPT=$(readlink -f -- "$INVENTORY_SCRIPT") || fail "cannot resolve inventory script: $INVENTORY_SCRIPT"
OUTPUT_DIR=$(readlink -f -- "$OUTPUT_DIR") || fail "cannot resolve output directory: $OUTPUT_DIR"

run_id=$(date -u +%Y%m%dT%H%M%SZ)-$$
observations_path="$OUTPUT_DIR/qualification-$run_id-observations.tsv"
report_path="$OUTPUT_DIR/qualification-$run_id.json"
: >"$observations_path"

deadline=$(( $(date +%s) + WAIT_SECONDS ))
final_inventory=''
matching_count=0
collection_failed=0
while :; do
    observation_index=$(wc -l <"$observations_path" | tr -d ' ')
    inventory_path="$OUTPUT_DIR/inventory-$run_id-$observation_index.json"
    set +e
    "$INVENTORY_SCRIPT" --output "$inventory_path" >/dev/null
    inventory_rc=$?
    set -e
    if ((inventory_rc != 0 && inventory_rc != 3)); then
        collection_failed=1
        final_inventory=$inventory_path
        printf '%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" 'collection-error' >>"$observations_path"
        break
    fi
    final_inventory=$inventory_path
    matching_count=$(python3 - "$inventory_path" "$EXPECTED_FAMILY" "$CONTROLLER_ID" <<'PY'
import json
import re
import sys

path, expected, requested = sys.argv[1:]
try:
    inventory = json.load(open(path, encoding="utf-8"))
except (OSError, ValueError):
    print(0)
    raise SystemExit(0)

def family(device):
    name = str(device.get("name", ""))
    if re.search(r"xbox|xinput", name, re.I):
        return "Xbox"
    if re.search(r"dualsense|ps5|playstation 5", name, re.I):
        return "DualSense"
    if re.search(r"dualshock(?:[ -]?4)?|ps4|playstation 4", name, re.I):
        return "DualShock4"
    return "Generic"

candidates_by_id = {}
for device in inventory.get("devices", []):
    capabilities = device.get("capabilities", {})
    if not (device.get("controllerCandidate") or capabilities.get("gamepad") or capabilities.get("joystick")):
        continue
    if requested and requested not in (str(device.get("stableId", "")), str(device.get("instanceId", ""))):
        continue
    if expected != "Any" and family(device) != expected:
        continue
    key = str(device.get("stableId") or device.get("instanceId") or device.get("node") or "")
    candidates_by_id.setdefault(key, device)
print(len(candidates_by_id))
PY
)
    printf '%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$matching_count" >>"$observations_path"
    if ((matching_count > 0 || $(date +%s) >= deadline)); then
        break
    fi
    sleep 3
done

python3 - "$final_inventory" "$observations_path" "$report_path" "$run_id" "$EXPECTED_FAMILY" "$CONTROLLER_ID" "$WAIT_SECONDS" "$collection_failed" <<'PY'
import datetime as _dt
import json
import pathlib
import re
import sys

inventory_path, observations_path, report_path, run_id, expected, requested, wait_seconds, collection_failed = sys.argv[1:]
try:
    inventory = json.loads(pathlib.Path(inventory_path).read_text(encoding="utf-8"))
except (OSError, ValueError) as exc:
    inventory = {"devices": [], "errors": [str(exc)]}

def family(device):
    name = str(device.get("name", ""))
    if re.search(r"xbox|xinput", name, re.I):
        return "Xbox"
    if re.search(r"dualsense|ps5|playstation 5", name, re.I):
        return "DualSense"
    if re.search(r"dualshock(?:[ -]?4)?|ps4|playstation 4", name, re.I):
        return "DualShock4"
    return "Generic"

matching_by_id = {}
for device in inventory.get("devices", []):
    capabilities = device.get("capabilities", {})
    if not (device.get("controllerCandidate") or capabilities.get("gamepad") or capabilities.get("joystick")):
        continue
    if requested and requested not in (str(device.get("stableId", "")), str(device.get("instanceId", ""))):
        continue
    if expected != "Any" and family(device) != expected:
        continue
    key = str(device.get("stableId") or device.get("instanceId") or device.get("node") or "")
    matching_by_id.setdefault(key, {
        "stableId": device.get("stableId"),
        "instanceId": device.get("instanceId"),
        "name": device.get("name"),
        "family": family(device),
        "connection": device.get("connection"),
        "capabilities": device.get("capabilities"),
        "coverage": device.get("coverage"),
    })
matching = list(matching_by_id.values())
observations = []
try:
    for line in pathlib.Path(observations_path).read_text(encoding="utf-8").splitlines():
        if "\t" in line:
            at, count = line.split("\t", 1)
            observations.append({"at": at, "matchingControllerCount": int(count) if count.isdigit() else None})
except OSError:
    pass
controller_behavior_proof = False
outcome = "error" if collection_failed == "1" else ("observed-no-behavior-proof" if matching else "blocked-no-matching-controller")
report = {
    "schema": 1,
    "generatedAt": _dt.datetime.now(_dt.timezone.utc).isoformat(),
    "platform": "linux",
    "runId": run_id,
    "outcome": outcome,
    "injectionAvailable": False,
    "proofMode": "read-only-device-inventory",
    "controllerBehaviorProof": controller_behavior_proof,
    "proofScope": "read-only-device-inventory",
    "controllerCount": inventory.get("controllerCount", 0),
    "controllerNodeCount": inventory.get("controllerNodeCount", 0),
    "expectedFamily": expected,
    "requestedControllerId": requested or None,
    "waitSeconds": int(wait_seconds),
    "finalInventory": inventory_path,
    "matchingControllers": matching,
    "observations": observations,
    "injection": {
        "status": "unavailable",
        "driverInstallAttempted": False,
        "physicalHardwareAttached": False,
        "injectionAvailable": False,
        "proofMode": "read-only-device-inventory",
        "virtualFixtureDetected": inventory.get("injection", {}).get("virtualFixtureDetected", False),
        "reason": "No controller injection is implemented. Qualification observes only already-present udev/sysfs/evdev evidence and does not open uinput or alter drivers.",
    },
    "applicationContract": {
        "commands": ["get_state", "set_selected", "set_device_enabled", "set_selected_enabled", "restore_disabled"],
        "mutationsInvoked": False,
        "note": "Run the installed Controwly UI manually for selection/toggle proof after a matching device is observed.",
    },
    "errors": inventory.get("errors", []),
}
pathlib.Path(report_path).write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
print(json.dumps(report, indent=2, sort_keys=True))
if outcome == "error":
    raise SystemExit(1)
if outcome != "error":
    raise SystemExit(3)
PY
