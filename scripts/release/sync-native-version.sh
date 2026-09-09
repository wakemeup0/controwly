#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

python3 - <<'PY'
from __future__ import annotations

import json
import re
from pathlib import Path

root = Path.cwd()
package_path = root / "package.json"
package_lock_path = root / "package-lock.json"
cargo_path = root / "src-tauri" / "Cargo.toml"
tauri_path = root / "src-tauri" / "tauri.conf.json"

for path in (package_path, package_lock_path, cargo_path, tauri_path):
    if not path.is_file():
        raise SystemExit(f"::error::release version sync requires {path.relative_to(root)}")

try:
    package = json.loads(package_path.read_text(encoding="utf-8"))
except (OSError, json.JSONDecodeError) as exc:
    raise SystemExit(f"::error::cannot read package.json: {exc}")

version = package.get("version")
if not isinstance(version, str) or not re.fullmatch(
    r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?",
    version,
):
    raise SystemExit("::error::package.json version must be a semantic version before native synchronization")

# npm records the root package version in two package-lock locations. Keep
# those entries synchronized without reserializing or reordering the lockfile.
try:
    package_lock = json.loads(package_lock_path.read_text(encoding="utf-8"))
except (OSError, json.JSONDecodeError) as exc:
    raise SystemExit(f"::error::cannot read package-lock.json: {exc}")
if package_lock.get("name") != package.get("name"):
    raise SystemExit("::error::package-lock.json name does not match package.json")
lock_packages = package_lock.get("packages")
if not isinstance(lock_packages, dict) or not isinstance(lock_packages.get(""), dict):
    raise SystemExit("::error::package-lock.json has no root packages entry")
lock_text = package_lock_path.read_text(encoding="utf-8")
lock_text, top_replaced = re.subn(
    r'(?m)^  "version"\s*:\s*"[^"]+"',
    f'  "version": "{version}"',
    lock_text,
    count=1,
)
if top_replaced != 1:
    raise SystemExit("::error::could not locate top-level package-lock.json version")
packages_offset = lock_text.find('\n  "packages":')
root_lock_version = re.search(r'(?m)^      "version"\s*:\s*"[^"]+"', lock_text[packages_offset:])
if packages_offset < 0 or root_lock_version is None:
    raise SystemExit("::error::could not locate package-lock.json root package version")
root_start = packages_offset + root_lock_version.start()
root_end = packages_offset + root_lock_version.end()
lock_text = (
    lock_text[:root_start]
    + f'      "version": "{version}"'
    + lock_text[root_end:]
)
package_lock_path.write_text(lock_text, encoding="utf-8")

cargo_text = cargo_path.read_text(encoding="utf-8")

# Prefer the package's own version. A workspace.package version is supported
# for projects that inherit it through `version.workspace = true`.
package_match = re.search(r"(?ms)^\[package\]\s*(.*?)(?=^\[[^\n]+\]|\Z)", cargo_text)
if not package_match:
    raise SystemExit("::error::src-tauri/Cargo.toml has no [package] section")
package_section = package_match.group(1)
name_match = re.search(r"(?m)^name\s*=\s*[\"']([^\"']+)[\"']\s*$", package_section)
name = name_match.group(1) if name_match else "controwly"

version_line = re.search(r'(?m)^(version\s*=\s*["\'])([^"\']+)(["\']\s*)$', package_section)
if version_line:
    start = package_match.start(1) + version_line.start(2)
    end = package_match.start(1) + version_line.end(2)
    cargo_text = cargo_text[:start] + version + cargo_text[end:]
else:
    workspace_match = re.search(r"(?ms)^\[workspace\.package\]\s*(.*?)(?=^\[[^\n]+\]|\Z)", cargo_text)
    if not workspace_match:
        raise SystemExit("::error::Cargo.toml has no package version or workspace.package version")
    workspace_section = workspace_match.group(1)
    workspace_version = re.search(r'(?m)^(version\s*=\s*["\'])([^"\']+)(["\']\s*)$', workspace_section)
    if not workspace_version:
        raise SystemExit("::error::Cargo.toml workspace.package has no version")
    start = workspace_match.start(1) + workspace_version.start(2)
    end = workspace_match.start(1) + workspace_version.end(2)
    cargo_text = cargo_text[:start] + version + cargo_text[end:]
cargo_path.write_text(cargo_text, encoding="utf-8")

# Tauri v2 keeps `version` at the top level. Replace only that key so unrelated
# dependency/version strings cannot be rewritten accidentally.
tauri_text = tauri_path.read_text(encoding="utf-8")
try:
    tauri = json.loads(tauri_text)
except (OSError, json.JSONDecodeError) as exc:
    raise SystemExit(f"::error::cannot read src-tauri/tauri.conf.json: {exc}")
if not isinstance(tauri, dict) or not isinstance(tauri.get("version"), str):
    raise SystemExit("::error::src-tauri/tauri.conf.json must contain a top-level string version")
if not re.search(r'(?m)^(\s*"version"\s*:\s*)"[^"]+"(\s*,?\s*)$', tauri_text):
    raise SystemExit("::error::could not locate top-level Tauri version in src-tauri/tauri.conf.json")
tauri_re = re.compile(r'(?m)^(\s*"version"\s*:\s*)"[^"]+"(\s*,?\s*)$')
tauri_replacement = lambda match: f'{match.group(1)}"{version}"{match.group(2)}'
tauri_text = tauri_re.sub(tauri_replacement, tauri_text, count=1)
tauri_path.write_text(tauri_text, encoding="utf-8")

lock_path = root / "src-tauri" / "Cargo.lock"
if lock_path.is_file():
    lock_text = lock_path.read_text(encoding="utf-8")
    lock_package = re.compile(
        rf'(?ms)(^\[\[package\]\]\s*\nname\s*=\s*"{re.escape(name)}"\s*\nversion\s*=\s*")([^"]+)(")'
    )
    lock_text, replaced = lock_package.subn(rf"\g<1>{version}\g<3>", lock_text, count=1)
    if replaced:
        lock_path.write_text(lock_text, encoding="utf-8")

# Re-read all authoritative values and prove the contract before Hooversion
# creates its release commit.
updated_package = json.loads(package_path.read_text(encoding="utf-8"))
updated_package_lock = json.loads(package_lock_path.read_text(encoding="utf-8"))
updated_tauri = json.loads(tauri_path.read_text(encoding="utf-8"))
updated_cargo = cargo_path.read_text(encoding="utf-8")
updated_package_version = updated_package.get("version")
updated_lock_version = updated_package_lock.get("version")
updated_lock_root = (
    updated_package_lock.get("packages", {}).get("", {}).get("version")
)
updated_tauri_version = updated_tauri.get("version")
updated_package_match = re.search(r"(?ms)^\[package\]\s*(.*?)(?=^\[[^\n]+\]|\Z)", updated_cargo)
updated_section = updated_package_match.group(1) if updated_package_match else ""
updated_cargo_match = re.search(r'(?m)^version\s*=\s*["\']([^"\']+)["\']\s*$', updated_section)
if not updated_cargo_match:
    workspace_match = re.search(r"(?ms)^\[workspace\.package\]\s*(.*?)(?=^\[[^\n]+\]|\Z)", updated_cargo)
    updated_cargo_match = re.search(
        r'(?m)^version\s*=\s*["\']([^"\']+)["\']\s*$',
        workspace_match.group(1) if workspace_match else "",
    )
updated_cargo_version = updated_cargo_match.group(1) if updated_cargo_match else None
if {
    updated_package_version,
    updated_lock_version,
    updated_lock_root,
    updated_cargo_version,
    updated_tauri_version,
} != {version}:
    raise SystemExit(
        "::error::native version synchronization failed: "
        f"package={updated_package_version!r} "
        f"lock={updated_lock_version!r}/{updated_lock_root!r} "
        f"cargo={updated_cargo_version!r} tauri={updated_tauri_version!r}"
    )

print(f"Synchronized package.json, package-lock.json, Cargo.toml, and tauri.conf.json to {version} ({name}).")
PY
