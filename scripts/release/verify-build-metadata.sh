#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 5 ]]; then
  echo "::error::usage: verify-build-metadata.sh <version> <tag> <full-commit-sha> <artifact-dir> <owner/repository>" >&2
  exit 2
fi
VERSION="$1"
TAG="$2"
COMMIT="$3"
ARTIFACT_DIR="$4"
REPOSITORY="$5"
if [[ ! "$VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "::error::build metadata verification requires a semantic version" >&2
  exit 2
fi
if [[ "$TAG" != "v$VERSION" ]]; then
  echo "::error::build metadata tag must be v$VERSION" >&2
  exit 2
fi
if [[ ! "$COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "::error::build metadata verification requires a lowercase full 40-character commit SHA" >&2
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
import re
import sys
from pathlib import Path

version, tag, commit, directory, repository = sys.argv[1:]
root = Path(directory)
metadata_path = root / "BUILD-METADATA.json"
checksums_path = root / "SHA256SUMS"
if not metadata_path.is_file() or metadata_path.is_symlink() or metadata_path.stat().st_size == 0:
    raise SystemExit("::error::BUILD-METADATA.json is missing, empty, or not a regular file")
if not checksums_path.is_file() or checksums_path.is_symlink() or checksums_path.stat().st_size == 0:
    raise SystemExit("::error::SHA256SUMS is missing, empty, or not a regular file")

try:
    raw_metadata = metadata_path.read_bytes()
    metadata = json.loads(raw_metadata.decode("utf-8"))
except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
    raise SystemExit(f"::error::BUILD-METADATA.json is not valid UTF-8 JSON: {exc}") from exc

expected_keys = {"schema", "repository", "commit", "tag", "version", "artifacts"}
if not isinstance(metadata, dict) or set(metadata) != expected_keys:
    raise SystemExit(f"::error::BUILD-METADATA.json keys must be exactly {sorted(expected_keys)}")
if type(metadata.get("schema")) is not int or metadata.get("schema") != 1:
    raise SystemExit("::error::BUILD-METADATA.json schema must be integer 1")
if metadata.get("repository") != f"https://github.com/{repository}":
    raise SystemExit("::error::BUILD-METADATA.json repository is not the canonical HTTPS repository URL")
if metadata.get("commit") != commit:
    raise SystemExit(
        f"::error::BUILD-METADATA.json commit {metadata.get('commit')!r} does not match prepared commit {commit}"
    )
if metadata.get("tag") != tag or metadata.get("version") != version:
    raise SystemExit("::error::BUILD-METADATA.json tag/version do not match the release inputs")

artifacts = metadata.get("artifacts")
if not isinstance(artifacts, dict) or not artifacts:
    raise SystemExit("::error::BUILD-METADATA.json artifacts must be a non-empty object")
if any(
    not isinstance(name, str)
    or not name
    or "/" in name
    or "\\" in name
    or any(char.isspace() for char in name)
    or not isinstance(digest, str)
    or not re.fullmatch(r"[0-9a-f]{64}", digest)
    for name, digest in artifacts.items()
):
    raise SystemExit("::error::BUILD-METADATA.json artifacts must map safe names to lowercase SHA-256 digests")

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
    raise SystemExit("::error::BUILD-METADATA.json verification requires exactly one DEB and one NSIS installer")
payloads = payload_files + installer_files
expected_paths = list(payloads)
for payload in payloads:
    if version not in payload.name:
        raise SystemExit(f"::error::artifact name does not contain release version: {payload.name}")
    signature = root / f"{payload.name}.sig"
    if not signature.is_file() or signature.is_symlink() or signature.stat().st_size == 0:
        raise SystemExit(f"::error::signed payload has no adjacent non-empty signature: {signature.name}")
    expected_paths.append(signature)
manifest = root / "latest.json"
if not manifest.is_file() or manifest.is_symlink() or manifest.stat().st_size == 0:
    raise SystemExit("::error::BUILD-METADATA.json verification requires a non-empty latest.json")
expected_paths.append(manifest)
expected_names = {path.name for path in expected_paths}
actual_names = {
    path.name for path in files if path.name not in {metadata_path.name, checksums_path.name}
}
if actual_names != expected_names or set(artifacts) != expected_names:
    raise SystemExit(
        "::error::BUILD-METADATA.json artifacts must cover exactly the two payloads, signatures, and latest.json; "
        f"expected {sorted(expected_names)}, files {sorted(actual_names)}, record {sorted(artifacts)}"
    )

for path in expected_paths:
    actual_digest = hashlib.sha256(path.read_bytes()).hexdigest()
    if actual_digest != artifacts[path.name]:
        raise SystemExit(f"::error::BUILD-METADATA.json digest mismatch for {path.name}")

canonical = json.dumps(metadata, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
if raw_metadata != canonical.encode("utf-8"):
    raise SystemExit("::error::BUILD-METADATA.json is not in the required deterministic serialization")

checksum_entries: dict[str, str] = {}
for line in checksums_path.read_text(encoding="utf-8", errors="strict").splitlines():
    parts = line.split()
    if len(parts) != 2 or not re.fullmatch(r"[0-9a-fA-F]{64}", parts[0]) or not parts[1]:
        raise SystemExit(f"::error::invalid SHA256SUMS line: {line!r}")
    if parts[1] in checksum_entries:
        raise SystemExit(f"::error::duplicate SHA256SUMS entry for {parts[1]}")
    checksum_entries[parts[1]] = parts[0].lower()
all_non_checksum = {path.name for path in files if path.name != checksums_path.name}
if set(checksum_entries) != all_non_checksum:
    raise SystemExit(
        f"::error::SHA256SUMS must cover every release asset including BUILD-METADATA.json; "
        f"expected {sorted(all_non_checksum)}, got {sorted(checksum_entries)}"
    )
for name, expected_digest in checksum_entries.items():
    actual_digest = hashlib.sha256((root / name).read_bytes()).hexdigest()
    if actual_digest != expected_digest:
        raise SystemExit(f"::error::SHA256SUMS digest mismatch for {name}")

print(f"Verified deterministic BUILD-METADATA.json for {repository} {tag} at {commit}.")
PY
