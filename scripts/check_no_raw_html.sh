#!/usr/bin/env bash
# Fail if any hand-written docs JS builds DOM from a markup string.
#
# The docs site has no backend and no accounts, so the one classic web risk
# left is injecting text into HTML. The codebase answer is a blanket rule
# rather than a per-call-site judgement: build DOM with createElement and
# textContent, never by assigning markup. This turns that convention into a
# gate (Law 2 — a claim in a comment becomes a check in CI).
#
# Scope is the hand-written sources only. sonda-editor.js and sonda_wasm.js
# are committed BUILD OUTPUTS (esbuild and wasm-bindgen); their contents are
# decided by upstream libraries, so policing them here would fail the build
# on a dependency's internals rather than on anything a reviewer can fix.
# They are covered instead by the rebuild-and-compare drift gates.
#
# Run directly or via `task site:no-raw-html`.

set -uo pipefail

cd "$(dirname "$0")/.."

# insertAdjacentElement is the SAFE sibling and must not match, so the pattern
# names insertAdjacentHTML exactly rather than matching on "insertAdjacent".
PATTERN='\.innerHTML|\.outerHTML|insertAdjacentHTML|document\.write\('

GENERATED='docs/site/docs/javascripts/sonda-editor.js
docs/site/docs/javascripts/sonda_wasm.js'

status=0
while IFS= read -r file; do
  case "$GENERATED" in
    *"$file"*) continue ;;
  esac
  if matches=$(grep -nE "$PATTERN" "$file"); then
    if [ "$status" -eq 0 ]; then
      echo "::error::raw-markup assignment found in hand-written docs JS." >&2
      echo "Build DOM with createElement/textContent instead." >&2
    fi
    status=1
    while IFS= read -r hit; do
      echo "  $file:$hit" >&2
    done <<<"$matches"
  fi
done < <(find docs/site/docs/javascripts docs/site/tools/editor/src -name '*.js' -type f | sort)

if [ "$status" -eq 0 ]; then
  echo "OK: no raw-markup assignment in hand-written docs JS"
fi
exit "$status"
