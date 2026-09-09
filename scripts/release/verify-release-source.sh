#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 2 ]]; then
  echo "::error::usage: verify-release-source.sh <full-commit-sha> <semantic-version-tag>" >&2
  exit 2
fi
EXPECTED_COMMIT="$1"
TAG="$2"
if [[ ! "$EXPECTED_COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "::error::expected release commit must be a lowercase full 40-character SHA" >&2
  exit 2
fi
if [[ ! "$TAG" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "::error::release tag must be a stable semantic version prefixed with v" >&2
  exit 2
fi

head_ref='HEAD^{commit}'
checked_out_commit="$(git rev-parse --verify "$head_ref")"
if [[ "$checked_out_commit" != "$EXPECTED_COMMIT" ]]; then
  echo "::error::checkout resolved to ${checked_out_commit}, expected immutable release commit ${EXPECTED_COMMIT}" >&2
  exit 1
fi

# Refresh only the requested tag from origin. A force update is intentional:
# if the remote tag moved after prepare, the exact comparison below fails in
# every downstream job instead of silently building or publishing new source.
git fetch --force --no-tags origin "refs/tags/${TAG}:refs/tags/${TAG}"
tag_ref="${TAG}^{commit}"
tag_commit="$(git rev-parse --verify "$tag_ref")"
if [[ "$tag_commit" != "$EXPECTED_COMMIT" ]]; then
  echo "::error::release tag ${TAG} resolves to ${tag_commit}, not the prepared commit ${EXPECTED_COMMIT}; refusing release" >&2
  exit 1
fi

printf 'Verified immutable release source: commit %s, tag %s.\n' "$EXPECTED_COMMIT" "$TAG"
