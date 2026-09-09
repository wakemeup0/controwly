#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "::error::usage: sign-updater-artifact.sh <finished-nsis-exe-or-deb>" >&2
  exit 2
fi
PAYLOAD="$1"
case "$PAYLOAD" in
  *.deb|*.exe) ;;
  *) echo "::error::only finished Linux .deb or Windows NSIS .exe payloads may be updater-signed" >&2; exit 2 ;;
esac
if [[ ! -f "$PAYLOAD" || -L "$PAYLOAD" ]]; then
  echo "::error::updater payload is missing or not a regular file: $PAYLOAD" >&2
  exit 1
fi
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" || -z "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" ]]; then
  echo "::error::Tauri updater signing is mandatory. Configure TAURI_SIGNING_PRIVATE_KEY and TAURI_SIGNING_PRIVATE_KEY_PASSWORD repository secrets; refusing an unsigned updater payload." >&2
  exit 2
fi
SIGNATURE="${PAYLOAD}.sig"
if [[ -s "$SIGNATURE" ]]; then
  echo "Reusing existing Tauri signature ${SIGNATURE}; final cryptographic verification remains mandatory."
  exit 0
fi
if [[ -e "$SIGNATURE" ]]; then
  echo "::error::existing updater signature is empty or invalid: $SIGNATURE" >&2
  exit 1
fi

# Build jobs run without these variables. This step is the only place that
# invokes the official Tauri signer with the protected private key.
npm run tauri -- signer sign "$PAYLOAD"
if [[ ! -s "$SIGNATURE" ]]; then
  echo "::error::Tauri signer produced no non-empty signature for $PAYLOAD" >&2
  exit 1
fi
python3 - "$SIGNATURE" <<'PY'
import re
import sys
from pathlib import Path
text = Path(sys.argv[1]).read_text(encoding='utf-8', errors='strict').strip()
if not text or re.search(r'(?:TODO|REPLACE|YOUR[_ -]?KEY|PLACEHOLDER)', text, re.I):
    raise SystemExit('::error::Tauri signer produced a placeholder updater signature')
PY
printf 'Signed finished updater payload %s.\n' "$(basename "$PAYLOAD")"
