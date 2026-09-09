#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 2 && "$#" -ne 3 ]]; then
  echo "::error::usage: publish-assets.sh <tag> <artifact-dir> [owner/repository]" >&2
  exit 2
fi
TAG="$1"
ARTIFACT_DIR="$2"
REPOSITORY="${3:-wakemeup0/controwly}"
if [[ ! "$TAG" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "::error::asset publisher requires an immutable semantic-version tag such as v0.1.0" >&2
  exit 2
fi
if [[ "$REPOSITORY" != "wakemeup0/controwly" ]]; then
  echo "::error::asset publisher is scoped to wakemeup0/controwly" >&2
  exit 2
fi
if [[ ! -d "$ARTIFACT_DIR" ]]; then
  echo "::error::artifact directory does not exist: $ARTIFACT_DIR" >&2
  exit 1
fi
if ! command -v gh >/dev/null 2>&1; then
  echo "::error::GitHub CLI (gh) is required; authenticate with the workflow GITHUB_TOKEN" >&2
  exit 1
fi

if ! release_pages="$(gh api --paginate --slurp "repos/${REPOSITORY}/releases?per_page=100" 2>/dev/null)"; then
  echo "::error::cannot list GitHub releases; authenticate with the workflow GITHUB_TOKEN" >&2
  exit 1
fi
matching_releases="$(jq -c --arg tag "$TAG" 'add | map(select(.tag_name == $tag))' <<<"$release_pages")"
matching_count="$(jq -er 'length' <<<"$matching_releases")"
if [[ "$matching_count" != 1 ]]; then
  echo "::error::expected exactly one GitHub release for ${TAG}; found ${matching_count}" >&2
  exit 1
fi
release_json="$(jq -c '.[0]' <<<"$matching_releases")"
release_id="$(jq -er '.id' <<<"$release_json")"
if [[ "$(jq -er '.draft' <<<"$release_json")" != true ]]; then
  echo "::error::release ${TAG} is already published; refusing to replace immutable release assets" >&2
  exit 1
fi

asset_tmp="$(mktemp -d "${RUNNER_TEMP:-/tmp}/controwly-assets.XXXXXXXX")"
trap 'rm -rf "$asset_tmp"' EXIT

assets_json() {
  gh api --paginate "repos/${REPOSITORY}/releases/${release_id}/assets?per_page=100"
}

verify_remote_asset() {
  local name="$1"
  local local_path="$2"
  local remote_id="$3"
  local remote_digest="$4"
  local local_digest
  local_digest="$(sha256sum "$local_path" | awk '{ print $1 }')"
  if [[ "$remote_digest" == "sha256:${local_digest}" ]]; then
    return 0
  fi
  local downloaded="${asset_tmp}/${name}"
  gh api -H 'Accept: application/octet-stream' "repos/${REPOSITORY}/releases/assets/${remote_id}" > "$downloaded"
  local actual
  actual="$(sha256sum "$downloaded" | awk '{ print $1 }')"
  if [[ "$actual" != "$local_digest" ]]; then
    echo "::error::remote asset ${name} differs from the reviewed local bytes; refusing overwrite" >&2
    return 1
  fi
}

local_file=""
while IFS= read -r -d '' local_file; do
  name="$(basename "$local_file")"
  current_assets="$(assets_json)"
  remote_id="$(jq -er --arg name "$name" '[.[] | select(.name == $name)] | if length == 1 then .[0].id else empty end' <<<"$current_assets" 2>/dev/null || true)"
  if [[ -n "$remote_id" ]]; then
    remote_digest="$(jq -r --arg name "$name" '[.[] | select(.name == $name)] | if length == 1 then .[0].digest // "" else "" end' <<<"$current_assets")"
    verify_remote_asset "$name" "$local_file" "$remote_id" "$remote_digest"
    echo "Retained verified draft asset ${name}."
  else
    echo "Uploading missing draft asset ${name}."
    gh release upload "$TAG" "$local_file" --repo "$REPOSITORY"
  fi
done < <(find "$ARTIFACT_DIR" -maxdepth 1 -type f -print0 | sort -z)

final_assets="$(assets_json)"
expected_names="$(find "$ARTIFACT_DIR" -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort)"
remote_names="$(jq -r '.[].name' <<<"$final_assets" | LC_ALL=C sort)"
if [[ "$expected_names" != "$remote_names" ]]; then
  echo "::error::draft release asset set is not exact; refusing publication" >&2
  echo "::error::expected: ${expected_names//$'\n'/, }" >&2
  echo "::error::remote: ${remote_names//$'\n'/, }" >&2
  exit 1
fi

while IFS= read -r -d '' local_file; do
  name="$(basename "$local_file")"
  remote_id="$(jq -er --arg name "$name" '[.[] | select(.name == $name)] | .[0].id' <<<"$final_assets")"
  remote_digest="$(jq -r --arg name "$name" '[.[] | select(.name == $name)] | .[0].digest // ""' <<<"$final_assets")"
  verify_remote_asset "$name" "$local_file" "$remote_id" "$remote_digest"
done < <(find "$ARTIFACT_DIR" -maxdepth 1 -type f -print0 | sort -z)

echo "Verified exact immutable draft asset set for ${REPOSITORY} ${TAG}."
