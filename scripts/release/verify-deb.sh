#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 2 ]]; then
  echo "::error::usage: verify-deb.sh <semantic-version> <debian-package>" >&2
  exit 2
fi
VERSION="$1"
DEB="$2"
if [[ ! "$VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "::error::invalid semantic version" >&2
  exit 2
fi
if [[ ! -f "$DEB" || -L "$DEB" ]]; then
  echo "::error::DEB is missing or not a regular file: $DEB" >&2
  exit 1
fi
if ! command -v dpkg-deb >/dev/null 2>&1; then
  echo "::error::dpkg-deb is required to inspect the release package" >&2
  exit 1
fi

package_version="$(dpkg-deb -f "$DEB" Version)"
package_name="$(dpkg-deb -f "$DEB" Package)"
package_arch="$(dpkg-deb -f "$DEB" Architecture)"
[[ "$package_name" == "controwly" ]] || { echo "::error::DEB package name must be controwly (got $package_name)" >&2; exit 1; }
[[ "$package_version" == "$VERSION" ]] || { echo "::error::DEB version $package_version does not match $VERSION" >&2; exit 1; }
[[ "$package_arch" == "amd64" ]] || { echo "::error::release DEB must target amd64 (got $package_arch)" >&2; exit 1; }

control_dir="$(mktemp -d "${RUNNER_TEMP:-/tmp}/controwly-deb-control.XXXXXXXX")"
payload_dir="$(mktemp -d "${RUNNER_TEMP:-/tmp}/controwly-deb-payload.XXXXXXXX")"
trap 'rm -rf "$control_dir" "$payload_dir"' EXIT
dpkg-deb -e "$DEB" "$control_dir"
dpkg-deb -x "$DEB" "$payload_dir"

helper="$payload_dir/usr/libexec/controwly-linux-input-helper"
policy="$payload_dir/usr/share/polkit-1/actions/org.controwly.input.policy"
tmpfiles="$payload_dir/usr/lib/tmpfiles.d/controwly-recovery.conf"
license="$payload_dir/usr/share/doc/controwly/LICENSE"
notice="$payload_dir/usr/share/doc/controwly/THIRD_PARTY_NOTICES"
prerm="$control_dir/prerm"
postinst="$control_dir/postinst"
for required in "$helper" "$policy" "$tmpfiles" "$license" "$notice" "$prerm" "$postinst"; do
  if [[ ! -f "$required" || -L "$required" ]]; then
    echo "::error::DEB is missing required safe-control/license file ${required#"$payload_dir"}" >&2
    exit 1
  fi
done

helper_mode="$(stat -c '%a' "$helper")"
[[ "$helper_mode" == "755" ]] || { echo "::error::DEB helper must be mode 0755 (got $helper_mode)" >&2; exit 1; }
helper_owner="$(python3 - "$DEB" <<'PY'
import io
import subprocess
import sys
import tarfile

deb = sys.argv[1]
try:
    archive_bytes = subprocess.run(
        ["dpkg-deb", "--fsys-tarfile", deb],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    ).stdout
except subprocess.CalledProcessError as exc:
    detail = exc.stderr.decode("utf-8", errors="replace").strip()
    raise SystemExit(f"::error::dpkg-deb could not read the filesystem archive: {detail}") from exc

matches = []
with tarfile.open(fileobj=io.BytesIO(archive_bytes), mode="r:*") as archive:
    for member in archive:
        if member.name not in {
            "usr/libexec/controwly-linux-input-helper",
            "./usr/libexec/controwly-linux-input-helper",
        }:
            continue
        matches.append(member)

if len(matches) != 1:
    raise SystemExit(
        "::error::DEB filesystem archive must contain exactly one canonical "
        f"helper entry, found {len(matches)}"
    )
member = matches[0]
if not member.isfile():
    raise SystemExit("::error::DEB helper archive entry must be a regular file")
if member.uid != 0 or member.gid != 0:
    raise SystemExit(
        "::error::DEB helper archive ownership must use numeric uid/gid 0/0 "
        f"(got {member.uid}/{member.gid})"
    )
print(f"{member.uid}/{member.gid}")
PY
)"
[[ "$helper_owner" == "0/0" ]] || {
  echo "::error::DEB helper archive ownership check returned an unexpected result (got ${helper_owner:-missing})" >&2
  exit 1
}
policy_mode="$(stat -c '%a' "$policy")"
[[ "$policy_mode" == "644" ]] || { echo "::error::DEB polkit policy must be mode 0644 (got $policy_mode)" >&2; exit 1; }
python3 - "$policy" "$tmpfiles" "$prerm" "$postinst" "$notice" "$license" <<'PY'
from pathlib import Path
import sys
import xml.etree.ElementTree as ET
policy, tmpfiles, prerm, postinst, notice, license = (
    Path(value).read_text(encoding='utf-8', errors='strict') for value in sys.argv[1:]
)
try:
    policy_root = ET.fromstring(policy)
except ET.ParseError as exc:
    raise SystemExit(f'::error::DEB polkit policy is not valid XML: {exc}') from exc
if policy_root.tag != 'policyconfig':
    raise SystemExit('::error::DEB polkit policy root must be policyconfig')
expected_action = 'io.github.wakemeup0.controwly.input'
actions = [node for node in policy_root.findall('./action') if node.get('id') == expected_action]
if len(actions) != 1:
    raise SystemExit(f'::error::DEB polkit policy must declare exactly one action {expected_action}')
expected_exec_key = 'org.freedesktop.policykit.exec.path'
expected_exec_path = '/usr/libexec/controwly-linux-input-helper'
exec_annotations = [
    (node.get('key'), (node.text or '').strip())
    for node in actions[0].findall('./annotate')
    if node.get('key') == expected_exec_key
]
if exec_annotations != [(expected_exec_key, expected_exec_path)]:
    raise SystemExit(
        f'::error::DEB polkit action {expected_action} must annotate '
        f'{expected_exec_key}={expected_exec_path}'
    )
directives = []
for line in tmpfiles.splitlines():
    stripped = line.strip()
    if stripped and not stripped.startswith('#'):
        directives.append(stripped.split())
recovery_directives = [
    fields for fields in directives
    if len(fields) >= 5 and fields[0] == 'd' and fields[1] == '/var/lib/controwly'
]
if len(recovery_directives) != 1 or recovery_directives[0][2:5] != ['0755', 'root', 'root']:
    raise SystemExit(
        '::error::DEB tmpfiles policy must declare '
        'd /var/lib/controwly 0755 root root'
    )
if '--prepare-removal' not in prerm or '/usr/libexec/controwly-linux-input-helper' not in prerm:
    raise SystemExit('::error::DEB prerm does not prepare removal through the fixed root-owned helper')
if '--clear-removal-gate' not in postinst or '/usr/libexec/controwly-linux-input-helper' not in postinst:
    raise SystemExit('::error::DEB postinst does not clear the removal gate through the fixed root-owned helper')
if 'MIT License' not in notice or 'Copyright (c) 2023 shadcn' not in notice or 'src/components/ui/' not in notice:
    raise SystemExit('::error::DEB third-party notice is not the complete shadcn MIT attribution')
if 'Apache License' not in license or 'Copyright 2026 Wakemeup' not in license:
    raise SystemExit('::error::DEB first-party LICENSE is not the repository Apache-2.0 license')
PY


printf 'Verified safe Controwly DEB %s: version %s, amd64, root-owned helper, polkit/tmpfiles/license/notice, and maintainer scripts present.\n' "$(basename "$DEB")" "$VERSION"
