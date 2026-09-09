#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "::error::usage: verify-tauri-signatures.sh <artifact-dir>" >&2
  exit 2
fi
ARTIFACT_DIR="$1"
if [[ ! -d "$ARTIFACT_DIR" ]]; then
  echo "::error::artifact directory does not exist: $ARTIFACT_DIR" >&2
  exit 1
fi
if ! command -v rsign >/dev/null 2>&1; then
  echo "::error::rsign2 0.6.6 (the rsign verifier) is required to verify Tauri updater signatures" >&2
  exit 1
fi

PUBLIC_KEY_FILE="$(mktemp "${RUNNER_TEMP:-/tmp}/controwly-pubkey.XXXXXXXX")"
SIGNATURE_TMP_DIR="$(mktemp -d "${RUNNER_TEMP:-/tmp}/controwly-signatures.XXXXXXXX")"
trap 'rm -rf "$PUBLIC_KEY_FILE" "$SIGNATURE_TMP_DIR"' EXIT

python3 - "$PUBLIC_KEY_FILE" <<'PY'
import base64
import binascii
import json
import re
import sys
from pathlib import Path
config = json.loads(Path('src-tauri/tauri.conf.json').read_text(encoding='utf-8'))
updater = config.get('plugins', {}).get('updater', {})
key = updater.get('pubkey') if isinstance(updater, dict) else None
if not isinstance(key, str) or not key.strip() or re.search(r'(?:TODO|REPLACE|YOUR[_ -]?KEY|PLACEHOLDER)', key, re.I):
    raise SystemExit('::error::Tauri updater public key is missing or a placeholder')
encoded = key.strip()
decoded = encoded
try:
    candidate = base64.b64decode(encoded, validate=True).decode('utf-8').strip()
except (binascii.Error, UnicodeDecodeError, ValueError):
    candidate = ''
if candidate.startswith('untrusted comment:') and '\n' in candidate:
    decoded = candidate
if not decoded.startswith('untrusted comment:'):
    if not re.search(r'(?m)^RW[A-Za-z0-9+/=]+$', decoded):
        raise SystemExit('::error::Tauri updater pubkey is not a minisign key or base64-encoded minisign key')
    decoded = 'untrusted comment: minisign public key\n' + decoded
Path(sys.argv[1]).write_text(decoded.rstrip() + '\n', encoding='utf-8')
PY

if [[ ! -s "$PUBLIC_KEY_FILE" ]]; then
  echo "::error::Tauri updater public key file is empty" >&2
  exit 1
fi

mapfile -t signatures < <(find "$ARTIFACT_DIR" -maxdepth 1 -type f -name '*.sig' -print | LC_ALL=C sort)
if [[ "${#signatures[@]}" -ne 2 ]]; then
  echo "::error::expected exactly two signed updater payloads (Linux DEB and Windows NSIS installer), found ${#signatures[@]}" >&2
  exit 1
fi
deb_signatures=0
windows_signatures=0
for signature in "${signatures[@]}"; do
  signature_name="$(basename "$signature")"
  case "$signature_name" in
    *.deb.sig) deb_signatures=$((deb_signatures + 1)) ;;
    *-setup.exe.sig|*setup.exe.sig) windows_signatures=$((windows_signatures + 1)) ;;
    *) echo "::error::unexpected updater signature: $signature_name" >&2; exit 1 ;;
  esac
  payload="${signature%.sig}"
  if [[ ! -f "$payload" || -L "$payload" ]]; then
    echo "::error::signature has no adjacent payload: $signature_name" >&2
    exit 1
  fi
  raw_signature="${SIGNATURE_TMP_DIR}/${signature_name}.minisig"
  python3 - "$signature" "$raw_signature" <<'PY'
import base64
import binascii
import re
import sys
from pathlib import Path

source = Path(sys.argv[1])
destination = Path(sys.argv[2])
try:
    encoded = "".join(source.read_text(encoding="ascii", errors="strict").split())
    raw = base64.b64decode(encoded, validate=True)
except (OSError, UnicodeError, binascii.Error, ValueError) as exc:
    raise SystemExit(f"::error::Tauri signature {source.name} is not valid outer base64: {exc}") from exc
try:
    text = raw.decode("utf-8").strip()
except UnicodeDecodeError as exc:
    raise SystemExit(f"::error::Tauri signature {source.name} is not UTF-8 minisign text: {exc}") from exc
lines = text.splitlines()
if (
    len(lines) != 4
    or not lines[0].startswith("untrusted comment:")
    or not re.fullmatch(r"[A-Za-z0-9+/=]+", lines[1])
    or not lines[2].startswith("trusted comment:")
    or not re.fullmatch(r"[A-Za-z0-9+/=]+", lines[3])
):
    raise SystemExit(f"::error::Tauri signature {source.name} has an invalid decoded minisign layout")
destination.write_bytes(raw.rstrip() + b"\n")
PY
  rsign verify "$payload" -p "$PUBLIC_KEY_FILE" -x "$raw_signature" -q
done
if [[ "$deb_signatures" -ne 1 || "$windows_signatures" -ne 1 ]]; then
  echo "::error::signature set must contain exactly one DEB and one NSIS installer signature" >&2
  exit 1
fi
