#!/usr/bin/env bash
set -euo pipefail

: "${BASE_BRANCH:?PR base branch is required}"
args=(check "--since=refs/remotes/origin/$BASE_BRANCH" --coverage=package)
if [[ "${SKIP_RELEASE:-false}" == "true" ]]; then
  args+=(--skip-release --reason "Maintainer approved a note exemption; see the PR description.")
fi
uvx changenote==0.9.2 "${args[@]}"
