#!/usr/bin/env bash
# Read-only/read-mostly readiness helper for the existing Hooviestar Windows VM.
# Deliberately does not use --force-boot, managedsave-remove, destroy, or any USB
# attach/detach operation. A failed plain start is reported for human approval.
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
LIBVIRT_URI=${CONTROWLY_LIBVIRT_URI:-qemu:///session}
VM_NAME=${CONTROWLY_VM_NAME:-hooviestar-win11}
SSH_ALIAS=${CONTROWLY_SSH_ALIAS:-hooviestar-win11}
OUTPUT_DIR=${CONTROWLY_VM_OUTPUT:-$REPO_ROOT/target/controwly-qualification/windows-vm}
START_IF_STOPPED=0
WAIT_SECONDS=${CONTROWLY_VM_WAIT_SECONDS:-90}
CAPTURE_SCREENSHOT=1

usage() {
    cat <<'USAGE'
Usage: controwly-windows-vm.sh [options]

Inspect the existing user-session Hooviestar Windows VM, optionally start it with
plain `virsh start`, wait for the configured SSH alias, and capture a SPICE
screenshot. No credentials are read or printed.

Options:
  --start                   Start a shut-off VM with plain virsh start.
  --no-screenshot           Skip the SPICE screenshot.
  --wait-seconds N          SSH wait limit (default: 90).
  --output-dir PATH         Evidence directory (default: target/controwly-qualification/windows-vm).
  --help                    Show this help.

Environment overrides: CONTROWLY_LIBVIRT_URI, CONTROWLY_VM_NAME,
CONTROWLY_SSH_ALIAS, CONTROWLY_VM_OUTPUT, CONTROWLY_VM_WAIT_SECONDS.
USAGE
}

fail() {
    printf 'controwly-windows-vm: ERROR: %s\n' "$*" >&2
    exit 2
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

while (($# > 0)); do
    case "$1" in
        --start)
            START_IF_STOPPED=1
            shift
            ;;
        --no-screenshot)
            CAPTURE_SCREENSHOT=0
            shift
            ;;
        --wait-seconds)
            (($# >= 2)) || fail "--wait-seconds requires a value"
            WAIT_SECONDS=$2
            shift 2
            ;;
        --output-dir)
            (($# >= 2)) || fail "--output-dir requires a value"
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

[[ $VM_NAME =~ ^[A-Za-z0-9._-]+$ ]] || fail 'CONTROWLY_VM_NAME must contain only letters, digits, dot, underscore, or hyphen'
[[ $SSH_ALIAS =~ ^[A-Za-z0-9._-]+$ ]] || fail 'CONTROWLY_SSH_ALIAS must contain only letters, digits, dot, underscore, or hyphen'
[[ $WAIT_SECONDS =~ ^[0-9]+$ ]] || fail '--wait-seconds must be a non-negative integer'

require_command virsh
require_command ssh
mkdir -p -- "$OUTPUT_DIR"

if ! virsh -c "$LIBVIRT_URI" dominfo "$VM_NAME" >/dev/null 2>&1; then
    fail "libvirt domain not found on $LIBVIRT_URI: $VM_NAME"
fi

get_state() {
    virsh -c "$LIBVIRT_URI" domstate "$VM_NAME" --reason | tr -d '\r' | awk 'NR == 1 { print $1; exit }'
}

state=$(get_state)
if [[ "$state" != running && "$state" != paused ]]; then
    if ((START_IF_STOPPED == 0)); then
        fail "domain state is '$state'; rerun with --start only after confirming this VM may be started (plain command: virsh -c $LIBVIRT_URI start $VM_NAME)"
    fi
    printf 'controwly-windows-vm: starting %s with plain virsh start (managed-save is not discarded)\n' "$VM_NAME"
    if ! virsh -c "$LIBVIRT_URI" start "$VM_NAME"; then
        save_path="$HOME/.config/libvirt/qemu/save/$VM_NAME.save"
        fail "plain VM start failed; no recovery/destructive action was attempted. Inspect $save_path and obtain Main approval before any managed-save operation"
    fi
    state=$(get_state)
fi

if [[ "$state" != running && "$state" != paused ]]; then
    fail "domain did not reach running/paused state: $state"
fi

printf 'controwly-windows-vm: domain=%s state=%s uri=%s\n' "$VM_NAME" "$state" "$LIBVIRT_URI"

ssh_ready=0
for ((attempt = 0; attempt <= WAIT_SECONDS; attempt++)); do
    if ssh -o BatchMode=yes -o ConnectTimeout=2 -o ConnectionAttempts=1 "$SSH_ALIAS" \
        'powershell -NoProfile -NonInteractive -Command "$PSVersionTable.PSVersion.ToString()"' \
        >/dev/null 2>&1; then
        ssh_ready=1
        break
    fi
    if ((attempt < WAIT_SECONDS)); then
        sleep 1
    fi
done

if ((ssh_ready == 0)); then
    fail "SSH alias '$SSH_ALIAS' did not become ready within ${WAIT_SECONDS}s; no credentials were requested"
fi

printf 'controwly-windows-vm: SSH ready via %s (BatchMode/key authentication)\n' "$SSH_ALIAS"

screenshot_path=''
if ((CAPTURE_SCREENSHOT == 1)); then
    timestamp=$(date -u +%Y%m%dT%H%M%SZ)
    screenshot_path="$OUTPUT_DIR/windows-vm-$timestamp.png"
    virsh -c "$LIBVIRT_URI" screenshot "$VM_NAME" "$screenshot_path" >/dev/null
    printf 'controwly-windows-vm: SPICE screenshot=%s\n' "$screenshot_path"
fi

status_path="$OUTPUT_DIR/vm-status.txt"
{
    printf 'domain=%s\n' "$VM_NAME"
    printf 'libvirt_uri=%s\n' "$LIBVIRT_URI"
    printf 'state=%s\n' "$state"
    printf 'ssh_alias=%s\n' "$SSH_ALIAS"
    printf 'ssh_ready=true\n'
    if [[ -n "$screenshot_path" ]]; then
        printf 'screenshot=%s\n' "$screenshot_path"
    else
        printf 'screenshot=skipped\n'
    fi
    printf 'unsafe_actions=not-run (no force-boot, managedsave-remove, destroy, reset, or USB attach)\n'
} >"$status_path"
printf 'controwly-windows-vm: status=%s\n' "$status_path"
printf 'controwly-windows-vm: exact SSH probe: ssh -o BatchMode=yes %s "powershell -NoProfile -NonInteractive -Command \\\"Get-PnpDevice -PresentOnly | ConvertTo-Json -Depth 5\\\""\n' "$SSH_ALIAS"
