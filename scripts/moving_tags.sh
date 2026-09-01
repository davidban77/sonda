#!/usr/bin/env bash
# Which moving tags may point at a release: MOVE_LATEST / MOVE_MAJOR /
# MOVE_MINOR, as `KEY=value` lines for $GITHUB_ENV. Notes go to stderr.
#
# One rule for every moving tag: it may move onto a release only if that
# release is the highest among the releases the tag denotes — `latest` over
# all of them, `vN`/`N` over its major line, `N.M` over its minor line.
# Nothing moves backwards: a backport cut after a newer release must not
# drag consumers pinned to a moving tag onto older code (#554 review W3).
#
# The git `vN` tag and the image tags are decided in different jobs, on
# different runners. Both call this, so there is one definition to be wrong.
#
# Usage: bash scripts/moving_tags.sh v1.22.3
set -uo pipefail

release_tag="${1-}"
if [ "$#" -ne 1 ] || [ -z "$release_tag" ]; then
  echo "usage: moving_tags.sh <vX.Y.Z>" >&2
  exit 2
fi

# An empty tag list reads as "nothing is newer", so a git that cannot answer
# would open every moving tag. Refuse instead.
if ! git rev-parse --git-dir > /dev/null 2>&1; then
  echo "error: not a git repository; refusing to decide moving tags" >&2
  exit 1
fi

if ! printf '%s' "$release_tag" | grep -qE '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "note: ${release_tag} is not a plain vX.Y.Z tag; no moving tag may point at it" >&2
  printf 'MOVE_LATEST=false\nMOVE_MAJOR=false\nMOVE_MINOR=false\n'
  exit 0
fi

# The comparisons below read the local tag list. A repository that has one but
# has not fetched it — a shallow clone, a checkout without tags — answers
# "nothing is newer" to every question, which is the same vacuous `true` a
# missing repository would give. The release being decided must itself be in
# that list for any answer to mean anything.
if ! git rev-parse -q --verify "refs/tags/${release_tag}" > /dev/null 2>&1; then
  echo "error: ${release_tag} is not among the fetched tags; refusing to decide moving tags" >&2
  exit 1
fi

decide() {
  local name="$1" pattern="$2" highest
  # Compared by version, not by release date, and not as strings — v1.9.0
  # sorts above v1.20.0 as text.
  highest="$(git tag -l "$pattern" | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | sort -V | tail -1)"
  if [ -n "$highest" ] && [ "$highest" != "$release_tag" ]; then
    echo "note: ${highest} is newer than ${release_tag}; ${name} stays where it is" >&2
    printf 'false'
  else
    printf 'true'
  fi
}

major="${release_tag%%.*}" # v1.22.3 -> v1
minor="${release_tag%.*}"  # v1.22.3 -> v1.22

printf 'MOVE_LATEST=%s\n' "$(decide latest 'v*')"
printf 'MOVE_MAJOR=%s\n' "$(decide "$major" "${major}.*")"
printf 'MOVE_MINOR=%s\n' "$(decide "$minor" "${minor}.*")"
