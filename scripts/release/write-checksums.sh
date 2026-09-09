#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "::error::usage: write-checksums.sh <artifact-dir>" >&2
  exit 2
fi

python3 - "$1" <<'PY'
from __future__ import annotations

import hashlib
import os
import sys
from pathlib import Path

root = Path(sys.argv[1])
if not root.is_dir():
    raise SystemExit(f"::error::artifact directory does not exist: {root}")
files = sorted((path for path in root.iterdir() if path.name != "SHA256SUMS"), key=lambda p: p.name)
if not files:
    raise SystemExit("::error::cannot create SHA256SUMS for an empty artifact directory")
lines: list[str] = []
for path in files:
    if path.is_symlink() or not path.is_file():
        raise SystemExit(f"::error::checksum input is not a regular file: {path.name}")
    if "\n" in path.name or "\r" in path.name or any(char.isspace() for char in path.name):
        raise SystemExit(f"::error::checksum input has unsupported whitespace in filename: {path.name!r}")
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    lines.append(f"{digest}  {path.name}")
output = root / "SHA256SUMS"
temporary = root / f".SHA256SUMS.tmp.{os.getpid()}"
temporary.write_text("\n".join(lines) + "\n", encoding="utf-8")
os.replace(temporary, output)
print(f"Wrote deterministic SHA256SUMS covering {len(lines)} assets.")
PY
