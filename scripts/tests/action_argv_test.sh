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
# Any invocation at all is recorded, so a test can assert the step ran
# nothing — which is how a command substitution in a message is caught
# regardless of how it was spelled.
[ -n "${SONDA_INVOKE_LOG:-}" ] && printf 'INVOKED: %s\n' "$*" >> "$SONDA_INVOKE_LOG"
for a in "$@"; do printf '%s\0' "$a"; done > "$SONDA_ARGV_FILE"
FAKE
chmod +x "$TMP/sonda"
PATH="$TMP:$PATH"

# THE REAL STEP, NOT A COPY.
#
# Every previous round of this PR found a defect the harness could not see
# because it drove a copy of action.yml's shell: the copy diverged on
# exactly the line a bug was on, twice, and a backtick that executed a
# command sat in production while the copy carried a clean message. So the
# copy is gone. `block:3` extracts the "Verify alert expectations" body
# from action.yml itself and every check below runs THAT.
#
# This also retires a whole class of finding: there is no longer a second
# copy that can drift, so the drift needles guard the remaining blocks
# (resolve, install) rather than the one under test.
ACTION_FILE="$(cd "$(dirname "$0")/../.." && pwd)/action.yml"
VERIFY_STEP="$TMP/verify_step.sh"
if ! python3 "$(dirname "$0")/extract_run_bodies.py" "$ACTION_FILE" block:3 > "$VERIFY_STEP"; then
  echo "could not extract the verify step from action.yml" >&2
  exit 1
fi
if ! grep -q 'sonda "${args\[@\]}"' "$VERIFY_STEP"; then
  echo "extracted block 3 does not look like the verify step — refusing to test the wrong thing" >&2
  exit 1
fi

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
  bash "$VERIFY_STEP" 2>/dev/null
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
# Every bail path is checked for BOTH things: that it refuses, and that
# refusing executes nothing. The step has three message paths, and the
# substitution bug has now shipped twice — as a backtick in round 2 and as
# $(…) in round 3 — both times inside an ::error:: string in this step.
# Guarding only the path where it happened would be guarding the instance
# instead of the class (#554 round 4, W1).
check_bail() {
  local label="$1" want="$2"; shift 2
  local out
  : > "$TMP/invoked.log"
  out="$(env "$@" SONDA_SCENARIO=s.yaml SONDA_EXTRA_ARGS="" \
    SONDA_INVOKE_LOG="$TMP/invoked.log" \
    bash "$VERIFY_STEP" 2>&1 >/dev/null; echo "rc=$?")"
  case "$out" in
    *"$want"*rc=1*) pass "$label" ;;
    *) fail "$label" "$out" ;;
  esac
  if [ -s "$TMP/invoked.log" ]; then
    fail "$label executes nothing" "the step invoked sonda while refusing: $(cat "$TMP/invoked.log")"
  else
    pass "$label executes nothing"
  fi
}

check_bail "neither URL is rejected" "one of prometheus-url or alertmanager-url is required" \
  SONDA_PROM_URL="" SONDA_AM_URL=""
check_bail "both URLs are rejected" "mutually exclusive" \
  SONDA_PROM_URL=http://p SONDA_AM_URL=http://a

invoke "s.yaml" "" "" "http://am:9093"
if [ "${ARGV[2]-}" = "--alertmanager-url" ] && [ "${ARGV[3]-}" = "http://am:9093" ]; then
  pass "alertmanager-url selects the alertmanager flag"
else
  fail "alertmanager-url selects the alertmanager flag" "$(show)"
fi

dry="$(SONDA_SCENARIO=s.yaml SONDA_PROM_URL=http://p SONDA_AM_URL="" SONDA_EXTRA_ARGS="--interval 5s --dry-run" \
  bash "$VERIFY_STEP" 2>&1 >/dev/null; echo "rc=$?")"
case "$dry" in
  *"without verifying"*rc=1*) pass "--dry-run in extra-args is refused" ;;
  *) fail "--dry-run in extra-args is refused" "$dry" ;;
esac

invoke "s.yaml" "--interval 5s"
if [ "$ARGC" -eq 6 ]; then
  pass "a normal extra-arg is still accepted"
else
  fail "a normal extra-arg is still accepted" "argc=${ARGC}: $(show)"
fi

echo
echo "== the harness still mirrors action.yml =="
# A copy of production code in a test rots silently, so the copy is only
# defensible if these checks pin it to production. Round 1 of #554 broke
# safety in action.yml TWICE with every check green, both times because the
# checks read the whole file as flat text:
#
#   - the `set -f` needle was satisfied by a COMMENT mentioning it, so
#     deleting the real line changed nothing here while globs expanded;
#   - the "no interpolation" check allowed any `:` before the expression,
#     so `echo "scenario:<expr>"` inside a run: body passed — which is
#     arbitrary command execution from a workflow input.
#
# Both are fixed by looking at the right text: the bodies of `run:` blocks,
# with comment lines removed, rather than the whole file.
run_bodies() {
  python3 "$(dirname "$0")/extract_run_bodies.py" "$1" "${2:-code}"
}

ACTION="$(dirname "$0")/../../action.yml"
ACTION_CODE="$(run_bodies "$ACTION" code)"
ACTION_RAW="$(run_bodies "$ACTION" raw)"

# The extractor is load-bearing for every check below it, so it gets its
# own checks — and they must test COMPLETENESS, not presence. Round 2
# showed why: the extractor recognised only `run: |` with exactly one
# space, so a single-line `run:` or a re-indented `run:  |` vanished, every
# check below inherited the blindness, and a non-empty result kept the
# guard green. "Some" is not "all".
counts="$(python3 "$(dirname "$0")/extract_run_bodies.py" "$ACTION" count)"
keys_in_file="${counts%% *}"
keys_opened="${counts##* }"
if [ -n "$ACTION_CODE" ]; then
  pass "run: bodies extracted from action.yml ($(printf '%s\n' "$ACTION_CODE" | grep -c .) code lines)"
else
  fail "run: bodies extracted from action.yml" "extractor produced nothing — every check below would pass vacuously"
fi
if [ "$keys_in_file" = "$keys_opened" ] && [ "$keys_in_file" -gt 0 ]; then
  pass "every run: key in action.yml was opened by the extractor (${keys_opened}/${keys_in_file})"
else
  fail "every run: key in action.yml was opened by the extractor" "file has ${keys_in_file} run: keys, extractor opened ${keys_opened} — the difference is invisible to every check below"
fi

missing=()
for needle in \
  'args=(test "$SONDA_SCENARIO")' \
  'args+=(--prometheus-url "$SONDA_PROM_URL")' \
  'args+=(--alertmanager-url "$SONDA_AM_URL")' \
  'extra=($SONDA_EXTRA_ARGS)' \
  'sonda "${args[@]}"' \
  'set -f' \
  'set +f' \
  'if [ "$arg" = "--dry-run" ]; then'
do
  printf '%s\n' "$ACTION_CODE" | grep -qF -- "$needle" || missing+=("$needle")
done
if [ ${#missing[@]} -eq 0 ]; then
  pass "every construction under test is real code in action.yml, not a comment"
else
  fail "every construction under test is real code in action.yml, not a comment" "absent: ${missing[*]}"
fi

# The inverse, and the one that matters most: NOTHING may be interpolated
# into a run: body — not an input, not any other expression. GitHub
# substitutes them textually before bash parses the script, so the value
# becomes program text. Checked against the raw bodies (comments included,
# since those are substituted too) with no allow-list to have a hole in.
if printf '%s\n' "$ACTION_RAW" | grep -q '\${{'; then
  fail "no expression is interpolated into a run: body" "$(printf '%s\n' "$ACTION_RAW" | grep -n '\${{' | head -5)"
else
  pass "no expression is interpolated into a run: body"
fi

# W1, the behavioural half: refusing --dry-run must not RUN anything.
#
# Round 2 shipped a backtick in the refusal message, which executed
# `sonda test --dry-run` while claiming to refuse it. Round 3 rewrote the
# same bug as $(…) and the backtick check missed it — and banning $(…)
# syntactically is not available, since the run bodies use it legitimately
# three times. A syntactic guard would have to become an allow-list, which
# is the shape round 1 deleted.
#
# So assert the behaviour instead: drive the real step and require that the
# fake binary was never invoked. That covers every SPELLING, present and
# future, because it tests what the shell did rather than how it was
# written — but only on the paths actually driven, which is why the two
# bail paths below carry the same assertion rather than trusting this one
# to speak for the file.
: > "$TMP/invoked.log"
SONDA_SCENARIO=s.yaml SONDA_PROM_URL=http://p SONDA_AM_URL="" \
  SONDA_EXTRA_ARGS="--dry-run" SONDA_INVOKE_LOG="$TMP/invoked.log" \
  bash "$VERIFY_STEP" >/dev/null 2>&1
if [ -s "$TMP/invoked.log" ]; then
  fail "refusing --dry-run executes nothing" "the step invoked sonda while refusing: $(cat "$TMP/invoked.log")"
else
  pass "refusing --dry-run executes nothing"
fi

# …and the refusal must still say something useful. A substitution that
# runs also swallows its own output, deleting the actionable half of the
# message — which is how round 2's bug hid in plain sight.
dry_msg="$(SONDA_SCENARIO=s.yaml SONDA_PROM_URL=http://p SONDA_AM_URL="" \
  SONDA_EXTRA_ARGS="--dry-run" bash "$VERIFY_STEP" 2>&1 >/dev/null)"
case "$dry_msg" in
  *"sonda test --dry-run locally"*) pass "the refusal message survives intact" ;;
  *) fail "the refusal message survives intact" "message was: ${dry_msg}" ;;
esac

# No unescaped backtick may survive in a run: body. Round 2 found one in
# the --dry-run refusal message, where it ran `sonda test --dry-run` while
# claiming to refuse it AND deleted the actionable half of the message.
# A general guard rather than a needle for that one line: the class is
# "shell metacharacter evaluated in the file whose thesis is that nothing
# is evaluated by the shell".
if printf '%s\n' "$ACTION_CODE" | grep -q '`'; then
  fail "no unescaped backtick in a run: body" "$(printf '%s\n' "$ACTION_CODE" | grep -n '`' | head -3)"
else
  pass "no unescaped backtick in a run: body"
fi

# …and the inputs must therefore arrive through env:, which is where the
# run: bodies read them from.
env_bound=0
for binding in \
  'SONDA_SCENARIO: ${{ inputs.scenario }}' \
  'SONDA_PROM_URL: ${{ inputs.prometheus-url }}' \
  'SONDA_AM_URL: ${{ inputs.alertmanager-url }}' \
  'SONDA_EXTRA_ARGS: ${{ inputs.extra-args }}'
do
  grep -qF -- "$binding" "$ACTION" && env_bound=$((env_bound + 1))
done
if [ "$env_bound" -eq 4 ]; then
  pass "all four inputs reach bash through env: bindings"
else
  fail "all four inputs reach bash through env: bindings" "found ${env_bound}/4"
fi

echo
echo "== the moving tags move only where they should =="
# `latest`, `vN`/`N` and `N.M` all follow one rule — a moving tag may point
# at a release only if that release is the highest among the releases the
# tag denotes — and one implementation, scripts/moving_tags.sh. A moving tag
# that moves when it should not silently reassigns every consumer pinned to
# it, so it gets a table, run against a REAL git repo: `sort -V` and
# `git tag -l` globbing are exactly the parts a reimplementation here would
# get right while production got them wrong.
MOVING_TAGS="$(cd "$(dirname "$0")/../.." && pwd)/scripts/moving_tags.sh"
IMAGE_TAGS="$(cd "$(dirname "$0")/../.." && pwd)/scripts/image_tags.sh"
RELEASE_YML="$(dirname "$0")/../../.github/workflows/release.yml"

scratch_repo() {
  local repo="$1"; shift
  rm -rf "$repo"; mkdir -p "$repo"
  git -C "$repo" init -q .
  git -C "$repo" config user.email t@t
  git -C "$repo" config user.name t
  git -C "$repo" commit -q --allow-empty -m x
  local t
  for t in "$@"; do git -C "$repo" tag "$t"; done
}

decide() {
  local release_tag="$1"; shift
  local repo="$TMP/tagrepo"
  scratch_repo "$repo" "$@"
  (cd "$repo" && bash "$MOVING_TAGS" "$release_tag" 2> /dev/null | tr '\n' ' ' | sed 's/ $//')
}

check_moving() {
  local label="$1" want="$2" got; shift 2
  got="$(decide "$@")"
  if [ "$got" = "$want" ]; then pass "$label"; else fail "$label" "got '${got}', want '${want}'"; fi
}

all_true="MOVE_LATEST=true MOVE_MAJOR=true MOVE_MINOR=true"
all_false="MOVE_LATEST=false MOVE_MAJOR=false MOVE_MINOR=false"

check_moving "the newest release moves everything" \
  "$all_true" "v2.0.0" v1.21.0 v1.22.3 v2.0.0
check_moving "newest of the v1 line moves 1 and 1.22, not latest" \
  "MOVE_LATEST=false MOVE_MAJOR=true MOVE_MINOR=true" "v1.22.3" v1.21.0 v1.22.3 v2.0.0
check_moving "an older minor moves only its own minor tag" \
  "MOVE_LATEST=false MOVE_MAJOR=false MOVE_MINOR=true" "v1.21.0" v1.21.0 v1.22.3 v2.0.0
check_moving "a backport does NOT move its minor tag back" \
  "MOVE_LATEST=false MOVE_MAJOR=false MOVE_MINOR=false" "v1.21.0" v1.21.0 v1.21.4 v1.22.3
check_moving "v1.9.0 vs v1.20.0 is compared numerically" \
  "MOVE_LATEST=false MOVE_MAJOR=false MOVE_MINOR=true" "v1.9.0" v1.9.0 v1.20.0
check_moving "the v10 line is not part of the v1 line" \
  "MOVE_LATEST=false MOVE_MAJOR=true MOVE_MINOR=true" "v1.20.0" v1.20.0 v10.3.1
check_moving "the first release of all moves everything" \
  "$all_true" "v1.0.0" v1.0.0
# Anything that is not a plain release tag publishes the exact version only.
check_moving "a pre-release moves nothing"   "$all_false" "v2.0.0-rc.1" v1.20.0 v2.0.0-rc.1
check_moving "a two-part tag moves nothing"  "$all_false" "v1.20"       v1.20
check_moving "a named tag moves nothing"     "$all_false" "nightly"     v1.20.0 nightly
check_moving "the major tag cannot recurse"  "$all_false" "v1"          v1 v1.20.0

# An empty or unanswerable tag list reads as "nothing is newer", which would
# open every moving tag at once. Each way of producing one must refuse.
refuses() {
  local label="$1" out rc; shift
  out="$("$@" 2>&1)"; rc=$?
  if [ "$rc" -ne 0 ] && ! printf '%s' "$out" | grep -q 'true'; then
    pass "$label"
  else
    fail "$label" "rc=${rc}, output: ${out}"
  fi
}

refuses "no argument is refused rather than answered" \
  bash "$MOVING_TAGS"
mkdir -p "$TMP/notarepo"
refuses "a missing git repository is refused rather than answered" \
  env -C "$TMP/notarepo" GIT_CEILING_DIRECTORIES="$TMP" bash "$MOVING_TAGS" v1.0.0
# A checkout whose tags were never fetched answers every question with
# "nothing is newer" — the same vacuous `true` a missing repository gives,
# from a repository that looks perfectly healthy.
scratch_repo "$TMP/notags"
refuses "a repository with no tags at all is refused" \
  env -C "$TMP/notags" bash "$MOVING_TAGS" v1.0.0
scratch_repo "$TMP/othertags" v1.0.0 v1.1.0
refuses "a release absent from the fetched tags is refused" \
  env -C "$TMP/othertags" bash "$MOVING_TAGS" v9.9.9

echo
echo "== the image tags this repository publishes =="
# The tag list is computed here rather than read off docker/metadata-action:
# its `flavor` defaults to `latest=auto`, under which the semver processor
# appends `:latest` from the first tag entry, so `enable=` on a
# `type=raw,value=latest` entry suppresses a duplicate and leaves the real
# one. Owning the list is the fix; this table is what makes owning it worth
# something.
image_tags_for() {
  local release_tag="$1"; shift
  local repo="$TMP/imagerepo"
  scratch_repo "$repo" "$@"
  (cd "$repo" && bash "$IMAGE_TAGS" "$release_tag" ghcr.io/davidban77/sonda 2> /dev/null \
    | sed 's#^ghcr.io/davidban77/sonda:##' | tr '\n' ' ' | sed 's/ $//')
}

check_image_tags() {
  local label="$1" want="$2" got; shift 2
  got="$(image_tags_for "$@")"
  if [ "$got" = "$want" ]; then pass "$label"; else fail "$label" "got '${got}', want '${want}'"; fi
}

check_image_tags "the newest release claims every moving tag" \
  "2.0.0 2.0 2 latest" "v2.0.0" v1.22.3 v2.0.0
check_image_tags "newest of the v1 line claims 1 and 1.22, never latest" \
  "1.22.3 1.22 1" "v1.22.3" v1.21.0 v1.22.3 v2.0.0
check_image_tags "an older minor claims only its own minor tag" \
  "1.21.0 1.21" "v1.21.0" v1.21.0 v1.22.3 v2.0.0
# The row the whole file exists for: a backport publishes its exact version
# and nothing else. Under metadata-action's default flavor this one produced
# `latest` as well, and every `docker pull sonda` got a downgrade.
check_image_tags "a backport publishes the exact version and nothing else" \
  "1.21.0" "v1.21.0" v1.21.0 v1.21.4 v1.22.3
check_image_tags "the first release of all claims every moving tag" \
  "1.0.0 1.0 1 latest" "v1.0.0" v1.0.0
check_image_tags "a pre-release publishes only itself" \
  "2.0.0-rc.1" "v2.0.0-rc.1" v1.22.3 v2.0.0-rc.1

refuses "a release absent from the fetched tags publishes no image tag" \
  env -C "$TMP/othertags" bash "$IMAGE_TAGS" v9.9.9 ghcr.io/davidban77/sonda
# A non-zero exit alone is satisfied by the wrong refusal: against a repository
# whose tags lack the release, the moving tags are undecided and the reference
# is never inspected. So: a repository carrying the release, and the message.
refuses_saying() {
  local label="$1" want="$2" out rc; shift 2
  out="$("$@" 2>&1)"; rc=$?
  if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -qF -- "$want" \
    && ! printf '%s' "$out" | grep -q 'true'; then
    pass "$label"
  else
    fail "$label" "rc=${rc}, output: ${out}"
  fi
}
scratch_repo "$TMP/refrepo" v1.0.0
refuses_saying "a repository that already carries a tag is refused" "already carries a tag" \
  env -C "$TMP/refrepo" bash "$IMAGE_TAGS" v1.0.0 ghcr.io/davidban77/sonda:latest
refuses_saying "a repository that already carries a digest is refused" "already carries a digest" \
  env -C "$TMP/refrepo" bash "$IMAGE_TAGS" v1.0.0 ghcr.io/davidban77/sonda@sha256:abc
# A `:` before the last `/` is a registry port. Rejecting it would make the
# script unusable against any registry that has one, which is every local
# one — and the red-verification for the tagging sequence runs on exactly
# that.
scratch_repo "$TMP/portrepo" v1.0.0
got_port="$(cd "$TMP/portrepo" && bash "$IMAGE_TAGS" v1.0.0 localhost:5000/sonda 2> /dev/null | tr '\n' ' ' | sed 's/ $//')"
if [ "$got_port" = "localhost:5000/sonda:1.0.0 localhost:5000/sonda:1.0 localhost:5000/sonda:1 localhost:5000/sonda:latest" ]; then
  pass "a registry port is not mistaken for a tag"
else
  fail "a registry port is not mistaken for a tag" "got '${got_port}'"
fi

# Renaming a key on one side of this pair leaves every moving tag silently
# unclaimed, and a moving tag that stops moving is invisible until someone
# pulls `latest` and gets an old release.
decision_keys="$(grep -oE 'MOVE_[A-Z]+=' "$MOVING_TAGS" | tr -d '=' | sort -u | tr '\n' ' ')"
consumer_keys="$(grep -oE 'MOVE_[A-Z]+=true' "$IMAGE_TAGS" | sed 's/=true//' | sort -u | tr '\n' ' ')"
if [ "$decision_keys" = "$consumer_keys" ] && [ -n "$decision_keys" ]; then
  pass "image_tags.sh gates on exactly the keys moving_tags.sh prints (${decision_keys% })"
else
  fail "image_tags.sh gates on exactly the keys moving_tags.sh prints" \
    "moving_tags.sh prints '${decision_keys}', image_tags.sh reads '${consumer_keys}'"
fi

echo
echo "== a tag is created only from a verified digest =="
# THE REAL STEP, NOT A COPY. The release job pushes by digest and names the
# manifest last, so a failed verification leaves nothing resolvable. The
# table above says which tags are correct; this says the step publishes
# those and only those, driven with a fake `docker` that records its argv.
cat > "$TMP/docker" <<'FAKE'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$DOCKER_LOG"
# `docker run … --version` is read by the smoke step; everything else here
# is only recorded.
[ "${1-}" = "run" ] && printf '%s\n' "${DOCKER_FAKE_VERSION-}"
exit 0
FAKE
chmod +x "$TMP/docker"

TAG_STEP="$TMP/tag_step.sh"
: > "$TAG_STEP"
for n in $(seq 1 30); do
  body="$(python3 "$(dirname "$0")/extract_run_bodies.py" "$RELEASE_YML" "block:$n" 2> /dev/null)" || continue
  case "$body" in
    *'imagetools create'*) printf '%s\n' "$body" > "$TAG_STEP"; break ;;
  esac
done
if [ ! -s "$TAG_STEP" ]; then
  echo "could not find the image-tagging step in release.yml — refusing to test the wrong thing" >&2
  exit 1
fi

# The exit code is part of the answer: a step that creates no tag AND reports
# success leaves the release looking published with nothing behind the name.
tagged() {
  local release_tag="$1" rc log; shift
  local repo="$TMP/tagsteprepo"
  scratch_repo "$repo" "$@"
  mkdir -p "$repo/scripts"
  cp "$MOVING_TAGS" "$IMAGE_TAGS" "$repo/scripts/"
  : > "$TMP/docker.log"
  (
    cd "$repo"
    RELEASE_TAG="$release_tag" IMAGE=ghcr.io/davidban77/sonda DIGEST=sha256:d1 \
      DOCKER_LOG="$TMP/docker.log" bash "$TAG_STEP" > /dev/null 2>&1
  )
  rc=$?
  log="$(cat "$TMP/docker.log")"
  printf 'rc=%s%s' "$rc" "${log:+ $log}"
}

check_tagged() {
  local label="$1" want="$2" got; shift 2
  got="$(tagged "$@")"
  if [ "$got" = "$want" ]; then pass "$label"; else fail "$label" "got '${got}', want '${want}'"; fi
}

check_tagged "the step tags the digest with every claimed tag" \
  "rc=0 buildx imagetools create --tag ghcr.io/davidban77/sonda:2.0.0 --tag ghcr.io/davidban77/sonda:2.0 --tag ghcr.io/davidban77/sonda:2 --tag ghcr.io/davidban77/sonda:latest ghcr.io/davidban77/sonda@sha256:d1" \
  "v2.0.0" v1.22.3 v2.0.0
check_tagged "a backport reaches the registry as its exact version only" \
  "rc=0 buildx imagetools create --tag ghcr.io/davidban77/sonda:1.21.0 ghcr.io/davidban77/sonda@sha256:d1" \
  "v1.21.0" v1.21.0 v1.21.4 v1.22.3
# An undecidable tag list must publish nothing rather than everything: the
# step is the last one in the job, so anything it creates is what the world
# sees.
check_tagged "an undecidable release fails the step and creates no tag" "rc=1" "v9.9.9" v1.0.0

# `latest` is the tag everyone gets by default, so publishing it for a release
# that is not the highest downgrades every `docker pull sonda` in the world.
# Asked of the argv the step hands `imagetools` rather than of what a script
# prints: the original defect computed the right answer and published `:latest`
# anyway, from a tag list the step read somewhere else.
claims_latest() {
  case " $(tagged "$@") " in
    *" --tag ghcr.io/davidban77/sonda:latest "*) printf 'LATEST' ;;
    *) printf 'NO-LATEST' ;;
  esac
}

check_latest() {
  local label="$1" want="$2" got; shift 2
  got="$(claims_latest "$@")"
  if [ "$got" = "$want" ]; then pass "$label"; else fail "$label" "got ${got}, want ${want}"; fi
}

check_latest "the highest release publishes latest"          LATEST    "v2.0.0"      v1.22.3 v2.0.0
check_latest "the first release of all publishes latest"     LATEST    "v1.0.0"      v1.0.0
check_latest "the newest of an older major line does not"    NO-LATEST "v1.22.3"     v1.21.0 v1.22.3 v2.0.0
check_latest "an older minor does not"                       NO-LATEST "v1.21.0"     v1.21.0 v1.22.3 v2.0.0
check_latest "a backport cut after a newer release does not" NO-LATEST "v1.19.1"     v1.19.0 v1.19.1 v1.20.0
check_latest "a pre-release does not"                        NO-LATEST "v2.0.0-rc.1" v1.22.3 v2.0.0-rc.1
# No undecidable row here: NO-LATEST cannot tell a refusal from a broken step, and the `rc=1` row above pins that case exactly.

# The sequencing D3 exists for. `push-by-digest` is what makes the manifest
# unreachable by name; the ordering is what keeps it that way until the
# comparison and the smoke tests have run. Either one alone is decorative.
#
# Every check below reads release.yml with comment lines dropped, for the
# reason round 1 learned the hard way: the prose next to a rule quotes the
# thing the rule is about, so a needle against the raw file is answered by the
# comment while the rule itself is deleted.
release_yaml_code="$(grep -vE '^[[:space:]]*#' "$RELEASE_YML")"
# Anchored inside the `outputs:` value, not anywhere in the file: an exporter
# that loses this key publishes `:latest` before anything has verified it.
build_outputs="$(printf '%s\n' "$release_yaml_code" | grep -E '^[[:space:]]*outputs:' | sed 's/^[^:]*://')"
case "$build_outputs" in
  *push-by-digest=true*)
    pass "the image is pushed by digest, not under a tag" ;;
  *)
    fail "the image is pushed by digest, not under a tag" \
      "the build step's outputs: is '${build_outputs}'" ;;
esac
if printf '%s\n' "$release_yaml_code" | awk '
  /name: Build and push the image by digest/{b=NR}
  /name: Verify the image ships the released binaries/{v=NR}
  /name: Smoke test the image/{s=NR}
  /name: Tag the verified image/{t=NR}
  END{exit !(b && v && s && t && b < v && v < t && s < t)}
'; then
  pass "the tags are created after the comparison and the smoke tests"
else
  fail "the tags are created after the comparison and the smoke tests" \
    "ordering in release.yml is wrong or a step was renamed"
fi
# metadata-action stays for labels only. The moment its tag output is read,
# `latest=auto` decides what ships again and the table above stops meaning
# anything — so the ban is on reading it, not on configuring it.
if printf '%s\n' "$release_yaml_code" | grep -q 'meta\.outputs\.tags'; then
  fail "metadata-action's tag list never decides what is published" \
    "$(printf '%s\n' "$release_yaml_code" | grep -n 'meta\.outputs\.tags')"
else
  pass "metadata-action's tag list never decides what is published"
fi
# THE REAL STEP, NOT A COPY, again. `sonda-server` is the image's ENTRYPOINT
# and was the one binary in it with no execution check at all, so "both
# binaries on both platforms" is asserted from what the step actually ran.
SMOKE_STEP="$TMP/smoke_step.sh"
: > "$SMOKE_STEP"
for n in $(seq 1 30); do
  body="$(python3 "$(dirname "$0")/extract_run_bodies.py" "$RELEASE_YML" "block:$n" 2> /dev/null)" || continue
  case "$body" in
    *'--entrypoint'*) printf '%s\n' "$body" > "$SMOKE_STEP"; break ;;
  esac
done
if [ ! -s "$SMOKE_STEP" ]; then
  echo "could not find the smoke-test step in release.yml — refusing to test the wrong thing" >&2
  exit 1
fi

smoked() {
  : > "$TMP/docker.log"
  RELEASE_TAG=v9.9.9 IMAGE=ghcr.io/davidban77/sonda@sha256:d1 \
    DOCKER_LOG="$TMP/docker.log" DOCKER_FAKE_VERSION="${1}" \
    bash "$SMOKE_STEP" > /dev/null 2>&1
  printf 'rc=%s ' "$?"
  grep -oE '\-\-platform linux/[a-z0-9]+ --entrypoint /[a-z-]+' "$TMP/docker.log" \
    | sort -u | tr '\n' ',' | sed 's/,$//'
}

want_smoke="rc=0 --platform linux/amd64 --entrypoint /sonda,--platform linux/amd64 --entrypoint /sonda-server,--platform linux/arm64 --entrypoint /sonda,--platform linux/arm64 --entrypoint /sonda-server"
got_smoke="$(smoked "sonda 9.9.9")"
if [ "$got_smoke" = "$want_smoke" ]; then
  pass "the smoke test executes both binaries on both platforms"
else
  fail "the smoke test executes both binaries on both platforms" "got '${got_smoke}'"
fi
# …and the version assertion is not decorative: a binary that starts but is
# not the one this tag released must fail the step.
case "$(smoked "sonda 1.0.0")" in
  rc=0*) fail "a wrong version fails the smoke test" "the step accepted 'sonda 1.0.0' while building v9.9.9" ;;
  *)     pass "a wrong version fails the smoke test" ;;
esac

echo
echo "== the major git tag moves only where it should =="
# THE REAL STEP, NOT A COPY, in a scratch repo with a bare origin where the
# answer becomes an actual moved tag. A reader that exits on its first match
# once left the script dead of SIGPIPE and every "move" reading as "leave" —
# a defect entirely invisible to a needle.
MAJOR_STEP="$TMP/major_step.sh"
: > "$MAJOR_STEP"
for n in $(seq 1 30); do
  body="$(python3 "$(dirname "$0")/extract_run_bodies.py" "$RELEASE_YML" "block:$n" 2> /dev/null)" || continue
  case "$body" in
    *'git push --force origin'*) printf '%s\n' "$body" > "$MAJOR_STEP"; break ;;
  esac
done
if [ ! -s "$MAJOR_STEP" ]; then
  echo "could not find the major-tag step in release.yml — refusing to test the wrong thing" >&2
  exit 1
fi

moved_major() {
  local release_tag="$1"; shift
  local repo="$TMP/steprepo" major="${release_tag%%.*}" rc head older local_ref remote_ref moved published
  local bystander=v1 by_local by_remote intact
  if [ "$major" = v1 ]; then bystander=v2; fi
  rm -rf "$repo" "$repo.git"; mkdir -p "$repo"
  (
    git init -q --bare "${repo}.git"
    # Guarded: unguarded, a failed mkdir leaves this subshell in the real
    # checkout, where the lines below push tags to the real origin.
    cd "$repo" || { printf 'FIXTURE-BROKEN'; exit 1; }
    git init -q .; git config user.email t@t; git config user.name t
    git remote add origin "${repo}.git"
    mkdir -p scripts; cp "$MOVING_TAGS" scripts/
    git commit -q --allow-empty -m older
    older="$(git rev-parse HEAD)"
    git tag "$major" # the moving tag, already published, on an older commit
    git tag "$bystander" # another major line's moving tag, which this release never claims
    # Published for real, so the step must overwrite the remote ref rather than create one.
    git push -q origin "refs/tags/${major}" "refs/tags/${bystander}"
    for t in "$major" "$bystander"; do
      git -C "${repo}.git" rev-parse -q --verify "refs/tags/${t}" > /dev/null \
        || { printf 'FIXTURE-BROKEN'; exit 1; }
    done
    git commit -q --allow-empty -m release
    for t in "$@"; do git tag "$t"; done
    head="$(git rev-parse HEAD)" # fixed before the step runs, so a step that commits cannot redefine it
    RELEASE_TAG="$release_tag" bash "$MAJOR_STEP" > /dev/null 2>&1
    rc=$?
    local_ref="$(git rev-parse -q --verify "refs/tags/${major}^{commit}" 2> /dev/null || printf 'none')"
    # The local ref alone is not the property: `git tag -f` satisfies it while origin keeps @v1 on the previous release.
    remote_ref="$(git -C "${repo}.git" rev-parse -q --verify "refs/tags/${major}^{commit}" 2> /dev/null || printf 'none')"
    # STRAY is the tag deleted, never created, or moved somewhere else entirely — outcomes a plain `else LEFT` calls correct.
    if   [ "$local_ref" = "$head" ];  then moved=MOVED
    elif [ "$local_ref" = "$older" ]; then moved=LEFT
    else                                   moved=STRAY; fi
    if   [ "$remote_ref" = "$head" ];  then published=PUBLISHED
    elif [ "$remote_ref" = "$older" ]; then published=UNPUBLISHED
    else                                    published=STRAY; fi
    by_local="$(git rev-parse -q --verify "refs/tags/${bystander}^{commit}" 2> /dev/null || printf 'none')"
    by_remote="$(git -C "${repo}.git" rev-parse -q --verify "refs/tags/${bystander}^{commit}" 2> /dev/null || printf 'none')"
    if [ "$by_local" = "$older" ] && [ "$by_remote" = "$older" ]; then intact=INTACT; else intact=DISTURBED; fi
    # The exit code is part of the answer: a release the rule cannot decide
    # must leave the tag alone AND say so, not leave it alone quietly.
    printf 'rc=%s %s/%s %s:%s' "$rc" "$moved" "$published" "$bystander" "$intact"
  )
}

check_step() {
  local label="$1" want="$2" got; shift 2
  got="$(moved_major "$@")"
  if [ "$got" = "$want" ]; then pass "$label"; else fail "$label" "got ${got}, want ${want}"; fi
}

check_step "the step really moves v1 onto the newest 1.x" "rc=0 MOVED/PUBLISHED v2:INTACT"  "v1.22.3" v1.21.0 v1.22.3 v2.0.0
check_step "the step leaves v1 alone on an older 1.x"     "rc=0 LEFT/UNPUBLISHED v2:INTACT" "v1.21.0" v1.21.0 v1.22.3 v2.0.0
check_step "the step moves v2 and never touches v1"       "rc=0 MOVED/PUBLISHED v1:INTACT"  "v2.0.0"  v1.22.3 v2.0.0
# The major line is its own question, which is why the step reads MOVE_MAJOR
# and not the overall-highest answer: someone pinned to `@v1` wants the newest
# 1.x whether or not a 2.x exists.
check_step "a newer major line does not stop v1 tracking its own" \
  "rc=0 MOVED/PUBLISHED v2:INTACT" "v1.21.0" v1.21.0 v2.0.0
check_step "a backport does not drag v1 backwards" \
  "rc=0 LEFT/UNPUBLISHED v2:INTACT" "v1.19.1" v1.19.0 v1.19.1 v1.20.0
check_step "a pre-release moves nothing" \
  "rc=0 LEFT/UNPUBLISHED v1:INTACT" "v2.0.0-rc.1" v1.22.3 v2.0.0-rc.1
# A tag list that cannot see this release answers "nothing is newer" to every
# question, so the step must refuse rather than take the vacuous `true`.
check_step "a release absent from the fetched tags is refused, not answered" \
  "rc=1 LEFT/UNPUBLISHED v1:INTACT" "v9.9.9" v1.0.0

# Read release.yml the way action.yml is read. Round 1's fix — needles
# against run: bodies with comments dropped — went to action.yml only, so
# the ORIGINAL defect stayed reachable in the file that decides where @v1
# points: commenting the tag move out, quoting it verbatim, kept every
# check green while the workflow logged "moving v1" and moved nothing
# (#554 round 3, W2).
RELEASE_CODE="$(run_bodies "$RELEASE_YML" code)"
RELEASE_RAW="$(run_bodies "$RELEASE_YML" raw)"
rel_counts="$(python3 "$(dirname "$0")/extract_run_bodies.py" "$RELEASE_YML" count)"
if [ "${rel_counts%% *}" = "${rel_counts##* }" ] && [ "${rel_counts%% *}" -gt 0 ]; then
  pass "every run: key in release.yml was opened by the extractor (${rel_counts##* }/${rel_counts%% *})"
else
  fail "every run: key in release.yml was opened by the extractor" "counts: ${rel_counts}"
fi
# release.yml cannot hold action.yml's absolute rule: its build steps
# legitimately interpolate `matrix.target`, a value defined literally in
# the same file. So the assertion is an INVENTORY rather than a ban —
# every interpolation reaching a run: body is pinned by name, and anything
# new goes red until a human decides whether its value can be influenced
# from outside the repo. That fails closed on change without becoming an
# allow-list that permits by syntactic accident.
release_exprs="$(printf '%s\n' "$RELEASE_RAW" \
  | grep -o '\${{[^}]*}}' \
  | sed -e 's/\${{ *//' -e 's/ *}}//' \
  | sort -u | tr '\n' ' ' | sed 's/ $//')"
expected_exprs="matrix.target"
if [ "$release_exprs" = "$expected_exprs" ]; then
  pass "release.yml run: bodies interpolate only the pinned expressions (${release_exprs})"
else
  fail "release.yml run: bodies interpolate only the pinned expressions" "expected '${expected_exprs}', found '${release_exprs}' — a new interpolation must be reviewed for whether its value is controlled from outside the repo, then pinned here"
fi
release_missing=()
for needle in \
  'decisions="$(bash scripts/moving_tags.sh "$RELEASE_TAG")"' \
  '*MOVE_MAJOR=true*)' \
  'major="${RELEASE_TAG%%.*}"' \
  'git push --force origin "refs/tags/${major}"' \
  'refs="$(bash scripts/image_tags.sh "$RELEASE_TAG" "$IMAGE")"' \
  'docker buildx imagetools create "${args[@]}" "${IMAGE}@${DIGEST}"'
do
  printf '%s\n' "$RELEASE_CODE" | grep -qF -- "$needle" || release_missing+=("$needle")
done
moving_tags_code="$(grep -vE '^[[:space:]]*#' "$MOVING_TAGS")"
for needle in \
  '^v[0-9]+\.[0-9]+\.[0-9]+$' \
  'sort -V | tail -1)"'
do
  printf '%s\n' "$moving_tags_code" | grep -qF -- "$needle" \
    || release_missing+=("moving_tags.sh: ${needle}")
done
if [ ${#release_missing[@]} -eq 0 ]; then
  pass "the derivations under test are the ones release.yml runs"
else
  fail "the derivations under test are the ones release.yml runs" "absent: ${release_missing[*]}"
fi
# The move must happen after the release exists, or @v1 can point at a tag
# whose assets have not uploaded yet.
if printf '%s\n' "$release_yaml_code" \
  | awk '/name: Create release/{r=NR} /name: Move the major-version tag/{m=NR} END{exit !(r && m && m > r)}'; then
  pass "the major tag moves after the release is created"
else
  fail "the major tag moves after the release is created" "ordering in release.yml is wrong or a step was renamed"
fi

echo
echo "== the release notes advertise a tag that is published =="
# The notes tell readers what to `docker pull`, one job before anything is
# pushed. `github.ref_name` is v1.22.3 and no published tag carries the `v`,
# so the notes take their reference from image_tags.sh — and this drives that
# step for real and requires its answer to be among the tags the tagging step
# actually creates.
NOTES_STEP="$TMP/notes_step.sh"
: > "$NOTES_STEP"
for n in $(seq 1 30); do
  body="$(python3 "$(dirname "$0")/extract_run_bodies.py" "$RELEASE_YML" "block:$n" 2> /dev/null)" || continue
  case "$body" in
    *image_tags.sh*GITHUB_OUTPUT*) printf '%s\n' "$body" > "$NOTES_STEP"; break ;;
  esac
done
if [ ! -s "$NOTES_STEP" ]; then
  echo "could not find the step that resolves the advertised image reference in release.yml — refusing to test the wrong thing" >&2
  exit 1
fi

# The whole GITHUB_OUTPUT line, key included: the notes interpolate that key
# by name, so a check that read only the value could not see it renamed.
advertised() {
  local release_tag="$1"; shift
  local repo="$TMP/notesrepo"
  scratch_repo "$repo" "$@"
  mkdir -p "$repo/scripts"
  cp "$MOVING_TAGS" "$IMAGE_TAGS" "$repo/scripts/"
  : > "$TMP/notes.output"
  (
    cd "$repo"
    GITHUB_REF_NAME="$release_tag" REGISTRY=ghcr.io IMAGE_NAME=davidban77/sonda \
      GITHUB_OUTPUT="$TMP/notes.output" bash "$NOTES_STEP" > /dev/null 2>&1
  )
  cat "$TMP/notes.output"
}

check_advertised() {
  local label="$1" release_tag="$2" out ref created; shift 2
  out="$(advertised "$release_tag" "$@")"
  ref="${out#*=}"
  if [ -z "$out" ] || [ "$ref" = "$out" ]; then
    fail "$label" "the step wrote '${out}' to GITHUB_OUTPUT"
    return
  fi
  created="$(tagged "$release_tag" "$@")"
  case " $created " in
    *" --tag ${ref} "*) pass "$label" ;;
    *) fail "$label" "the notes advertise '${ref}', the tag step creates '${created}'" ;;
  esac
}

check_advertised "the notes advertise a tag the image job creates" \
  "v2.0.0" v1.22.3 v2.0.0
# A backport publishes one tag and nothing else, so this row has a single
# right answer where the row above has four.
check_advertised "a backport advertises the one tag it publishes" \
  "v1.21.0" v1.21.0 v1.21.4 v1.22.3

# Membership is not enough on its own: `latest` and `1` are published too,
# and both are in the list at the moment the notes are written. Release notes
# are read long after that, by which point a moving tag names a different
# image. So the reference must be one no later release can claim — asked by
# computing what a later release publishes and requiring the advertised tag
# to be absent from it.
later_tags="$(image_tags_for v1.30.0 v1.22.3 v1.30.0)"
adv_out="$(advertised v1.22.3 v1.21.0 v1.22.3)"
adv_tag="${adv_out##*:}"
if [ -z "$later_tags" ] || [ -z "$adv_out" ]; then
  fail "the advertised tag is one no later release can claim" \
    "advertised '${adv_out}', a later release publishes '${later_tags}'"
else
  case " $later_tags " in
    *" $adv_tag "*)
      fail "the advertised tag is one no later release can claim" \
        "the notes advertise '${adv_tag}', which v1.30.0 publishes too" ;;
    *) pass "the advertised tag is one no later release can claim" ;;
  esac
fi

# …and the notes must read that step. A resolver whose output nothing
# interpolates leaves the old spelling advertised, with every check above it
# green. Both halves of the expression are derived: the step's own id, and
# the key it wrote.
notes_id="$(grep -vE '^[[:space:]]*#' "$RELEASE_YML" | awk '
  /^[[:space:]]*- name:/ { id=""; seen=0 }
  /^[[:space:]]*id:/ { id=$2 }
  /image_tags\.sh/ { seen=1 }
  /GITHUB_OUTPUT/ { if (seen && id != "") print id }
')"
notes_out="$(advertised v2.0.0 v1.22.3 v2.0.0)"
# The notes line is the file's only `docker pull` outside a run: body — the
# others are the verification steps, which pull by digest.
notes_pull="$(grep -F 'docker pull' "$RELEASE_YML" | sed 's/^[[:space:]]*//' \
  | grep -vxF -f <(printf '%s\n' "$RELEASE_RAW" | sed 's/^[[:space:]]*//'))"
want_pull="docker pull \${{ steps.${notes_id}.outputs.${notes_out%%=*} }}"
if [ -n "$notes_id" ] && [ "$notes_pull" = "$want_pull" ]; then
  pass "the release notes advertise the resolved reference"
else
  fail "the release notes advertise the resolved reference" \
    "notes say '${notes_pull}', want '${want_pull}'"
fi

echo
echo "== one feature set, in every place that compiles one =="
# Three separate literals, nothing binding them: a divergence ships a `docker
# build .` without a sink the release has. Compared to each other rather than
# to a fourth copy kept here.
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
dockerfile_features="$(grep -vE '^[[:space:]]*#' "$REPO_ROOT/Dockerfile" \
  | grep -oE '^ARG FEATURES=[^[:space:]]+' | sed 's/^ARG FEATURES=//' \
  | sort -u | tr '\n' ' ' | sed 's/ $//')"
release_features="$(printf '%s\n' "$RELEASE_CODE" \
  | grep -oE -- '--features [^[:space:]]+' | sed 's/^--features //' \
  | sort -u | tr '\n' ' ' | sed 's/ $//')"
e2e_features="$(grep -vE '^[[:space:]]*#' "$REPO_ROOT/tests/e2e/run.sh" \
  | grep -oE -- '--features [^[:space:]]+' | sed 's/^--features //' \
  | sort -u | tr '\n' ' ' | sed 's/ $//')"
if [ -n "$dockerfile_features" ] \
  && [ "$dockerfile_features" = "$release_features" ] \
  && [ "$release_features" = "$e2e_features" ]; then
  pass "the Dockerfile default, the release build and the e2e build agree (${release_features})"
else
  fail "the Dockerfile default, the release build and the e2e build agree" \
    "Dockerfile '${dockerfile_features}', release.yml '${release_features}', tests/e2e/run.sh '${e2e_features}'"
fi

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "action argv: all checks passed"
  exit 0
fi
echo "action argv: ${FAILURES} check(s) failed"
exit 1
