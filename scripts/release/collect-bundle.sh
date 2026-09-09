#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 3 ]]; then
  echo "::error::usage: collect-bundle.sh <linux|windows> <bundle-root> <destination>" >&2
  exit 2
fi
PLATFORM="$1"
SOURCE="$2"
DESTINATION="$3"
case "$PLATFORM" in
  linux|windows) ;;
  *) echo "::error::bundle platform must be linux or windows" >&2; exit 2 ;;
esac

python3 - "$PLATFORM" "$SOURCE" "$DESTINATION" <<'PY'
from __future__ import annotations

import re
import shutil
import sys
from pathlib import Path

platform, source_name, destination_name = sys.argv[1:]
source = Path(source_name)
destination = Path(destination_name)
if not source.is_dir():
    raise SystemExit(f"::error::Tauri bundle root does not exist: {source}")
destination.mkdir(parents=True, exist_ok=True)

all_files = sorted((path for path in source.rglob("*") if path.is_file() and not path.is_symlink()), key=lambda p: str(p))
def one(label: str, pattern: str) -> Path:
    found = [path for path in all_files if re.fullmatch(pattern, path.name, re.I)]
    if len(found) != 1:
        raise SystemExit(f"::error::expected exactly one {label}; found {[str(path) for path in found]}")
    return found[0]

if platform == "linux":
    artifacts = [
        one("Debian package", r".+\.deb"),
    ]
else:
    artifacts = [
        one("NSIS installer", r".+(?:-setup|setup)\.exe"),
    ]

for artifact in artifacts:
    if artifact.stat().st_size == 0:
        raise SystemExit(f"::error::Tauri artifact is empty: {artifact}")
    destination_path = destination / artifact.name
    if destination_path.exists():
        raise SystemExit(f"::error::refusing to overwrite staged artifact: {destination_path.name}")
    shutil.copy2(artifact, destination_path)
    if artifact.name.endswith((".deb", ".exe")):
        signature = artifact.with_name(artifact.name + ".sig")
        if not signature.is_file() or signature.is_symlink() or signature.stat().st_size == 0:
            raise SystemExit(f"::error::signed updater artifact has no adjacent .sig: {signature}")
        shutil.copy2(signature, destination / signature.name)

print(f"Staged {platform} Tauri bundle into {destination}.")
PY
