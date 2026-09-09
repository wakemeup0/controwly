#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 2 ]]; then
  echo "::error::usage: ensure-draft-release.sh <version> <tag>" >&2
  exit 2
fi
VERSION="$1"
TAG="$2"
REPOSITORY="${GITHUB_REPOSITORY:-wakemeup0/controwly}"
if [[ "$REPOSITORY" != "wakemeup0/controwly" ]]; then
  echo "::error::draft release helper is scoped to wakemeup0/controwly" >&2
  exit 2
fi
if [[ "$TAG" != "v$VERSION" ]]; then
  echo "::error::release tag ${TAG} does not match version ${VERSION}" >&2
  exit 2
fi
if ! command -v gh >/dev/null 2>&1; then
  echo "::error::GitHub CLI (gh) is required to create the draft release" >&2
  exit 1
fi

if ! release_pages="$(gh api --paginate --slurp "repos/${REPOSITORY}/releases?per_page=100" 2>/dev/null)"; then
  echo "::error::cannot list GitHub releases; authenticate with the workflow GITHUB_TOKEN" >&2
  exit 1
fi
matching_releases="$(jq -c --arg tag "$TAG" 'add | map(select(.tag_name == $tag))' <<<"$release_pages")"
matching_count="$(jq -er 'length' <<<"$matching_releases")"
case "$matching_count" in
  1)
    release_json="$(jq -c '.[0]' <<<"$matching_releases")"
    if [[ "$(jq -er '.tag_name' <<<"$release_json")" != "$TAG" ]]; then
      echo "::error::GitHub returned a release for the wrong tag" >&2
      exit 1
    fi
    if [[ "$(jq -er '.draft' <<<"$release_json")" != true ]]; then
      echo "::error::release ${TAG} is already published; immutable retry cannot replace it" >&2
      exit 1
    fi
    echo "Reusing existing draft release ${TAG}."
    exit 0
    ;;
  0) ;;
  *)
    echo "::error::more than one GitHub release has tag ${TAG}; refusing ambiguous publication" >&2
    exit 1
    ;;
esac

# --verify-tag refuses to manufacture a tag from an arbitrary ref. Hooversion
# has already pushed the annotated tag and the build jobs validate its commit.
gh release create "$TAG" \
  --repo "$REPOSITORY" \
  --draft \
  --verify-tag \
  --title "Controwly ${VERSION}" \
  --notes "Controwly ${VERSION}: signed Tauri updater artifacts for Windows NSIS and Linux DEB. The Linux DEB installs the root-owned helper required for full controller blocking; the initial Windows NSIS installer is not Authenticode-signed."

echo "Created draft release ${TAG}; it remains unpublished until every artifact and hash is verified."
