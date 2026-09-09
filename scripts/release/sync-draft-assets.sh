#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 2 ]]; then
  echo "::error::usage: sync-draft-assets.sh <tag> <artifact-dir>" >&2
  exit 2
fi
TAG="$1"
ARTIFACT_DIR="$2"
REPOSITORY="${GITHUB_REPOSITORY:-wakemeup0/controwly}"
if [[ "$REPOSITORY" != "wakemeup0/controwly" ]]; then
  echo "::error::draft reconciliation is scoped to wakemeup0/controwly" >&2
  exit 2
fi
if [[ ! -d "$ARTIFACT_DIR" ]]; then
  echo "::error::artifact directory does not exist: $ARTIFACT_DIR" >&2
  exit 1
fi
if ! command -v gh >/dev/null 2>&1 || ! command -v jq >/dev/null 2>&1; then
  echo "::error::gh and jq are required to reconcile a draft release" >&2
  exit 1
fi

release_pages="$(gh api --paginate --slurp "repos/${REPOSITORY}/releases?per_page=100")"
matching_releases="$(jq -c --arg tag "$TAG" 'add | map(select(.tag_name == $tag))' <<<"$release_pages")"
if [[ "$(jq -er 'length' <<<"$matching_releases")" != 1 ]]; then
  echo "::error::expected exactly one GitHub release for ${TAG}" >&2
  exit 1
fi
release_json="$(jq -c '.[0]' <<<"$matching_releases")"
if [[ "$(jq -er '.draft' <<<"$release_json")" != true ]]; then
  echo "::error::release ${TAG} is already published; refusing draft reconciliation" >&2
  exit 1
fi
release_id="$(jq -er '.id' <<<"$release_json")"
assets="$(gh api --paginate --slurp "repos/${REPOSITORY}/releases/${release_id}/assets?per_page=100" | jq -c 'add')"
if [[ "$(jq -er '[.[].name] | length == (unique | length)' <<<"$assets")" != true ]]; then
  echo "::error::draft release contains duplicate asset names; refusing reconciliation" >&2
  exit 1
fi

while IFS= read -r name; do
  case "$name" in
    */*|*..*) echo "::error::draft asset name is not a safe basename: ${name}" >&2; exit 1 ;;
    *.deb|*.deb.sig|*-setup.exe|*setup.exe|*-setup.exe.sig|*setup.exe.sig|latest.json|BUILD-METADATA.json|SHA256SUMS) ;;
    *) echo "::error::draft release contains unexpected asset ${name}; refusing deletion or replacement" >&2; exit 1 ;;
  esac
done < <(jq -r '.[].name' <<<"$assets")

mapfile -t local_debs < <(
  find "$ARTIFACT_DIR" -maxdepth 1 -type f -name '*.deb' -printf '%f\n' | LC_ALL=C sort
)
mapfile -t local_installers < <(
  find "$ARTIFACT_DIR" -maxdepth 1 -type f -iname '*setup.exe' -printf '%f\n' | LC_ALL=C sort
)
if [[ "${#local_debs[@]}" -ne 1 || "${#local_installers[@]}" -ne 1 ]]; then
  echo "::error::draft reconciliation requires exactly one local DEB and one local NSIS installer; found ${#local_debs[@]} DEB(s) and ${#local_installers[@]} installer(s)" >&2
  exit 1
fi
local_payloads=("${local_debs[@]}" "${local_installers[@]}")
for payload in "${local_payloads[@]}"; do
  signature="${payload}.sig"
  if [[ ! -s "${ARTIFACT_DIR}/${payload}" || ! -s "${ARTIFACT_DIR}/${signature}" ]]; then
    echo "::error::local updater pair is missing or empty: ${payload}" >&2
    exit 1
  fi
done

sync_tmp="$(mktemp -d "${RUNNER_TEMP:-/tmp}/controwly-draft-sync.XXXXXXXX")"
trap 'rm -rf "$sync_tmp"' EXIT

asset_json() {
  local name="$1"
  jq -c --arg name "$name" '[.[] | select(.name == $name)] | if length == 1 then .[0] else empty end' <<<"$assets"
}

asset_id() {
  local name="$1"
  jq -er '.id // empty' <<<"$(asset_json "$name")" 2>/dev/null || true
}

asset_digest() {
  local name="$1"
  jq -r '.digest // ""' <<<"$(asset_json "$name")"
}

download_asset() {
  local name="$1"
  local id="$2"
  local destination="$3"
  local expected_digest="$4"
  gh api -H 'Accept: application/octet-stream' \
    "repos/${REPOSITORY}/releases/assets/${id}" >"$destination"
  [[ -s "$destination" ]] || {
    echo "::error::downloaded draft asset ${name} is empty" >&2
    exit 1
  }
  if [[ -n "$expected_digest" ]]; then
    actual_digest="sha256:$(sha256sum "$destination" | awk '{ print $1 }')"
    [[ "$actual_digest" == "$expected_digest" ]] || {
      echo "::error::GitHub digest mismatch while downloading draft asset ${name}" >&2
      exit 1
    }
  fi
}

all_pairs_complete=true
for payload in "${local_payloads[@]}"; do
  signature="${payload}.sig"
  payload_id="$(asset_id "$payload")"
  signature_id="$(asset_id "$signature")"
  if [[ -n "$payload_id" && -n "$signature_id" ]]; then
    download_asset "$payload" "$payload_id" "${sync_tmp}/${payload}" "$(asset_digest "$payload")"
    download_asset "$signature" "$signature_id" "${sync_tmp}/${signature}" "$(asset_digest "$signature")"
  else
    all_pairs_complete=false
  fi
done

if [[ "$all_pairs_complete" == true ]]; then
  # Both members of every remote pair were downloaded before either local
  # member is replaced. The following verification step checks the complete
  # staged pair cryptographically before any release publication.
  for payload in "${local_payloads[@]}"; do
    signature="${payload}.sig"
    mv -- "${sync_tmp}/${payload}" "${ARTIFACT_DIR}/${payload}"
    mv -- "${sync_tmp}/${signature}" "${ARTIFACT_DIR}/${signature}"
  done
  latest_id="$(asset_id latest.json)"
  metadata_id="$(asset_id BUILD-METADATA.json)"
  checksums_id="$(asset_id SHA256SUMS)"
  if [[ -n "$latest_id" && -n "$metadata_id" && -n "$checksums_id" ]]; then
    download_asset latest.json "$latest_id" "${sync_tmp}/latest.json" "$(asset_digest latest.json)"
    download_asset BUILD-METADATA.json "$metadata_id" "${sync_tmp}/BUILD-METADATA.json" "$(asset_digest BUILD-METADATA.json)"
    download_asset SHA256SUMS "$checksums_id" "${sync_tmp}/SHA256SUMS" "$(asset_digest SHA256SUMS)"
    mv -- "${sync_tmp}/latest.json" "${ARTIFACT_DIR}/latest.json"
    mv -- "${sync_tmp}/BUILD-METADATA.json" "${ARTIFACT_DIR}/BUILD-METADATA.json"
    mv -- "${sync_tmp}/SHA256SUMS" "${ARTIFACT_DIR}/SHA256SUMS"
    echo "Reused complete draft metadata; manifest and source record generation will preserve its publication date."
  else
    for orphan in "$latest_id" "$metadata_id" "$checksums_id"; do
      if [[ -n "$orphan" ]]; then
        gh api --method DELETE "repos/${REPOSITORY}/releases/assets/${orphan}" >/dev/null
      fi
    done
  fi
else
  # A complete pair may have been downloaded above, but it cannot be retained
  # when another pair is incomplete: delete every remote payload member so the
  # local fresh pairs remain a coherent set for the next upload.
  for payload in "${local_payloads[@]}"; do
    signature="${payload}.sig"
    for name in "$payload" "$signature"; do
      orphan="$(asset_id "$name")"
      if [[ -n "$orphan" ]]; then
        gh api --method DELETE "repos/${REPOSITORY}/releases/assets/${orphan}" >/dev/null
      fi
    done
  done
  # Metadata describes signatures and hashes, so an incomplete payload pair
  # cannot safely retain it. Draft-only metadata orphans are removed and will
  # be regenerated from the locally verified fresh pairs.
  for name in latest.json BUILD-METADATA.json SHA256SUMS; do
    orphan="$(asset_id "$name")"
    if [[ -n "$orphan" ]]; then
      gh api --method DELETE "repos/${REPOSITORY}/releases/assets/${orphan}" >/dev/null
    fi
  done
fi

echo "Draft asset reconciliation complete for ${REPOSITORY} ${TAG}."
