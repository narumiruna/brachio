#!/usr/bin/env bash
set -euo pipefail

: "${BASE_BRANCH:?PR base branch is required}"
: "${GITHUB_OUTPUT:?Action output path is required}"
since="refs/remotes/origin/$BASE_BRANCH"
base=$(git merge-base "$since" HEAD)
changes=$(mktemp)
trap 'rm -f -- "$changes"' EXIT
git diff --no-ext-diff --no-textconv --no-renames --name-only -z "$base" HEAD -- > "$changes"

has_changes=false
non_release_only=true
release_outputs_only=true
while IFS= read -r -d '' path; do
  has_changes=true
  case "$path" in
    docs/*|tests/*|.github/*|.changenote/*.md|.changenote/config.json|README.md|AGENTS.md|prek.toml) ;;
    *) non_release_only=false ;;
  esac
  case "$path" in
    .changenote/releases.json|.changenote/*.md|Cargo.toml|Cargo.lock|CHANGELOG.md) ;;
    *) release_outputs_only=false ;;
  esac
done < "$changes"

skip_release=false
reason=''
if [[ "$has_changes" == true && "$non_release_only" == true ]]; then
  skip_release=true
  reason="Only documentation, tests, CI, repository guidance, or changenote files changed."
elif [[ "$has_changes" == true && "$release_outputs_only" == true &&
        -n "${GITHUB_EVENT_PATH:-}" && -n "${GITHUB_REPOSITORY:-}" ]] &&
  jq -e --arg base "$BASE_BRANCH" --arg repo "$GITHUB_REPOSITORY" '
    .pull_request.base.ref == $base and
    .pull_request.base.repo.full_name == $repo and
    (.pull_request.head.ref == ("changenote-release/" + $base) or
     .pull_request.head.ref == ("narumi/chore/changenote-release-" + $base)) and
    .pull_request.head.repo.full_name == $repo and
    ((.pull_request.body // "") | startswith("<!-- changenote-release-pr -->"))
  ' "$GITHUB_EVENT_PATH" > /dev/null; then
  skip_release=true
  reason="Managed Release PR updates only release outputs and consumes existing changenote entries."
fi
printf 'skip-release=%s\nreason=%s\n' "$skip_release" "$reason" >> "$GITHUB_OUTPUT"
