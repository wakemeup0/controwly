#!/usr/bin/env bash
# Cross-build a Controwly Windows installer on the Arch host.
# This script intentionally does not install packages, run tests, or mutate a VM.
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
TARGET=${CONTROWLY_WINDOWS_TARGET:-x86_64-pc-windows-msvc}
BUNDLE=${CONTROWLY_WINDOWS_BUNDLE:-nsis}
TAURI_CONFIG=${CONTROWLY_TAURI_CONFIG:-}
CHECK_ONLY=0
STATIC_CRT=${CONTROWLY_STATIC_CRT:-1}

usage() {
    cat <<'USAGE'
Usage: build-controwly-windows.sh [--check-only]

Check the pinned local Tauri/cargo-xwin toolchain or build the Windows bundle.
The default bundle is NSIS and the default target is x86_64-pc-windows-msvc.
No package installation, VM operation, or test command is performed.

Environment overrides:
  CONTROWLY_WINDOWS_TARGET   Rust target triple
  CONTROWLY_WINDOWS_BUNDLE   Tauri bundle name (default: nsis)
  CONTROWLY_TAURI_CONFIG     Optional readable Tauri JSON/TOML config path
  CONTROWLY_STATIC_CRT=0     Opt out of +crt-static (default is 1)
USAGE
}

fail() {
    printf 'build-controwly-windows: ERROR: %s\n' "$*" >&2
    exit 2
}

while (($# > 0)); do
    case "$1" in
        --check-only)
            CHECK_ONLY=1
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

[[ $TARGET =~ ^[A-Za-z0-9._-]+$ ]] || fail 'CONTROWLY_WINDOWS_TARGET contains invalid characters'
[[ $BUNDLE =~ ^[A-Za-z0-9._-]+$ ]] || fail 'CONTROWLY_WINDOWS_BUNDLE contains invalid characters'
[[ $STATIC_CRT == 0 || $STATIC_CRT == 1 ]] || fail 'CONTROWLY_STATIC_CRT must be 0 or 1'
if [[ -n "$TAURI_CONFIG" ]]; then
    [[ -f "$TAURI_CONFIG" && -r "$TAURI_CONFIG" ]] || fail "CONTROWLY_TAURI_CONFIG is not a readable regular file: $TAURI_CONFIG"
    TAURI_CONFIG=$(readlink -f -- "$TAURI_CONFIG") || fail "cannot resolve CONTROWLY_TAURI_CONFIG: $TAURI_CONFIG"
fi

for command_name in cargo npm; do
    command -v "$command_name" >/dev/null 2>&1 || fail "required command not found: $command_name"
done
RUST_TOOLCHAIN=''
if [[ -f "$REPO_ROOT/rust-toolchain.toml" ]]; then
    RUST_TOOLCHAIN=$(awk -F '"' '/^[[:space:]]*channel[[:space:]]*=/ { print $2; exit }' "$REPO_ROOT/rust-toolchain.toml")
    [[ -n "$RUST_TOOLCHAIN" ]] || fail "rust-toolchain.toml has no channel"
    command -v rustup >/dev/null 2>&1 || fail "rustup is required to verify pinned toolchain $RUST_TOOLCHAIN"
    installed_targets=$(rustup target list --installed --toolchain "$RUST_TOOLCHAIN") || fail "cannot inspect targets for pinned Rust toolchain $RUST_TOOLCHAIN"
    if ! printf '%s\n' "$installed_targets" | awk -v target="$TARGET" '$0 == target { found = 1 } END { exit(found ? 0 : 1) }'; then
        fail "Rust target $TARGET is missing for pinned toolchain $RUST_TOOLCHAIN; remediation (not automatic): rustup target add --toolchain $RUST_TOOLCHAIN $TARGET"
    fi
    printf 'build-controwly-windows: pinned-toolchain=%s target-installed=%s\n' "$RUST_TOOLCHAIN" "$TARGET"
fi
command -v cargo-xwin >/dev/null 2>&1 || fail 'cargo-xwin is not installed (expected cargo-xwin on PATH)'
[[ -f "$REPO_ROOT/package.json" ]] || fail "package.json not found at $REPO_ROOT"
[[ -f "$REPO_ROOT/src-tauri/tauri.conf.json" ]] || fail "src-tauri/tauri.conf.json not found at $REPO_ROOT"
[[ -x "$REPO_ROOT/node_modules/.bin/tauri" ]] || fail 'local Tauri CLI is missing; install dependencies explicitly before invoking this helper'

printf 'build-controwly-windows: cargo=%s\n' "$(cargo --version)"
printf 'build-controwly-windows: cargo-xwin=%s\n' "$(cargo-xwin --version)"
printf 'build-controwly-windows: target=%s bundle=%s\n' "$TARGET" "$BUNDLE"
printf 'build-controwly-windows: exact environment command: eval "$(cargo xwin env --target %s)"\n' "$TARGET"
tauri_build_args=(build --bundles "$BUNDLE" --target "$TARGET")
if [[ -n "$TAURI_CONFIG" ]]; then
    tauri_build_args+=(--config "$TAURI_CONFIG")
fi
printf 'build-controwly-windows: exact build command: npm run tauri --'
printf ' %q' "${tauri_build_args[@]}"
printf '\n'

if ((CHECK_ONLY == 1)); then
    exit 0
fi

# cargo xwin env emits shell assignments; this is the documented way to make
# the linker/sysroot visible to the Tauri Rust build.
eval "$(cargo xwin env --target "$TARGET")"
if ((STATIC_CRT == 1)); then
    export RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static"
    printf 'build-controwly-windows: RUSTFLAGS includes +crt-static for standalone executables\n'
fi

cd -- "$REPO_ROOT"
exec npm run tauri -- "${tauri_build_args[@]}"
