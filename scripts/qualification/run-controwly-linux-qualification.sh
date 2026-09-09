#!/usr/bin/env bash
# Linux DEB package and interactive qualification. By default this only inspects
# a package. A DEB installation requires the explicit --install-deb switch and
# root; no sudo/password prompt is attempted. Controller inventory is read-only.
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
PACKAGE_PATH=''
EXPECTED_VERSION=''
OUTPUT_DIR=${CONTROWLY_LINUX_QUALIFICATION_OUTPUT:-$REPO_ROOT/target/controwly-qualification/linux}
INSTALL_DEB=0
NO_LAUNCH=0
NO_SCREENSHOT=0

usage() {
    cat <<'USAGE'
Usage: run-controwly-linux-qualification.sh --package PATH [options]

Inspect a Controwly .deb, optionally install only that .deb, launch with
isolated XDG state, capture a screenshot when a supported screen tool is
available, and collect read-only controller evidence.

Options:
  --package PATH           Controwly .deb (required).
  --expected-version VER   Optional package version.
  --install-deb            Explicitly run dpkg -i as root for this package only.
  --no-launch              Skip interactive launch.
  --no-screenshot          Skip screenshot discovery.
  --output-dir PATH        Evidence/runtime directory.
  --help                   Show this help.

No option opens /dev/uinput, injects events, unbinds a driver, changes default
input devices, attaches hardware, or removes packages/user data. DEB launches
use private XDG_CONFIG_HOME/XDG_DATA_HOME/XDG_CACHE_HOME directories.
Package installation and launch alone never constitute controller behavior proof;
the report records this boundary explicitly.
USAGE
}

fail() {
    printf 'run-controwly-linux-qualification: ERROR: %s\n' "$*" >&2
    exit 2
}

while (($# > 0)); do
    case "$1" in
        --package)
            (($# >= 2)) || fail '--package requires a path'
            PACKAGE_PATH=$2
            shift 2
            ;;
        --expected-version)
            (($# >= 2)) || fail '--expected-version requires a value'
            EXPECTED_VERSION=$2
            shift 2
            ;;
        --install-deb)
            INSTALL_DEB=1
            shift
            ;;
        --no-launch)
            NO_LAUNCH=1
            shift
            ;;
        --no-screenshot)
            NO_SCREENSHOT=1
            shift
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

[[ -n "$PACKAGE_PATH" ]] || fail '--package is required'
[[ -f "$PACKAGE_PATH" ]] || fail "package not found: $PACKAGE_PATH"
[[ "$PACKAGE_PATH" == *.deb ]] || fail 'package must end in .deb'
[[ -z "$EXPECTED_VERSION" || "$EXPECTED_VERSION" =~ ^[A-Za-z0-9._+-]+$ ]] || fail 'expected version contains invalid characters'
command -v python3 >/dev/null 2>&1 || fail 'python3 is required for deterministic JSON evidence'
mkdir -p -- "$OUTPUT_DIR"
OUTPUT_DIR=$(readlink -f -- "$OUTPUT_DIR") || fail "cannot resolve output directory: $OUTPUT_DIR"

package_path=$(readlink -f -- "$PACKAGE_PATH")
run_id=$(date -u +%Y%m%dT%H%M%SZ)-$$
report_path="$OUTPUT_DIR/qualification-$run_id.json"
package_info_path="$OUTPUT_DIR/package-$run_id.txt"
inventory_path="$OUTPUT_DIR/controller-inventory-$run_id.json"
launch_log="$OUTPUT_DIR/launch-$run_id.log"
runtime_root="$OUTPUT_DIR/runtime-$run_id"
mkdir -p -- "$runtime_root/config" "$runtime_root/data" "$runtime_root/cache"

package_type=deb
package_name=''
package_version=''
package_architecture=''
launch_path=''
install_status='not-requested'
install_exit_code=''

if [[ "$package_path" == *.deb ]]; then
    package_type=deb
    command -v dpkg-deb >/dev/null 2>&1 || fail 'dpkg-deb is required to inspect .deb packages'
    package_name=$(dpkg-deb -f "$package_path" Package)
    package_version=$(dpkg-deb -f "$package_path" Version)
    package_architecture=$(dpkg-deb -f "$package_path" Architecture)
    dpkg-deb -I "$package_path" >"$package_info_path"
    [[ "$package_name" == controwly ]] || fail "refusing package name '$package_name'; expected exact Controwly package identity"
    if [[ -n "$EXPECTED_VERSION" && "$package_version" != "$EXPECTED_VERSION" ]]; then
        fail "package version '$package_version' does not match expected '$EXPECTED_VERSION'"
    fi
    if ((INSTALL_DEB == 1)); then
        [[ $EUID -eq 0 ]] || fail '--install-deb requires root; rerun explicitly as root (no sudo/password handling is attempted)'
        set +e
        dpkg -i -- "$package_path" >"$OUTPUT_DIR/dpkg-$run_id.log" 2>&1
        install_exit_code=$?
        set -e
        if ((install_exit_code != 0)); then
            fail "dpkg -i failed with exit code $install_exit_code; package was not removed or rolled back"
        fi
        install_status=installed
    fi
    # Read package contents to find the installed executable without assuming a
    # desktop path; only the exact package's own file list is inspected.
    launch_path=$(dpkg-deb -c "$package_path" | awk '$NF ~ /^\/usr\/bin\// && $NF !~ /\/$/ {print $NF; exit}')
    if [[ -z "$launch_path" ]]; then
        launch_path=/usr/bin/controwly
    fi
fi

launch_outcome='not-requested'
launch_pid=''
screenshot_path=''
existing_process_count=0
if command -v pgrep >/dev/null 2>&1; then
    existing_process_count=$(pgrep -x controwly 2>/dev/null | wc -l | tr -d ' ' || true)
fi

if ((NO_LAUNCH == 0)); then
    if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
        launch_outcome='blocked-no-display'
    elif [[ "$package_type" == deb && ! -x "$launch_path" ]]; then
        launch_outcome='blocked-executable-not-installed'
    elif [[ ! -x "$launch_path" ]]; then
        launch_outcome='blocked-executable-not-runnable'
    else
        set +e
        env XDG_CONFIG_HOME="$runtime_root/config" XDG_DATA_HOME="$runtime_root/data" XDG_CACHE_HOME="$runtime_root/cache" \
            "$launch_path" >"$launch_log" 2>&1 &
        launch_pid=$!
        set -e
        launch_outcome='started'
        sleep 8
        if ! kill -0 "$launch_pid" 2>/dev/null; then
            launch_outcome='exited-during-observation'
        fi

        if ((NO_SCREENSHOT == 0)); then
            screenshot_path="$OUTPUT_DIR/interactive-$run_id.png"
            if command -v grim >/dev/null 2>&1 && [[ -n "${WAYLAND_DISPLAY:-}" ]]; then
                grim "$screenshot_path" >/dev/null 2>&1 || screenshot_path=''
            elif command -v gnome-screenshot >/dev/null 2>&1; then
                gnome-screenshot -f "$screenshot_path" >/dev/null 2>&1 || screenshot_path=''
            elif command -v spectacle >/dev/null 2>&1; then
                spectacle -b -n -o "$screenshot_path" >/dev/null 2>&1 || screenshot_path=''
            elif command -v import >/dev/null 2>&1 && [[ -n "${DISPLAY:-}" ]]; then
                import -window root "$screenshot_path" >/dev/null 2>&1 || screenshot_path=''
            else
                screenshot_path=''
            fi
        fi
    fi
fi

if [[ -n "$launch_pid" ]] && kill -0 "$launch_pid" 2>/dev/null; then
    kill "$launch_pid" 2>/dev/null || true
    wait "$launch_pid" 2>/dev/null || true
fi

set +e
"$SCRIPT_DIR/get-controwly-controller-inventory.sh" --output "$inventory_path" >/dev/null
inventory_exit_code=$?
set -e

python3 - "$report_path" "$package_path" "$package_type" "$package_name" "$package_version" "$package_architecture" "$install_status" "$install_exit_code" "$launch_outcome" "$launch_pid" "$screenshot_path" "$inventory_path" "$inventory_exit_code" "$existing_process_count" "$EXPECTED_VERSION" "$NO_LAUNCH" <<'PY'
import datetime as _dt
import json
import pathlib
import sys

(
    report_path, package_path, package_type, package_name, package_version,
    package_architecture, install_status, install_exit_code, launch_outcome,
    launch_pid, screenshot_path, inventory_path, inventory_exit_code,
    existing_process_count, expected_version, no_launch,
) = sys.argv[1:]
try:
    inventory = json.loads(pathlib.Path(inventory_path).read_text(encoding="utf-8"))
except (OSError, ValueError) as exc:
    inventory = {"errors": [f"controller inventory was not readable: {exc}"], "controllerCount": 0}
controller_behavior_proof = False
if inventory_exit_code not in ("0", "3"):
    outcome = "error-controller-inventory"
elif launch_outcome == "started":
    outcome = "launched-no-behavior-proof"
elif launch_outcome == "exited-during-observation":
    outcome = "error-launch-exited"
elif launch_outcome.startswith("blocked"):
    outcome = launch_outcome
else:
    outcome = "inspected"
report = {
    "schema": 1,
    "controllerBehaviorProof": controller_behavior_proof,
    "injectionAvailable": False,
    "proofMode": "package-and-read-only-device-inventory",
    "proofScope": "package-and-read-only-device-inventory",
    "generatedAt": _dt.datetime.now(_dt.timezone.utc).isoformat(),
    "platform": "linux",
    "outcome": outcome,
    "package": {
        "path": package_path,
        "type": package_type,
        "name": package_name,
        "version": package_version or None,
        "expectedVersion": expected_version or None,
        "architecture": package_architecture,
        "installStatus": install_status,
        "installExitCode": int(install_exit_code) if install_exit_code else None,
    },
    "interactive": {
        "requested": no_launch != "1",
        "outcome": launch_outcome,
        "pid": int(launch_pid) if launch_pid.isdigit() else None,
        "screenshot": screenshot_path or None,
        "existingControwlyProcessCountBeforeLaunch": int(existing_process_count),
        "isolatedXdgState": True,
    },
    "controllerInventory": inventory_path,
    "controllerCount": inventory.get("controllerCount", 0),
    "controllerNodeCount": inventory.get("controllerNodeCount", 0),
    "injection": {
        "status": "unavailable",
        "injectionAvailable": False,
        "proofMode": "read-only-device-inventory",
        "driverInstallAttempted": False,
        "physicalHardwareAttached": False,
        "virtualFixtureDetected": inventory.get("injection", {}).get("virtualFixtureDetected", False),
        "reason": "This runner only reads udev/sysfs evidence. It never opens uinput, injects events, unbinds drivers, or attaches hardware.",
    },
    "applicationContract": {
        "commands": ["get_state", "set_selected", "set_device_enabled", "set_selected_enabled", "restore_disabled"],
        "mutationsInvoked": False,
    },
    "errors": inventory.get("errors", []),
}
pathlib.Path(report_path).write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
print(json.dumps(report, indent=2, sort_keys=True))
if outcome.startswith("error-"):
    raise SystemExit(1)
if outcome != "inspected":
    raise SystemExit(3)
PY
