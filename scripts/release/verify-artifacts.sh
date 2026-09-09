#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
VERSION="${1:-}"
ARTIFACT_DIR="${2:-dist}"
MODE="${3:-all}"
EXPECTED_TAG="${4:-}"
EXPECTED_COMMIT="${5:-}"
if [[ ! "$VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "::error::usage: verify-artifacts.sh <semantic-version> <artifact-dir> <linux|windows|all> [tag] [full-commit-sha]" >&2
  exit 2
fi
case "$MODE" in
  linux|windows|all) ;;
  *) echo "::error::artifact mode must be linux, windows, or all" >&2; exit 2 ;;
esac
if [[ -n "$EXPECTED_TAG" || -n "$EXPECTED_COMMIT" ]] && [[ -z "$EXPECTED_TAG" || -z "$EXPECTED_COMMIT" ]]; then
  echo "::error::tag and full commit must be supplied together" >&2
  exit 2
fi
if [[ -n "$EXPECTED_COMMIT" && ! "$EXPECTED_COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "::error::expected full commit must be a lowercase 40-character SHA" >&2
  exit 2
fi
if [[ -n "$EXPECTED_TAG" && "$EXPECTED_TAG" != "v$VERSION" ]]; then
  echo "::error::expected tag must be v$VERSION" >&2
  exit 2
fi
if [[ ! -d "$ARTIFACT_DIR" ]]; then
  echo "::error::artifact directory does not exist: $ARTIFACT_DIR" >&2
  exit 1
fi

python3 - "$VERSION" "$ARTIFACT_DIR" "$MODE" "$EXPECTED_TAG" "$EXPECTED_COMMIT" <<'PY'
from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path
from urllib.parse import quote, urlparse

version, directory, mode, expected_tag, expected_commit = sys.argv[1:]
root = Path(directory)
if not root.is_dir():
    raise SystemExit(f"::error::artifact directory does not exist: {root}")

files = sorted(root.iterdir(), key=lambda item: item.name)
for path in files:
    if path.is_symlink() or not path.is_file():
        raise SystemExit(f"::error::artifact directory contains a non-regular file: {path.name}")
    if path.stat().st_size == 0:
        raise SystemExit(f"::error::artifact is empty: {path.name}")
    if path.stat().st_size > 1024 * 1024 * 1024:
        raise SystemExit(f"::error::artifact exceeds 1 GiB safety limit: {path.name}")

names = {path.name for path in files}
def matching(pattern: str) -> list[Path]:
    return [path for path in files if re.fullmatch(pattern, path.name, re.I)]

def require_one(label: str, pattern: str) -> Path:
    found = matching(pattern)
    if len(found) != 1:
        raise SystemExit(f"::error::expected exactly one {label} matching {pattern!r}; found {[p.name for p in found]}")
    return found[0]

def check_version_name(path: Path, label: str) -> None:
    if version not in path.name:
        raise SystemExit(f"::error::{label} {path.name} does not contain release version {version}")

def check_signature(archive: Path) -> Path:
    signature = root / f"{archive.name}.sig"
    if not signature.is_file() or signature.is_symlink() or signature.stat().st_size == 0:
        raise SystemExit(f"::error::signed updater payload {archive.name} is missing its non-empty .sig file")
    text = signature.read_text(encoding="utf-8", errors="strict").strip()
    if not text or re.search(r"(?:TODO|REPLACE|YOUR[_ -]?KEY|PLACEHOLDER)", text, re.I):
        raise SystemExit(f"::error::updater signature {signature.name} is missing or appears to be a placeholder")
    return signature

expected: set[str] = set()
linux_payload: Path | None = None
windows_payload: Path | None = None
if mode in ("linux", "all"):
    deb = require_one("Linux Debian package", r".+\.deb")
    check_version_name(deb, "Debian package")
    linux_payload = deb
    expected.update((deb.name, check_signature(deb).name))
if mode in ("windows", "all"):
    installer = require_one("Windows NSIS installer", r".+(?:-setup|setup)\.exe")
    check_version_name(installer, "NSIS installer")
    windows_payload = installer
    expected.update((installer.name, check_signature(installer).name))

expected_files = expected
if mode == "linux":
    expected_files = expected
elif mode == "windows":
    expected_files = expected
else:
    manifest_path = root / "latest.json"
    metadata_path = root / "BUILD-METADATA.json"
    checksums_path = root / "SHA256SUMS"
    if not manifest_path.is_file() or not metadata_path.is_file() or not checksums_path.is_file():
        raise SystemExit("::error::all-platform release requires latest.json, BUILD-METADATA.json, and SHA256SUMS")
    expected.update((manifest_path.name, metadata_path.name, checksums_path.name))
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise SystemExit(f"::error::latest.json is not valid UTF-8 JSON: {exc}") from exc
    if manifest.get("version") != version:
        raise SystemExit(f"::error::latest.json version must be {version}; got {manifest.get('version')!r}")
    platforms = manifest.get("platforms")
    if not isinstance(platforms, dict) or set(platforms) != {"linux-x86_64-deb", "windows-x86_64"}:
        raise SystemExit("::error::latest.json platforms must be exactly linux-x86_64-deb and windows-x86_64")
    expected_repo = "https://github.com/wakemeup0/controwly/releases/download/" + quote("v" + version, safe="") + "/"
    for platform, payload in (("linux-x86_64-deb", linux_payload), ("windows-x86_64", windows_payload)):
        if payload is None:
            raise SystemExit(f"::error::missing payload for {platform}")
        entry = platforms[platform]
        if not isinstance(entry, dict):
            raise SystemExit(f"::error::latest.json platform entry {platform} is not an object")
        if set(entry) != {"signature", "url"}:
            raise SystemExit(f"::error::latest.json platform entry {platform} must contain only signature and url")
        if entry.get("signature") != (root / f"{payload.name}.sig").read_text(encoding="utf-8").strip():
            raise SystemExit(f"::error::latest.json signature does not match {payload.name}.sig")
        url = entry.get("url")
        expected_url = expected_repo + quote(payload.name, safe="-_.")
        if not isinstance(url, str) or url != expected_url:
            raise SystemExit(f"::error::latest.json URL for {platform} must target the exact immutable v{version} release payload")
        parsed = urlparse(url)
        if parsed.scheme != "https" or parsed.query or parsed.fragment:
            raise SystemExit(f"::error::latest.json URL for {platform} must be a query-free HTTPS URL")

    try:
        raw_metadata = metadata_path.read_bytes()
        metadata = json.loads(raw_metadata.decode("utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise SystemExit(f"::error::BUILD-METADATA.json is not valid UTF-8 JSON: {exc}") from exc
    metadata_keys = {"schema", "repository", "commit", "tag", "version", "artifacts"}
    if not isinstance(metadata, dict) or set(metadata) != metadata_keys:
        raise SystemExit(f"::error::BUILD-METADATA.json keys must be exactly {sorted(metadata_keys)}")
    if type(metadata.get("schema")) is not int or metadata.get("schema") != 1:
        raise SystemExit("::error::BUILD-METADATA.json schema must be integer 1")
    if metadata.get("repository") != "https://github.com/wakemeup0/controwly":
        raise SystemExit("::error::BUILD-METADATA.json repository is not the canonical HTTPS URL")
    record_tag = expected_tag or "v" + version
    if metadata.get("tag") != record_tag or metadata.get("version") != version:
        raise SystemExit("::error::BUILD-METADATA.json tag/version do not match the release")
    record_commit = metadata.get("commit")
    if not isinstance(record_commit, str) or not re.fullmatch(r"[0-9a-f]{40}", record_commit):
        raise SystemExit("::error::BUILD-METADATA.json commit must be a lowercase full 40-character SHA")
    if expected_commit and record_commit != expected_commit:
        raise SystemExit("::error::BUILD-METADATA.json commit does not match the prepared release commit")
    record_artifacts = metadata.get("artifacts")
    expected_record_names = expected - {"BUILD-METADATA.json", "SHA256SUMS"}
    if not isinstance(record_artifacts, dict) or set(record_artifacts) != expected_record_names:
        raise SystemExit(
            "::error::BUILD-METADATA.json artifacts must cover exactly the payloads, signatures, and latest.json; "
            f"expected {sorted(expected_record_names)}, got {sorted(record_artifacts) if isinstance(record_artifacts, dict) else record_artifacts!r}"
        )
    if any(
        not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest)
        for digest in record_artifacts.values()
    ):
        raise SystemExit("::error::BUILD-METADATA.json artifacts must contain lowercase SHA-256 digests")
    for name, expected_digest in record_artifacts.items():
        actual_digest = hashlib.sha256((root / name).read_bytes()).hexdigest()
        if actual_digest != expected_digest:
            raise SystemExit(f"::error::BUILD-METADATA.json digest mismatch for {name}")
    canonical_metadata = json.dumps(metadata, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    if raw_metadata != canonical_metadata.encode("utf-8"):
        raise SystemExit("::error::BUILD-METADATA.json is not in the required deterministic serialization")

actual_names = names
if actual_names != expected_files:
    raise SystemExit(f"::error::unexpected release artifact set; expected {sorted(expected_files)}, got {sorted(actual_names)}")

if mode == "all":
    checksum_path = root / "SHA256SUMS"
    checksum_lines = checksum_path.read_text(encoding="utf-8").splitlines()
    entries: dict[str, str] = {}
    for line in checksum_lines:
        parts = line.split()
        if len(parts) != 2 or not re.fullmatch(r"[0-9a-fA-F]{64}", parts[0]) or not parts[1]:
            raise SystemExit(f"::error::invalid SHA256SUMS line: {line!r}")
        if parts[1] in entries:
            raise SystemExit(f"::error::duplicate SHA256SUMS entry for {parts[1]}")
        entries[parts[1]] = parts[0].lower()
    expected_checksum_names = sorted(expected_files - {"SHA256SUMS"})
    if sorted(entries) != expected_checksum_names:
        raise SystemExit(f"::error::SHA256SUMS must cover exactly {expected_checksum_names}; got {sorted(entries)}")
    for name, expected_digest in entries.items():
        actual_digest = hashlib.sha256((root / name).read_bytes()).hexdigest()
        if actual_digest != expected_digest:
            raise SystemExit(f"::error::SHA256SUMS digest mismatch for {name}")

print(f"Verified {mode} release artifacts for Controwly {version}: {', '.join(sorted(expected_files))}")
PY
