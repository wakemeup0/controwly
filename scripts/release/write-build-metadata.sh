#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 5 ]]; then
  echo "::error::usage: write-build-metadata.sh <version> <tag> <full-commit-sha> <artifact-dir> <owner/repository>" >&2
  exit 2
fi
VERSION="$1"
TAG="$2"
COMMIT="$3"
ARTIFACT_DIR="$4"
REPOSITORY="$5"
if [[ ! "$VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "::error::build metadata requires a semantic version" >&2
  exit 2
fi
if [[ "$TAG" != "v$VERSION" ]]; then
  echo "::error::build metadata tag must be v$VERSION" >&2
  exit 2
fi
if [[ ! "$COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "::error::build metadata requires a lowercase full 40-character commit SHA" >&2
  exit 2
fi
if [[ "$REPOSITORY" != "wakemeup0/controwly" ]]; then
  echo "::error::build metadata repository must be wakemeup0/controwly" >&2
  exit 2
fi
if [[ ! -d "$ARTIFACT_DIR" ]]; then
  echo "::error::artifact directory does not exist: $ARTIFACT_DIR" >&2
  exit 1
fi

python3 - "$VERSION" "$TAG" "$COMMIT" "$ARTIFACT_DIR" "$REPOSITORY" <<'PY'
from __future__ import annotations

import hashlib
import json
import os
import re
import sys
from pathlib import Path

version, tag, commit, directory, repository = sys.argv[1:]
root = Path(directory)
metadata_name = "BUILD-METADATA.json"
checksums_name = "SHA256SUMS"
files = sorted(root.iterdir(), key=lambda path: path.name)
for path in files:
    if path.is_symlink() or not path.is_file():
        raise SystemExit(f"::error::artifact directory contains a non-regular file: {path.name}")
    if path.stat().st_size == 0:
        raise SystemExit(f"::error::artifact is empty: {path.name}")

payload_files = [path for path in files if re.fullmatch(r".+\.deb", path.name, re.I)]
installer_files = [
    path for path in files
    if re.fullmatch(r".+(?:-setup|setup)\.exe", path.name, re.I)
]
if len(payload_files) != 1 or len(installer_files) != 1:
    raise SystemExit(
        "::error::build metadata requires exactly one DEB and one NSIS installer; "
        f"found {[path.name for path in payload_files]} and {[path.name for path in installer_files]}"
    )

payloads = payload_files + installer_files
artifact_paths = list(payloads)
for payload in payloads:
    if version not in payload.name:
        raise SystemExit(f"::error::artifact name does not contain release version: {payload.name}")
    signature = root / f"{payload.name}.sig"
    if not signature.is_file() or signature.is_symlink() or signature.stat().st_size == 0:
        raise SystemExit(f"::error::signed payload has no adjacent non-empty signature: {signature.name}")
    artifact_paths.append(signature)

manifest = root / "latest.json"
if not manifest.is_file() or manifest.is_symlink() or manifest.stat().st_size == 0:
    raise SystemExit("::error::build metadata requires a non-empty latest.json")
artifact_paths.append(manifest)
expected_names = {path.name for path in artifact_paths}
actual_names = {
    path.name for path in files if path.name not in {metadata_name, checksums_name}
}
if actual_names != expected_names:
    raise SystemExit(
        "::error::build metadata artifact set must contain only payloads, signatures, and latest.json; "
        f"expected {sorted(expected_names)}, got {sorted(actual_names)}"
    )

record = {
    "schema": 1,
    "repository": f"https://github.com/{repository}",
    "commit": commit,
    "tag": tag,
    "version": version,
    "artifacts": {
        path.name: hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(artifact_paths, key=lambda item: item.name)
    },
}
serialized = json.dumps(record, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
output = root / metadata_name
temporary = root / f".{metadata_name}.tmp.{os.getpid()}"
temporary.write_text(serialized, encoding="utf-8")
os.replace(temporary, output)
print(f"Wrote deterministic {metadata_name} for {repository} {tag} at {commit} covering {len(artifact_paths)} artifacts.")
PY
