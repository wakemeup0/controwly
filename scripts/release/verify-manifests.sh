#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
VERSION="${1:-}"
if [[ ! "$VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "::error::usage: verify-manifests.sh <semantic-version>" >&2
  exit 2
fi

python3 - "$VERSION" <<'PY'
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

version = sys.argv[1]
root = Path.cwd()
package_path = root / "package.json"
package_lock_path = root / "package-lock.json"
cargo_path = root / "src-tauri" / "Cargo.toml"
cargo_lock_path = root / "src-tauri" / "Cargo.lock"
tauri_path = root / "src-tauri" / "tauri.conf.json"
required = (package_path, package_lock_path, cargo_path, cargo_lock_path, tauri_path)
for path in required:
    if not path.is_file():
        raise SystemExit(f"::error::missing release metadata file: {path.relative_to(root)}")

try:
    package = json.loads(package_path.read_text(encoding="utf-8"))
    package_lock = json.loads(package_lock_path.read_text(encoding="utf-8"))
    tauri = json.loads(tauri_path.read_text(encoding="utf-8"))
except (OSError, json.JSONDecodeError) as exc:
    raise SystemExit(f"::error::invalid JSON release metadata: {exc}")

package_version = package.get("version")
if package.get("name") != "controwly" or package_version != version:
    raise SystemExit(
        f"::error::package.json must be controwly {version}; got {package.get('name')!r} {package_version!r}"
    )

lock_root = package_lock.get("packages", {}).get("")
if (
    package_lock.get("name") != "controwly"
    or package_lock.get("version") != version
    or not isinstance(lock_root, dict)
    or lock_root.get("name") != "controwly"
    or lock_root.get("version") != version
):
    raise SystemExit("::error::package-lock.json must identify controwly at the requested version in both root entries")

try:
    cargo_lock_text = cargo_lock_path.read_text(encoding="utf-8")
except OSError as exc:
    raise SystemExit(f"::error::cannot read src-tauri/Cargo.lock: {exc}")
lock_entries = re.findall(
    r"(?ms)^\[\[package\]\]\s*\n(.*?)(?=^\[\[package\]\]|\Z)",
    cargo_lock_text,
)
local_versions = []
for entry in lock_entries:
    name_match = re.search(r'(?m)^name\s*=\s*"([^"]+)"\s*$', entry)
    if name_match and name_match.group(1) == "controwly":
        version_match = re.search(r'(?m)^version\s*=\s*"([^"]+)"\s*$', entry)
        local_versions.append(version_match.group(1) if version_match else None)
if local_versions != [version]:
    raise SystemExit(
        "::error::src-tauri/Cargo.lock must contain exactly one local controwly package "
        f"at {version}; got {local_versions!r}"
    )

cargo_text = cargo_path.read_text(encoding="utf-8")

package_section_match = re.search(r"(?ms)^\[package\]\s*(.*?)(?=^\[[^\n]+\]|\Z)", cargo_text)
if not package_section_match:
    raise SystemExit("::error::Cargo.toml has no [package] section")
package_section = package_section_match.group(1)
cargo_name_match = re.search(r'(?m)^name\s*=\s*["\']([^"\']+)["\']\s*$', package_section)
cargo_version_match = re.search(r'(?m)^version\s*=\s*["\']([^"\']+)["\']\s*$', package_section)
if not cargo_version_match:
    workspace_match = re.search(r"(?ms)^\[workspace\.package\]\s*(.*?)(?=^\[[^\n]+\]|\Z)", cargo_text)
    cargo_version_match = re.search(
        r'(?m)^version\s*=\s*["\']([^"\']+)["\']\s*$',
        workspace_match.group(1) if workspace_match else "",
    )
if (cargo_name_match.group(1) if cargo_name_match else None) != "controwly" or not cargo_version_match or cargo_version_match.group(1) != version:
    got = cargo_version_match.group(1) if cargo_version_match else None
    name = cargo_name_match.group(1) if cargo_name_match else None
    raise SystemExit(f"::error::Cargo package must be controwly {version}; got {name!r} {got!r}")
license_match = re.search(r'(?m)^license\s*=\s*["\']([^"\']+)["\']\s*$', package_section)
repository_match = re.search(r'(?m)^repository\s*=\s*["\']([^"\']+)["\']\s*$', package_section)
authors_match = re.search(r'(?m)^authors\s*=\s*\[([^\]]*)\]', package_section)
if not license_match or license_match.group(1) != "Apache-2.0":
    raise SystemExit("::error::Cargo package must declare license = Apache-2.0")
if not repository_match or repository_match.group(1) != "https://github.com/wakemeup0/controwly":
    raise SystemExit("::error::Cargo package repository must be https://github.com/wakemeup0/controwly")
if not authors_match or "Wakemeup" not in authors_match.group(1):
    raise SystemExit("::error::Cargo package authors must identify Wakemeup")
license_path = root / "LICENSE"
notice_path = root / "THIRD_PARTY_NOTICES"
if not license_path.is_file() or "Apache License" not in license_path.read_text(encoding="utf-8"):
    raise SystemExit("::error::root Apache-2.0 LICENSE is missing")
if not notice_path.is_file() or "Copyright (c) 2023 shadcn" not in notice_path.read_text(encoding="utf-8"):
    raise SystemExit("::error::root THIRD_PARTY_NOTICES must retain shadcn MIT attribution")

if tauri.get("version") != version:
    raise SystemExit(f"::error::Tauri version must be {version}; got {tauri.get('version')!r}")
if tauri.get("productName") != "Controwly":
    raise SystemExit(f"::error::Tauri productName must be Controwly; got {tauri.get('productName')!r}")
if tauri.get("identifier") != "io.github.wakemeup0.controwly":
    raise SystemExit(f"::error::Tauri identifier must be io.github.wakemeup0.controwly; got {tauri.get('identifier')!r}")
plugins = tauri.get("plugins")
updater = plugins.get("updater") if isinstance(plugins, dict) else None
if not isinstance(updater, dict):
    raise SystemExit("::error::Tauri plugins.updater configuration is required for signed updates")
pubkey = updater.get("pubkey")
if not isinstance(pubkey, str) or not pubkey.strip() or re.search(r"(?:TODO|REPLACE|YOUR[_ -]?KEY|CHANGE[_ -]?ME|PLACEHOLDER)", pubkey, re.I):
    raise SystemExit("::error::Tauri updater pubkey is missing or still a placeholder")
endpoints = updater.get("endpoints")
expected_endpoint = "https://github.com/wakemeup0/controwly/releases/latest/download/latest.json"
if not isinstance(endpoints, list) or endpoints != [expected_endpoint]:
    raise SystemExit(f"::error::Tauri updater endpoints must be exactly [{expected_endpoint!r}]")
updater_windows = updater.get("windows")
if not isinstance(updater_windows, dict) or updater_windows.get("installMode") not in {"passive", "basicUi", "quiet"}:
    raise SystemExit("::error::Tauri updater.windows.installMode must be passive, basicUi, or quiet")

bundle = tauri.get("bundle")
if not isinstance(bundle, dict):
    raise SystemExit("::error::Tauri bundle configuration is required")
targets = bundle.get("targets")
if not isinstance(targets, list) or len(targets) != 2 or set(targets) != {"deb", "nsis"}:
    raise SystemExit("::error::Tauri bundle.targets must be exactly the supported deb and nsis installers")
updater_artifacts = bundle.get("createUpdaterArtifacts")
if updater_artifacts is not True and updater_artifacts != "v1Compatible":
    raise SystemExit("::error::Tauri bundle.createUpdaterArtifacts must be JSON true or v1Compatible for signed updates")
if bundle.get("publisher") != "Wakemeup":
    raise SystemExit(f"::error::Tauri bundle.publisher must be Wakemeup; got {bundle.get('publisher')!r}")
if "Wakemeup" not in str(bundle.get("copyright", "")):
    raise SystemExit("::error::Tauri bundle.copyright must identify Wakemeup")
linux = bundle.get("linux")
deb = linux.get("deb") if isinstance(linux, dict) else None
if not isinstance(deb, dict):
    raise SystemExit("::error::Tauri bundle.linux.deb configuration is required for the supported Linux release")
if deb.get("postInstallScript") != "../packaging/linux/controwly.postinst":
    raise SystemExit("::error::Tauri DEB postInstallScript must invoke packaging/linux/controwly.postinst")
depends = deb.get("depends")
if not isinstance(depends, list) or "pkexec" not in depends:
    raise SystemExit("::error::Tauri DEB depends must include pkexec for the root-owned helper lifecycle")
deb_files = deb.get("files")
expected_deb_files = {
    "/usr/libexec/controwly-linux-input-helper": "../target/release/controwly-linux-input-helper",
    "/usr/share/polkit-1/actions/org.controwly.input.policy": "../packaging/linux/org.controwly.input.policy",
    "/usr/lib/tmpfiles.d/controwly-recovery.conf": "../packaging/linux/controwly-recovery.tmpfiles",
    "/usr/share/doc/controwly/THIRD_PARTY_NOTICES": "../THIRD_PARTY_NOTICES",
    "/usr/share/doc/controwly/LICENSE": "../LICENSE",
}
if not isinstance(deb_files, dict) or any(deb_files.get(destination) != source for destination, source in expected_deb_files.items()):
    raise SystemExit("::error::Tauri DEB files must map helper, polkit, tmpfiles, LICENSE, and third-party notice from real repository sources to fixed destinations")
if deb.get("preRemoveScript") != "../packaging/linux/controwly.prerm":
    raise SystemExit("::error::Tauri DEB preRemoveScript must invoke packaging/linux/controwly.prerm")

print(f"Release metadata verified for Controwly {version}: package/Cargo/Tauri, updater, and bundle targets agree.")
PY
