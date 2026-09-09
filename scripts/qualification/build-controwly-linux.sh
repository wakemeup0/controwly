#!/usr/bin/env bash
# Build the supported Controwly Linux DEB only. No AppImage target is exposed.
# This helper does not install packages, run tests, or touch controller devices.
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
TARGET=${CONTROWLY_LINUX_TARGET:-x86_64-unknown-linux-gnu}
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$REPO_ROOT/target}
HELPER_SOURCE="$REPO_ROOT/target/release/controwly-linux-input-helper"
HELPER_BINARY="$CARGO_TARGET_DIR/$TARGET/release/controwly-linux-input-helper"
TAURI_CONFIG=${CONTROWLY_TAURI_CONFIG:-}
CHECK_ONLY=0

usage() {
    cat <<'USAGE'
Usage: build-controwly-linux.sh [--check-only]

Check the local Tauri toolchain or build the supported Linux DEB bundle. The
portable AppImage target is intentionally not built or published.

Environment overrides:
  CONTROWLY_LINUX_TARGET   Rust target (default: x86_64-unknown-linux-gnu)
  CONTROWLY_TAURI_CONFIG   Optional readable Tauri JSON/TOML config path
USAGE
}

fail() {
    printf 'build-controwly-linux: ERROR: %s\n' "$*" >&2
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
[[ $TARGET =~ ^[A-Za-z0-9._-]+$ ]] || fail 'CONTROWLY_LINUX_TARGET contains invalid characters'
if [[ -n "$TAURI_CONFIG" ]]; then
    [[ -f "$TAURI_CONFIG" && -r "$TAURI_CONFIG" ]] || fail "CONTROWLY_TAURI_CONFIG is not a readable regular file: $TAURI_CONFIG"
    TAURI_CONFIG=$(readlink -f -- "$TAURI_CONFIG") || fail "cannot resolve CONTROWLY_TAURI_CONFIG: $TAURI_CONFIG"
fi
for command_name in npm node cargo install; do
    command -v "$command_name" >/dev/null 2>&1 || fail "required command not found: $command_name"
done
[[ -f "$REPO_ROOT/package.json" ]] || fail "package.json not found at $REPO_ROOT"
[[ -f "$REPO_ROOT/src-tauri/tauri.conf.json" ]] || fail "src-tauri/tauri.conf.json not found at $REPO_ROOT"
[[ -x "$REPO_ROOT/node_modules/.bin/tauri" ]] || fail 'local Tauri CLI is missing; install dependencies explicitly before invoking this helper'
printf 'build-controwly-linux: node=%s npm=%s\n' "$(node --version)" "$(npm --version)"
printf 'build-controwly-linux: target=%s bundle=deb\n' "$TARGET"
printf 'build-controwly-linux: CARGO_TARGET_DIR=%s\n' "$CARGO_TARGET_DIR"
printf 'build-controwly-linux: helper command: CARGO_TARGET_DIR=%s cargo build --manifest-path src-tauri/Cargo.toml --release --features linux-helper --bin controwly-linux-input-helper --target %s\n' "$CARGO_TARGET_DIR" "$TARGET"
printf 'build-controwly-linux: helper resource source: %s\n' "$HELPER_SOURCE"
tauri_build_args=(build --bundles deb --target "$TARGET")
if [[ -n "$TAURI_CONFIG" ]]; then
    tauri_build_args+=(--config "$TAURI_CONFIG")
fi
printf 'build-controwly-linux: exact bundle command: CARGO_TARGET_DIR=%q npm run tauri --' "$CARGO_TARGET_DIR"
printf ' %q' "${tauri_build_args[@]}"
printf '\n'

if ((CHECK_ONLY == 1)); then
    exit 0
fi

cd -- "$REPO_ROOT"
export CARGO_TARGET_DIR
cargo build --manifest-path src-tauri/Cargo.toml --release --features linux-helper --bin controwly-linux-input-helper --target "$TARGET"
install -D -m 0755 "$HELPER_BINARY" "$HELPER_SOURCE"
trap 'rm -f -- "$HELPER_SOURCE"' EXIT
npm run tauri -- "${tauri_build_args[@]}"
