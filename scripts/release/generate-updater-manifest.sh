#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
if [[ "$#" -lt 4 || "$#" -gt 5 ]]; then
  echo "::error::usage: generate-updater-manifest.sh <version> <tag> <artifact-dir> <repository> [pub-date]" >&2
  exit 2
fi
VERSION="$1"
TAG="$2"
ARTIFACT_DIR="$3"
REPOSITORY="$4"
if [[ "$#" -eq 5 ]]; then
  PUB_DATE="$5"
elif [[ -f "${ARTIFACT_DIR}/latest.json" ]]; then
  # Draft reconciliation may have retained a complete prior manifest. Reuse
  # its publication date so a retry produces byte-identical latest.json.
  PUB_DATE="$(python3 - "${ARTIFACT_DIR}/latest.json" <<'PY'
import json
import re
import sys
from pathlib import Path

path = Path(sys.argv[1])
try:
    data = json.loads(path.read_text(encoding="utf-8"))
except (OSError, json.JSONDecodeError) as exc:
    raise SystemExit(f"::error::retained latest.json is not valid JSON: {exc}") from exc
value = data.get("pub_date")
if not isinstance(value, str) or not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z", value):
    raise SystemExit("::error::retained latest.json has no valid UTC RFC3339 pub_date")
print(value)
PY
  )"
else
  commit_date="$(git show -s --format=%cI "${TAG}^{commit}")"
  PUB_DATE="$(python3 - "$commit_date" <<'PY'
from datetime import datetime, timezone
import sys

value = datetime.fromisoformat(sys.argv[1].replace("Z", "+00:00"))
print(value.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"))
PY
  )"
fi
if [[ ! "$VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "::error::manifest version must be semantic" >&2
  exit 2
fi
if [[ "$TAG" != "v$VERSION" ]]; then
  echo "::error::manifest tag must be v$VERSION; refusing mutable or mismatched update metadata" >&2
  exit 2
fi
if [[ "$REPOSITORY" != "wakemeup0/controwly" ]]; then
  echo "::error::manifest repository must be wakemeup0/controwly" >&2
  exit 2
fi
if [[ ! "$PUB_DATE" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]]; then
  echo "::error::manifest publication date must be UTC RFC3339 (YYYY-MM-DDTHH:MM:SSZ)" >&2
  exit 2
fi

python3 - "$VERSION" "$TAG" "$ARTIFACT_DIR" "$REPOSITORY" "$PUB_DATE" <<'PY'
from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path
from urllib.parse import quote

version, tag, directory, repository, pub_date = sys.argv[1:]
root = Path(directory)
if not root.is_dir():
    raise SystemExit(f"::error::artifact directory does not exist: {root}")
files = [path for path in root.iterdir() if path.is_file() and not path.is_symlink()]
def one(label: str, pattern: str) -> Path:
    found = sorted((path for path in files if re.fullmatch(pattern, path.name, re.I)), key=lambda p: p.name)
    if len(found) != 1:
        raise SystemExit(f"::error::expected one {label}; got {[path.name for path in found]}")
    return found[0]

linux = one("Linux signed Debian updater package", r".+\.deb")
windows = one("Windows signed NSIS installer", r".+(?:-setup|setup)\.exe")
for archive in (linux, windows):
    if version not in archive.name or archive.stat().st_size == 0:
        raise SystemExit(f"::error::invalid updater artifact name or size: {archive.name}")

def signature_for(archive: Path) -> str:
    path = root / f"{archive.name}.sig"
    if not path.is_file() or path.is_symlink() or path.stat().st_size == 0:
        raise SystemExit(f"::error::missing non-empty Tauri signature: {path.name}")
    text = path.read_text(encoding="utf-8", errors="strict").strip()
    if not text or re.search(r"(?:TODO|REPLACE|YOUR[_ -]?KEY|PLACEHOLDER)", text, re.I):
        raise SystemExit(f"::error::invalid or placeholder Tauri signature: {path.name}")
    return text

# Require the first checksum pass to cover the signed updater payloads before
# emitting a manifest. A final pass later also covers latest.json and BUILD-METADATA.json.
checksums = root / "SHA256SUMS"
if not checksums.is_file() or checksums.is_symlink():
    raise SystemExit("::error::generate SHA256SUMS before generating latest.json")
checksum_names = {line.split()[1] for line in checksums.read_text(encoding="utf-8").splitlines() if len(line.split()) == 2}
for archive in (linux, windows):
    if archive.name not in checksum_names or f"{archive.name}.sig" not in checksum_names:
        raise SystemExit(f"::error::SHA256SUMS must cover {archive.name} and its signature before manifest generation")

base = f"https://github.com/{repository}/releases/download/{quote(tag, safe='')}/"
def platform_entry(archive: Path) -> dict[str, str]:
    return {
        "signature": signature_for(archive),
        "url": base + quote(archive.name, safe="-_."),
    }

manifest = {
    "version": version,
    "notes": (
        f"Controwly {version}: signed Tauri updater artifacts for Windows NSIS and Linux DEB. "
        "The Linux DEB installs the root-owned helper required for full controller blocking. "
        "Windows Authenticode signing is optional; Tauri updater signatures are mandatory."
    ),
    "pub_date": pub_date,
    "platforms": {
        "linux-x86_64-deb": platform_entry(linux),
        "windows-x86_64": platform_entry(windows),
    },
}
output = root / "latest.json"
temporary = output.with_name(f".{output.name}.tmp.{os.getpid()}")
temporary.write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
os.replace(temporary, output)
print(f"Generated signed updater manifest {output} for {repository} {tag}.")
PY
