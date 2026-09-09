#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "::error::usage: sign-deb.sh <debian-package>" >&2
  exit 2
fi
DEB="$1"
if [[ ! -f "$DEB" || -L "$DEB" ]]; then
  echo "::error::Debian package is missing or not a regular file: $DEB" >&2
  exit 1
fi
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" || -z "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" ]]; then
  echo "::error::Tauri updater signing is mandatory. Configure TAURI_SIGNING_PRIVATE_KEY and TAURI_SIGNING_PRIVATE_KEY_PASSWORD repository secrets; refusing an unsigned Linux updater." >&2
  exit 2
fi
SIGNATURE="${DEB}.sig"
if [[ -s "$SIGNATURE" ]]; then
  echo "Reusing the Tauri-generated DEB signature ${SIGNATURE}; final minisign verification runs before publication."
  exit 0
fi
if [[ -e "$SIGNATURE" ]]; then
  echo "::error::existing DEB signature is empty or invalid: $SIGNATURE" >&2
  exit 1
fi

# The exact CLI is supplied by the checked-in @tauri-apps/cli dependency. Tauri
# signer writes a minisign-compatible adjacent .sig consumed by updater.rs.
npm run tauri -- signer sign "$DEB"
if [[ ! -s "$SIGNATURE" ]]; then
  echo "::error::Tauri signer produced no non-empty DEB signature" >&2
  exit 1
fi
python3 - "$SIGNATURE" <<'PY'
import re
import sys
text = open(sys.argv[1], encoding="utf-8").read().strip()
if not text or re.search(r"(?:TODO|REPLACE|YOUR[_ -]?KEY|PLACEHOLDER)", text, re.I):
    raise SystemExit("::error::Tauri signer produced a placeholder DEB signature")
PY
printf 'Signed Linux updater payload %s.\n' "$(basename "$DEB")"
