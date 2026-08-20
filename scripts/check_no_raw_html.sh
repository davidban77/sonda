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

# The roots this gate claims to cover. Keep them here rather than inline in the
# `find` below, so the corpus assertion and the scan read from one list.
ROOTS='docs/site/docs/javascripts
docs/site/tools/editor/src'

# Assert the corpus BEFORE checking it. `find` over a moved or renamed
# directory prints nothing and exits 0, so without this the gate scans zero
# files, reports "OK" and reads as coverage — the failure this whole section
# of CLAUDE.md exists about.
#
# Checked per root, not in total: a single count would still be satisfied by
# one surviving directory while the other had silently stopped being scanned.
# Counted AFTER the generated-file exemption, because a root holding nothing
# but build outputs contributes nothing to this gate either.
corpus=""
while IFS= read -r root; do
  [ -n "$root" ] || continue
  if [ ! -d "$root" ]; then
    echo "::error::check_no_raw_html.sh: scan root '$root' is not a directory." >&2
    echo "It was moved, renamed or deleted. Update ROOTS in this script to match —" >&2
    echo "this gate refuses to pass over a corpus it cannot find." >&2
    exit 2
  fi
  kept=0
  while IFS= read -r found; do
    [ -n "$found" ] || continue
    case "$GENERATED" in
      *"$found"*) continue ;;
    esac
    corpus="$corpus$found
"
    kept=$((kept + 1))
  done < <(find "$root" -name '*.js' -type f | sort)
  if [ "$kept" -eq 0 ]; then
    echo "::error::check_no_raw_html.sh: scan root '$root' contributed no hand-written .js files." >&2
    echo "Either the sources moved, or every file in it is on the GENERATED exemption list." >&2
    echo "Refusing to report OK over an empty corpus." >&2
    exit 2
  fi
done <<EOF
$ROOTS
EOF

checked=0
status=0
while IFS= read -r file; do
  [ -n "$file" ] || continue
  checked=$((checked + 1))
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
done <<EOF
$corpus
EOF

if [ "$status" -eq 0 ]; then
  # Print the count: a reader can see the corpus was non-empty without
  # trusting that the assertion above ran.
  echo "OK: no raw-markup assignment in $checked hand-written docs JS files"
fi
exit "$status"
