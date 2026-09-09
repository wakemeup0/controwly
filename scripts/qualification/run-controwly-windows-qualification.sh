#!/usr/bin/env bash
# Stage and run Controwly qualification on the existing Windows VM. The VM
# must already be running unless --start is explicitly supplied; --start uses
# plain virsh start and never discards managed-save state.
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
VM_HELPER=$SCRIPT_DIR/controwly-windows-vm.sh
SSH_ALIAS=${CONTROWLY_SSH_ALIAS:-hooviestar-win11}
VM_USER=${CONTROWLY_VM_USER:-}
OUTPUT_DIR=${CONTROWLY_WINDOWS_QUALIFICATION_OUTPUT:-$REPO_ROOT/target/controwly-qualification/windows}
INSTALLER_PATH=''
EXPECTED_VERSION=''
INSTALL_DIRECTORY=''
EXECUTABLE_PATH=''
START_VM=0
REQUIRE_SIGNATURE=0
REQUIRE_ELEVATED=1

usage() {
    cat <<'USAGE'
Usage: run-controwly-windows-qualification.sh --installer PATH [options]

Stage a Controwly NSIS installer and read-only qualification scripts on the
configured Windows VM, install it silently, launch it in the active desktop,
collect controller evidence, and capture a host-side SPICE screenshot.

Options:
  --installer PATH         Local NSIS installer (required).
  --expected-version VER   Optional exact installed file version.
  --install-directory PATH Guest install directory (optional).
  --executable PATH        Guest executable path (optional).
  --require-signature      Require a valid Authenticode signature.
  --allow-un-elevated      Do not require an elevated token for a custom path.
  --start                  Start a shut-off VM using plain virsh start.
  --output-dir PATH        Host evidence directory.
  --help                   Show this help.

The runner never calls virsh --force-boot, managedsave-remove, destroy, reset,
attach-device, or detach-device. It never stops an existing process, removes
user data, changes default devices, installs drivers, or attaches hardware.
USAGE
}

fail() {
    printf 'run-controwly-windows-qualification: ERROR: %s\n' "$*" >&2
    exit 2
}
encode_powershell() {
    python3 - "$1" <<'PY'
import base64
import sys

print(base64.b64encode(sys.argv[1].encode("utf-16le")).decode("ascii"))
PY
}

invoke_remote_code() {
    local encoded
    encoded=$(encode_powershell "$1")
    ssh "$SSH_ALIAS" powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand "$encoded"
}

invoke_remote_script() {
    local script_path=$1
    shift
    local encoded
    encoded=$(python3 - "$script_path" "$@" <<'PY'
import base64
import sys

script_path, *specification = sys.argv[1:]

def literal(value):
    return "'" + value.replace("'", "''") + "'"

parts = []
for item in specification:
    if "=" in item:
        key, value = item.split("=", 1)
        if not key.isidentifier():
            raise SystemExit("invalid remote PowerShell parameter name")
        parts.extend(("-" + key, literal(value)))
    else:
        if not item.isidentifier():
            raise SystemExit("invalid remote PowerShell switch name")
        parts.append("-" + item)

command = "& " + literal(script_path)
if parts:
    command += " " + " ".join(parts)
command += "; $exitCode = if ($null -eq $LASTEXITCODE) { 0 } else { $LASTEXITCODE }; exit $exitCode"
print(base64.b64encode(command.encode("utf-16le")).decode("ascii"))
PY
)
    ssh "$SSH_ALIAS" powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand "$encoded"
}


while (($# > 0)); do
    case "$1" in
        --installer)
            (($# >= 2)) || fail '--installer requires a path'
            INSTALLER_PATH=$2
            shift 2
            ;;
        --expected-version)
            (($# >= 2)) || fail '--expected-version requires a value'
            EXPECTED_VERSION=$2
            shift 2
            ;;
        --install-directory)
            (($# >= 2)) || fail '--install-directory requires a path'
            INSTALL_DIRECTORY=$2
            shift 2
            ;;
        --executable)
            (($# >= 2)) || fail '--executable requires a path'
            EXECUTABLE_PATH=$2
            shift 2
            ;;
        --require-signature)
            REQUIRE_SIGNATURE=1
            shift
            ;;
        --allow-un-elevated)
            REQUIRE_ELEVATED=0
            shift
            ;;
        --start)
            START_VM=1
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

[[ -n "$INSTALLER_PATH" ]] || fail '--installer is required'
[[ -f "$INSTALLER_PATH" ]] || fail "installer not found: $INSTALLER_PATH"
[[ "$INSTALLER_PATH" == *.exe ]] || fail 'installer must be an .exe file'
[[ -z "$EXPECTED_VERSION" || "$EXPECTED_VERSION" =~ ^[A-Za-z0-9._+-]+$ ]] || fail 'expected version contains invalid characters'
[[ -z "$VM_USER" || "$VM_USER" =~ ^[A-Za-z0-9._-]+$ ]] || fail 'CONTROWLY_VM_USER contains invalid characters'

for command_name in ssh scp python3; do
    command -v "$command_name" >/dev/null 2>&1 || fail "required command not found: $command_name"
done
[[ -x "$VM_HELPER" ]] || fail "VM helper is not executable: $VM_HELPER"
mkdir -p -- "$OUTPUT_DIR"

if [[ -z "$VM_USER" ]]; then
    VM_USER=$(ssh -G "$SSH_ALIAS" | awk '$1 == "user" { print $2; exit }')
fi
[[ "$VM_USER" =~ ^[A-Za-z0-9._-]+$ ]] || fail "could not determine a safe Windows SSH user for $SSH_ALIAS"

if ((START_VM == 1)); then
    "$VM_HELPER" --start --no-screenshot --output-dir "$OUTPUT_DIR/vm-readiness"
else
    "$VM_HELPER" --no-screenshot --output-dir "$OUTPUT_DIR/vm-readiness"
fi

run_id=$(date -u +%Y%m%dT%H%M%SZ)-$$
remote_stage="C:/Users/$VM_USER/Downloads/Controwly-Qualification/$run_id"
local_installer=$(readlink -f -- "$INSTALLER_PATH")
remote_installer="$remote_stage/$(basename -- "$local_installer")"

invoke_remote_code "New-Item -ItemType Directory -Force -Path '$remote_stage' | Out-Null"
scp "$local_installer" "$SSH_ALIAS:$remote_stage/"
scp "$SCRIPT_DIR/Install-Controwly.ps1" "$SCRIPT_DIR/Invoke-ControwlyInteractive.ps1" "$SCRIPT_DIR/Get-ControwlyControllerInventory.ps1" "$SCRIPT_DIR/Invoke-ControwlyControllerQualification.ps1" "$SSH_ALIAS:$remote_stage/"

printf 'run-controwly-windows-qualification: staged installer=%s\n' "$remote_installer"
printf 'run-controwly-windows-qualification: remote evidence root=%s\n' "$remote_stage"

install_specification=(
    "InstallerPath=$remote_installer"
    "OutputPath=$remote_stage/installer-install.json"
)
if [[ -n "$EXPECTED_VERSION" ]]; then
    install_specification+=("ExpectedVersion=$EXPECTED_VERSION")
fi
if [[ -n "$INSTALL_DIRECTORY" ]]; then
    install_specification+=("InstallDirectory=$INSTALL_DIRECTORY")
fi
if [[ "$REQUIRE_SIGNATURE" == 1 ]]; then
    install_specification+=(RequireSignature)
fi
if ((REQUIRE_ELEVATED == 1)); then
    install_specification+=(RequireElevation)
fi
invoke_remote_script "$remote_stage/Install-Controwly.ps1" "${install_specification[@]}" \
    | tee "$OUTPUT_DIR/installer-install-$run_id.log"

if [[ -z "$EXECUTABLE_PATH" ]]; then
    if [[ -n "$INSTALL_DIRECTORY" ]]; then
        EXECUTABLE_PATH="$INSTALL_DIRECTORY/Controwly.exe"
    else
        EXECUTABLE_PATH="C:/Program Files/Controwly/Controwly.exe"
    fi
fi

invoke_remote_script "$remote_stage/Invoke-ControwlyInteractive.ps1" \
    "ExecutablePath=$EXECUTABLE_PATH" \
    "OutputDirectory=$remote_stage/interactive" \
    | tee "$OUTPUT_DIR/interactive-$run_id.log"

invoke_remote_script "$remote_stage/Get-ControwlyControllerInventory.ps1" \
    "OutputPath=$remote_stage/controller-inventory.json" \
    | tee "$OUTPUT_DIR/controller-inventory-$run_id.log"

set +e
invoke_remote_script "$remote_stage/Invoke-ControwlyControllerQualification.ps1" \
    "InventoryScriptPath=$remote_stage/Get-ControwlyControllerInventory.ps1" \
    "OutputDirectory=$remote_stage/controller-qualification" \
    | tee "$OUTPUT_DIR/controller-qualification-$run_id.log"
qualification_rc=${PIPESTATUS[0]}
set -e
if ((qualification_rc != 0 && qualification_rc != 3)); then
    fail "controller qualification failed with exit code $qualification_rc"
fi

"$VM_HELPER" --no-screenshot --output-dir "$OUTPUT_DIR/vm-final"
printf 'run-controwly-windows-qualification: remote evidence is retained at %s\n' "$remote_stage"
printf 'run-controwly-windows-qualification: host logs/evidence are under %s\n' "$OUTPUT_DIR"
printf '%s\n' 'run-controwly-windows-qualification: no controller injection was attempted; use the direct controller qualification script only with an approved pre-existing device.'
