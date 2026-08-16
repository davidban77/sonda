#!/usr/bin/env bash
# Injection tests for the argv the composite action builds.
#
# The action's "Verify alert expectations" step is bash, so its argument
# handling cannot be covered by cargo tests. This harness extracts the same
# construction and drives it with hostile inputs, asserting on the *argv*
# a fake `sonda` receives — the only place where a value that escaped its
# quoting becomes visible.
#
# The contract under test:
#   1. scenario reaches the CLI as exactly one argument, whatever is in it
#   2. no input is ever evaluated by the shell (no command substitution,
#      no glob expansion against the workspace)
#   3. extra-args, and only extra-args, splits on whitespace
#
# Run: bash scripts/tests/action_argv_test.sh
set -uo pipefail

FAILURES=0
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# A stand-in for the real binary. Argument boundaries are reported with NUL
# separators and the count is reported separately: a newline *inside* an
# argument is a case under test here, so a line-per-argument format would
# make a correct single argument look like two.
cat > "$TMP/sonda" <<'FAKE'
#!/usr/bin/env bash
printf '%s' "$#" > "$SONDA_ARGC_FILE"
for a in "$@"; do printf '%s\0' "$a"; done > "$SONDA_ARGV_FILE"
FAKE
chmod +x "$TMP/sonda"
PATH="$TMP:$PATH"

# Verbatim from action.yml's "Verify alert expectations" step. Kept in sync
# by `assert_matches_action` below, which fails if the action drifts.
run_action_step() {
  set -euo pipefail
  if [ -n "$SONDA_PROM_URL" ] && [ -n "$SONDA_AM_URL" ]; then
    echo "mutually exclusive" >&2
    exit 1
  fi
  if [ -z "$SONDA_PROM_URL" ] && [ -z "$SONDA_AM_URL" ]; then
    echo "one of prometheus-url or alertmanager-url is required" >&2
    exit 1
  fi
  args=(test "$SONDA_SCENARIO")
  if [ -n "$SONDA_PROM_URL" ]; then
    args+=(--prometheus-url "$SONDA_PROM_URL")
  else
    args+=(--alertmanager-url "$SONDA_AM_URL")
  fi
  if [ -n "$SONDA_EXTRA_ARGS" ]; then
    set -f
    # shellcheck disable=SC2206
    extra=($SONDA_EXTRA_ARGS)
    set +f
    args+=("${extra[@]}")
  fi
  sonda "${args[@]}"
}

export SONDA_ARGC_FILE="$TMP/argc" SONDA_ARGV_FILE="$TMP/argv"

# Runs the step; leaves the observed argv in $ARGV (array) and $ARGC.
declare -a ARGV=()
ARGC=0
invoke() {
  : > "$SONDA_ARGC_FILE"; : > "$SONDA_ARGV_FILE"
  SONDA_SCENARIO="$1" \
  SONDA_PROM_URL="${3-http://localhost:9090}" \
  SONDA_AM_URL="${4-}" \
  SONDA_EXTRA_ARGS="${2:-}" \
  bash -c "$(declare -f run_action_step); run_action_step" 2>/dev/null
  ARGC="$(cat "$SONDA_ARGC_FILE" 2>/dev/null || echo 0)"
  ARGV=()
  while IFS= read -r -d '' item; do ARGV+=("$item"); done < "$SONDA_ARGV_FILE"
}

pass() { printf '  ok   %s\n' "$1"; }
fail() { printf '  FAIL %s\n     %s\n' "$1" "$2"; FAILURES=$((FAILURES + 1)); }

show() { local a; for a in "${ARGV[@]}"; do printf '<%s>' "$a"; done; }

check_scenario_is_one_arg() {
  local label="$1" scenario="$2"
  invoke "$scenario"
  # argv = [test, scenario, --prometheus-url, url]
  if [ "$ARGC" -eq 4 ] && [ "${ARGV[1]-}" = "$scenario" ]; then
    pass "$label"
  else
    fail "$label" "expected argc 4 with argv[1] intact, got argc=${ARGC}: $(show)"
  fi
}

echo "== scenario survives as exactly one argument =="
check_scenario_is_one_arg "plain path"            "examples/alert.yaml"
check_scenario_is_one_arg "spaces"                "my scenarios/alert test.yaml"
check_scenario_is_one_arg "semicolon"             "a.yaml; echo pwned"
check_scenario_is_one_arg "command substitution"  'a$(echo pwned).yaml'
check_scenario_is_one_arg "backticks"             'a`echo pwned`.yaml'
check_scenario_is_one_arg "pipe and redirect"     "a.yaml | tee /tmp/x > /dev/null"
check_scenario_is_one_arg "ampersands"            "a.yaml && rm -rf /"
check_scenario_is_one_arg "double quote"          'a"b.yaml'
check_scenario_is_one_arg "single quote"          "a'b.yaml"
check_scenario_is_one_arg "dollar var"            'a$HOME.yaml'
check_scenario_is_one_arg "newline"               'a.yaml
echo pwned'
check_scenario_is_one_arg "glob"                  "*.yaml"
check_scenario_is_one_arg "utf8 and emoji"        "café-日本-🔥.yaml"
check_scenario_is_one_arg "leading dash"          "--not-a-flag.yaml"

echo
echo "== nothing is evaluated by the shell =="
# A command substitution that would create a file if it ever ran.
CANARY="$TMP/canary"
invoke "a\$(touch $CANARY).yaml"
if [ -e "$CANARY" ]; then
  fail "command substitution never executes" "canary file was created"
else
  pass "command substitution never executes"
fi

invoke "x.yaml" "\$(touch $CANARY.extra)"
if [ -e "$CANARY.extra" ]; then
  fail "extra-args substitution never executes" "canary file was created"
else
  pass "extra-args substitution never executes"
fi

# A glob in extra-args must not expand against the working directory.
#
# This check is only meaningful from a directory where the glob HAS
# matches — run from the repo root, where nothing matches `*.yaml`, an
# unexpanded glob and a broken one look identical and the test passes
# vacuously. So: create matches, run from there, and assert the count the
# glob would have produced had it expanded (5 args, not 6).
( cd "$TMP" && touch one.yaml two.yaml )
matches=$(cd "$TMP" && ls *.yaml 2>/dev/null | wc -l)
if [ "$matches" -lt 2 ]; then
  fail "glob fixture is real" "expected >=2 matching files in the cwd, found ${matches}"
fi
cd "$TMP"
invoke "s.yaml" "*.yaml"
cd - >/dev/null
if [ "$ARGC" -eq 5 ] && [ "${ARGV[4]-}" = "*.yaml" ]; then
  pass "glob in extra-args is not expanded (${matches} files would have matched)"
else
  fail "glob in extra-args is not expanded" "argc=${ARGC}: $(show)"
fi

echo
echo "== extra-args, and only extra-args, splits =="
invoke "s.yaml" "--interval 5s --query-step 10s"
if [ "$ARGC" -eq 8 ]; then
  pass "four extra tokens become four arguments"
else
  fail "four extra tokens become four arguments" "argc=${ARGC}: $(show)"
fi

invoke "s.yaml" ""
if [ "$ARGC" -eq 4 ]; then
  pass "empty extra-args adds nothing"
else
  fail "empty extra-args adds nothing" "argc=${ARGC}"
fi

echo
echo "== the one-of rule =="
neither="$(SONDA_SCENARIO=s.yaml SONDA_PROM_URL="" SONDA_AM_URL="" SONDA_EXTRA_ARGS="" \
  bash -c "$(declare -f run_action_step); run_action_step" 2>&1 >/dev/null; echo "rc=$?")"
case "$neither" in
  *"one of prometheus-url or alertmanager-url is required"*rc=1*) pass "neither URL is rejected" ;;
  *) fail "neither URL is rejected" "$neither" ;;
esac

both="$(SONDA_SCENARIO=s.yaml SONDA_PROM_URL=http://p SONDA_AM_URL=http://a SONDA_EXTRA_ARGS="" \
  bash -c "$(declare -f run_action_step); run_action_step" 2>&1 >/dev/null; echo "rc=$?")"
case "$both" in
  *"mutually exclusive"*rc=1*) pass "both URLs are rejected" ;;
  *) fail "both URLs are rejected" "$both" ;;
esac

invoke "s.yaml" "" "" "http://am:9093"
if [ "${ARGV[2]-}" = "--alertmanager-url" ] && [ "${ARGV[3]-}" = "http://am:9093" ]; then
  pass "alertmanager-url selects the alertmanager flag"
else
  fail "alertmanager-url selects the alertmanager flag" "$(show)"
fi

echo
echo "== the harness still mirrors action.yml =="
# A copy of production code in a test rots silently. Pin the pieces that
# carry the safety property; if action.yml stops containing them, this
# harness is testing something the action no longer does.
ACTION="$(dirname "$0")/../../action.yml"
missing=()
for needle in \
  'args=(test "$SONDA_SCENARIO")' \
  'args+=(--prometheus-url "$SONDA_PROM_URL")' \
  'args+=(--alertmanager-url "$SONDA_AM_URL")' \
  'extra=($SONDA_EXTRA_ARGS)' \
  'sonda "${args[@]}"' \
  'set -f'
do
  grep -qF -- "$needle" "$ACTION" || missing+=("$needle")
done
if [ ${#missing[@]} -eq 0 ]; then
  pass "every construction under test is present in action.yml"
else
  fail "every construction under test is present in action.yml" "absent: ${missing[*]}"
fi
# And the inverse: the action must not interpolate inputs into a run: body.
if grep -nE '\$\{\{ *inputs\.' "$ACTION" | grep -qv ': *\${{'; then
  fail "no input is interpolated into a run: body" "$(grep -nE '\$\{\{ *inputs\.' "$ACTION")"
else
  pass "inputs reach bash only through env:"
fi

echo
echo "== the major-version tag moves only where it should =="
# release.yml force-moves `vN` onto each release so `uses: …@v1` tracks the
# newest 1.x. A major tag that moves when it should not is worse than one
# that never moves — it silently reassigns every consumer pinned to it — so
# the guard gets a table.
major_for() {
  # Verbatim derivation from release.yml's "Move the major-version tag".
  local RELEASE_TAG="$1"
  if ! printf '%s' "$RELEASE_TAG" | grep -qE '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
    printf 'SKIP'
    return
  fi
  printf '%s' "${RELEASE_TAG%%.*}"
}

check_major() {
  local tag="$1" want="$2" got
  got="$(major_for "$tag")"
  if [ "$got" = "$want" ]; then
    pass "${tag} -> ${want}"
  else
    fail "${tag} -> ${want}" "got ${got}"
  fi
}

check_major "v1.20.0"    "v1"
check_major "v1.0.0"     "v1"
check_major "v2.0.0"     "v2"      # a 2.0 release must NOT touch v1
check_major "v10.3.1"    "v10"
# Anything that is not a plain release tag leaves major tags alone.
check_major "v2.0.0-rc.1" "SKIP"
check_major "v1.20"       "SKIP"
check_major "nightly"     "SKIP"
check_major "v1"          "SKIP"   # the major tag itself must not recurse

RELEASE_YML="$(dirname "$0")/../../.github/workflows/release.yml"
release_missing=()
for needle in \
  '^v[0-9]+\.[0-9]+\.[0-9]+$' \
  'major="${RELEASE_TAG%%.*}"' \
  'git push --force origin "refs/tags/${major}"'
do
  grep -qF -- "$needle" "$RELEASE_YML" || release_missing+=("$needle")
done
if [ ${#release_missing[@]} -eq 0 ]; then
  pass "the derivation under test is present in release.yml"
else
  fail "the derivation under test is present in release.yml" "absent: ${release_missing[*]}"
fi
# The move must happen after the release exists, or @v1 can point at a tag
# whose assets have not uploaded yet.
if awk '/name: Create release/{r=NR} /name: Move the major-version tag/{m=NR} END{exit !(r && m && m > r)}' "$RELEASE_YML"; then
  pass "the major tag moves after the release is created"
else
  fail "the major tag moves after the release is created" "ordering in release.yml is wrong or a step was renamed"
fi

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "action argv: all checks passed"
  exit 0
fi
echo "action argv: ${FAILURES} check(s) failed"
exit 1
