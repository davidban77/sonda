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
# tag denotes — and one implementation, scripts/moving_tags.sh, shared by
# the git-tag guard and the image-tag guard. A moving tag that moves when it
# should not silently reassigns every consumer pinned to it, so it gets a
# table, run against a REAL git repo: `sort -V` and `git tag -l` globbing
# are exactly the parts a reimplementation here would get right while
# production got them wrong.
MOVING_TAGS="$(cd "$(dirname "$0")/../.." && pwd)/scripts/moving_tags.sh"
RELEASE_YML="$(dirname "$0")/../../.github/workflows/release.yml"

decide() {
  local release_tag="$1"; shift
  local repo="$TMP/tagrepo"
  rm -rf "$repo"; mkdir -p "$repo"
  (
    cd "$repo"
    git init -q .; git config user.email t@t; git config user.name t
    git commit -q --allow-empty -m x
    for t in "$@"; do git tag "$t"; done
    bash "$MOVING_TAGS" "$release_tag" 2> /dev/null | tr '\n' ' ' | sed 's/ $//'
  )
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

# An empty tag list reads as "nothing is newer", so the two ways of getting
# one — no argument, no repository — must not answer `true` to anything.
if ! bash "$MOVING_TAGS" > "$TMP/noargs.out" 2> /dev/null && ! grep -q 'true' "$TMP/noargs.out"; then
  pass "no argument is refused rather than answered"
else
  fail "no argument is refused rather than answered" "$(cat "$TMP/noargs.out")"
fi
mkdir -p "$TMP/notarepo"
if ! (cd "$TMP/notarepo" && GIT_CEILING_DIRECTORIES="$TMP" bash "$MOVING_TAGS" v1.0.0) \
  > "$TMP/norepo.out" 2> /dev/null && ! grep -q 'true' "$TMP/norepo.out"; then
  pass "a missing git repository is refused rather than answered"
else
  fail "a missing git repository is refused rather than answered" "$(cat "$TMP/norepo.out")"
fi

# THE REAL STEP, NOT A COPY. The table above covers the decision; this
# covers the step that consumes it, in a scratch repo with a bare origin
# where the answer becomes an actual moved tag. A reader that exits on its
# first match once left the script dead of SIGPIPE and every "move" reading
# as "leave" — a defect entirely invisible to a needle.
MAJOR_STEP="$TMP/major_step.sh"
: > "$MAJOR_STEP"
for n in $(seq 1 20); do
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
  local repo="$TMP/steprepo" major="${release_tag%%.*}"
  rm -rf "$repo" "$repo.git"; mkdir -p "$repo"
  (
    git init -q --bare "${repo}.git"
    cd "$repo"
    git init -q .; git config user.email t@t; git config user.name t
    git remote add origin "${repo}.git"
    git commit -q --allow-empty -m older
    git tag "$major" # the moving tag, already published, on an older commit
    git commit -q --allow-empty -m release
    for t in "$@"; do git tag "$t"; done
    mkdir -p scripts && cp "$MOVING_TAGS" scripts/
    RELEASE_TAG="$release_tag" bash "$MAJOR_STEP" > /dev/null 2>&1
    if [ "$(git rev-parse "${major}^{commit}")" = "$(git rev-parse HEAD)" ]; then
      printf 'MOVED'
    else
      printf 'LEFT'
    fi
  )
}

check_step() {
  local label="$1" want="$2" got; shift 2
  got="$(moved_major "$@")"
  if [ "$got" = "$want" ]; then pass "$label"; else fail "$label" "got ${got}, want ${want}"; fi
}

check_step "the step really moves v1 onto the newest 1.x" MOVED "v1.22.3" v1.21.0 v1.22.3 v2.0.0
check_step "the step leaves v1 alone on an older 1.x"     LEFT  "v1.21.0" v1.21.0 v1.22.3 v2.0.0
check_step "the step moves v2 and never touches v1"       MOVED "v2.0.0"  v1.22.3 v2.0.0

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
  '*MOVE_MAJOR=true*) ;;' \
  'bash scripts/moving_tags.sh "$RELEASE_TAG" | tee -a "$GITHUB_ENV"' \
  'major="${RELEASE_TAG%%.*}"' \
  'git push --force origin "refs/tags/${major}"'
do
  printf '%s\n' "$RELEASE_CODE" | grep -qF -- "$needle" || release_missing+=("$needle")
done
for needle in \
  '^v[0-9]+\.[0-9]+\.[0-9]+$' \
  'sort -V | tail -1)"'
do
  grep -qF -- "$needle" "$MOVING_TAGS" || release_missing+=("moving_tags.sh: ${needle}")
done
if [ ${#release_missing[@]} -eq 0 ]; then
  pass "the derivation under test is the one both jobs call"
else
  fail "the derivation under test is the one both jobs call" "absent: ${release_missing[*]}"
fi

# The script's answer reaches the image tags as env: renaming a key in one
# file leaves every `enable=` false forever, and a moving tag that stops
# moving is invisible until someone pulls `latest` and gets an old release.
script_keys="$(grep -oE 'MOVE_[A-Z]+=' "$MOVING_TAGS" | tr -d '=' | sort -u | tr '\n' ' ')"
yaml_keys="$(grep -oE 'env\.MOVE_[A-Z]+' "$RELEASE_YML" | sed 's/env\.//' | sort -u | tr '\n' ' ')"
if [ "$script_keys" = "$yaml_keys" ] && [ -n "$script_keys" ]; then
  pass "release.yml gates on exactly the keys moving_tags.sh prints (${script_keys% })"
else
  fail "release.yml gates on exactly the keys moving_tags.sh prints" "script prints '${script_keys}', release.yml reads '${yaml_keys}'"
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
