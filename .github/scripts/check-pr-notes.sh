#!/usr/bin/env bash
set -euo pipefail

: "${BASE_BRANCH:?PR base branch is required}"
since="refs/remotes/origin/$BASE_BRANCH"
base=$(git merge-base "$since" HEAD)
changes=$(mktemp)
trap 'rm -f -- "$changes"' EXIT
git diff --no-ext-diff --no-textconv --no-renames --name-only -z "$base" HEAD -- > "$changes"

has_changes=false
non_release_only=true
release_outputs_only=true
batch_changed=false
while IFS= read -r -d '' path; do
  has_changes=true
  case "$path" in
    docs/*|tests/*|.github/*|.changenote/*.md|.changenote/config.json|README.md|AGENTS.md) ;;
    *) non_release_only=false ;;
  esac
  case "$path" in
    .changenote/releases.json) batch_changed=true ;;
    .changenote/*.md|Cargo.toml|Cargo.lock|CHANGELOG.md) ;;
    *) release_outputs_only=false ;;
  esac
done < "$changes"

args=(check "--since=$since" --coverage=package)
if [[ "$has_changes" == true && "$non_release_only" == true ]]; then
  args+=(--skip-release --reason "Only documentation, tests, CI, repository guidance, or changenote files changed.")
elif [[ "$release_outputs_only" == true && "$batch_changed" == true &&
        -f .changenote/releases.json && ! -L .changenote/releases.json &&
        -n "${GITHUB_EVENT_PATH:-}" && -n "${GITHUB_REPOSITORY:-}" ]] &&
  jq -e --arg base "$BASE_BRANCH" --arg repo "$GITHUB_REPOSITORY" '
    .pull_request.base.ref == $base and
    .pull_request.base.repo.full_name == $repo and
    .pull_request.head.ref == ("narumi/chore/changenote-release-" + $base) and
    .pull_request.head.repo.full_name == $repo and
    ((.pull_request.body // "") | startswith("<!-- changenote-release-pr -->"))
  ' "$GITHUB_EVENT_PATH" > /dev/null; then
  args+=(--skip-release --reason "Managed Release PR updates only release outputs and consumes existing changenote entries.")
fi
uvx changenote==0.9.2 "${args[@]}"
