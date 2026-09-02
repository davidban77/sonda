#!/usr/bin/env bash
# The image references a release publishes, one fully-qualified `repo:tag` per
# line, on stdout. Notes go to stderr.
#
# This repository decides its own tag list rather than reading one off
# docker/metadata-action. That action's `flavor` defaults to `latest=auto`,
# under which `:latest` is emitted by the semver processor from the first tag
# entry — not by the `type=raw,value=latest` entry — so disabling that entry
# suppresses a duplicate and leaves the real one. A backported release would
# publish `:latest` no matter what the entry said. Deciding here means
# `:latest` exists because this file emitted it.
#
# The exact version is always published. Every other tag moves, so each is
# gated on scripts/moving_tags.sh: `{major}.{minor}`, `{major}` and `latest`
# appear only when that release is the highest one the tag denotes.
#
# Usage: bash scripts/image_tags.sh v1.22.3 ghcr.io/davidban77/sonda
set -uo pipefail

release_tag="${1-}"
image="${2-}"
if [ "$#" -ne 2 ] || [ -z "$release_tag" ] || [ -z "$image" ]; then
  echo "usage: image_tags.sh <vX.Y.Z> <registry/repository>" >&2
  exit 2
fi

# A repository that already carries a tag or a digest would compose into
# `repo:1.22.3:latest`, which the registry rejects only after the caller has
# decided it is publishing. Only the final path component is inspected for a
# tag: a `:` before the last `/` is a registry port, and `localhost:5000/sonda`
# is a repository like any other.
reference=""
case "${image##*/}" in *:*) reference="a tag" ;; esac
case "$image" in *@*) reference="a digest" ;; esac
if [ -n "$reference" ]; then
  echo "error: '${image}' already carries ${reference}; publishing nothing" >&2
  exit 1
fi

if ! decisions="$(bash "$(dirname "${BASH_SOURCE[0]}")/moving_tags.sh" "$release_tag")"; then
  echo "error: the moving tags for ${release_tag} are undecided; publishing nothing" >&2
  exit 1
fi

version="${release_tag#v}"
tags=("$version")
case "$decisions" in *MOVE_MINOR=true*) tags+=("${version%.*}") ;; esac
case "$decisions" in *MOVE_MAJOR=true*) tags+=("${version%%.*}") ;; esac
case "$decisions" in *MOVE_LATEST=true*) tags+=(latest) ;; esac

# Validated before anything is printed: a caller that reads this list is about
# to publish it, so a rejected tag must not arrive after three good ones.
for tag in "${tags[@]}"; do
  if ! printf '%s' "$tag" | grep -qE '^[A-Za-z0-9_][A-Za-z0-9._-]{0,127}$'; then
    echo "error: '${tag}' is not a valid image tag; publishing nothing" >&2
    exit 1
  fi
done

for tag in "${tags[@]}"; do
  printf '%s:%s\n' "$image" "$tag"
done
