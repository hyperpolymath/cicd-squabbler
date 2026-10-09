#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# squabble_action_test.sh — exercise each guard of .github/actions/squabble
# on its own, then install.sh end to end, with planted negatives.
#
#   0. action.yml calls exactly the scripts this suite tests, with exactly the
#      env and conditions it tests, and nothing else; four planted mutants of
#      it must each be refused. This repository has an actions.lock, which
#      does not support local-path actions, so CI cannot run the action with
#      `uses: ./`; this check ties what CI runs to what the action runs.
#   1. check_attestation_json, offline, against hand-built fixtures: the real
#      shape passes; an empty array, a wrong digest, a missing subject, one bad
#      result among good ones, a non-array and an empty file are refused.
#   2. run.sh, offline, against a stand-in binary: arguments are split on
#      newlines only, taken literally, blank lines dropped; the exit code is
#      passed through and recorded; a missing binary is refused.
#   3. verify_digest and verify_attestation against the real v0.1.0 asset
#      (must pass) and a copy with one byte appended (must be refused).
#   4. install.sh end to end with `gh release download` shimmed to deliver the
#      tampered copy: it must exit non-zero and write nothing to GITHUB_PATH
#      or GITHUB_OUTPUT. Then the real download: it must install the binary
#      and record both.
#
# Parts 3 and 4 need the network and a GitHub token (GH_TOKEN, or a gh login);
# part 0 needs mikefarah yq v4. The run ends by checking that every planned
# case ran, so a skipped section cannot pass silently.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ACTION_DIR="${ROOT}/.github/actions/squabble"
ACTION_YML="${ACTION_DIR}/action.yml"
INSTALL_SH="${ACTION_DIR}/install.sh"
RUN_SH="${ACTION_DIR}/run.sh"
EXPECTED_CASES=27

# shellcheck source-path=SCRIPTDIR source=../.github/actions/squabble/install.sh
source "$INSTALL_SH"

WORK="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/squabble-test.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

passed=0
failed=0

# expect_pass NAME CMD... — run CMD in a subshell and record a failure unless it succeeds.
expect_pass() {
  local name="$1"
  shift
  if ("$@"); then
    printf 'ok    %s\n' "$name"
    passed=$((passed + 1))
  else
    printf 'FAIL  %s (expected success)\n' "$name"
    failed=$((failed + 1))
  fi
}

# expect_fail NAME CMD... — run CMD in a subshell and record a failure unless it is refused.
expect_fail() {
  local name="$1"
  shift
  if ("$@"); then
    printf 'FAIL  %s (expected refusal, got success)\n' "$name"
    failed=$((failed + 1))
  else
    printf 'ok    %s (refused, as it must be)\n' "$name"
    passed=$((passed + 1))
  fi
}

# action_contract FILE — succeed only if the composite action in FILE has
# exactly the two steps this suite tests (install.sh, then run.sh only when
# args is set), with exactly their env, no `uses:`, and outputs wired to
# them; print the first mismatch.
action_contract() {
  local file="$1" i got
  command -v yq >/dev/null || { echo "  yq (mikefarah v4) is required" >&2; return 1; }
  # shellcheck disable=SC2016  # the ${{ }} and ${GITHUB_ACTION_PATH} are literal text
  local -a checks=(
    '.runs.using' 'composite'
    '.runs.steps | length' '2'
    '[.runs.steps[] | select(has("uses"))] | length' '0'
    '.runs.steps[0].id' 'install'
    '.runs.steps[0].shell' 'bash'
    '.runs.steps[0].run' 'bash "${GITHUB_ACTION_PATH}/install.sh"'
    '.runs.steps[0] | has("if")' 'false'
    '.runs.steps[0].env | to_entries | sort_by(.key) | map(.key + "=" + .value) | join(",")'
    'GH_TOKEN=${{ github.token }}'
    '.runs.steps[1].id' 'run'
    '.runs.steps[1].shell' 'bash'
    '.runs.steps[1].run' 'bash "${GITHUB_ACTION_PATH}/run.sh"'
    '.runs.steps[1].if' "inputs.args != ''"
    '.runs.steps[1].env | to_entries | sort_by(.key) | map(.key + "=" + .value) | join(",")'
    'GH_TOKEN=${{ inputs.token }},SQUABBLE_ARGS=${{ inputs.args }},SQUABBLE_BIN=${{ steps.install.outputs.path }}'
    '.outputs.path.value' '${{ steps.install.outputs.path }}'
    '.outputs."exit-code".value' '${{ steps.run.outputs.exit-code }}'
  )
  for ((i = 0; i < ${#checks[@]}; i += 2)); do
    got="$(yq -r "${checks[i]}" "$file")" || return 1
    if [[ "$got" != "${checks[i + 1]}" ]]; then
      printf '  %s: got [%s], want [%s]\n' "${checks[i]}" "$got" "${checks[i + 1]}" >&2
      return 1
    fi
  done
}

# mutant NAME YQ_EXPR — write action.yml with YQ_EXPR applied under WORK and print its path.
mutant() {
  yq "$2" "$ACTION_YML" >"${WORK}/mutant-$1.yml"
  printf '%s\n' "${WORK}/mutant-$1.yml"
}

# scripts_executable — succeed only if both scripts the action runs are executable files.
scripts_executable() {
  [[ -f "$INSTALL_SH" && -x "$INSTALL_SH" && -f "$RUN_SH" && -x "$RUN_SH" ]]
}

echo "== 0. action.yml runs exactly what this suite tests"
expect_pass "action.yml matches the tested wiring" action_contract "$ACTION_YML"
# shellcheck disable=SC2016  # literal ${{ }} in the yq expressions
{
  expect_fail "mutant: install step runs another script" action_contract \
    "$(mutant other-script '.runs.steps[0].run = "bash \"${GITHUB_ACTION_PATH}/other.sh\""')"
  expect_fail "mutant: an extra step with uses:" action_contract \
    "$(mutant extra-uses '.runs.steps += [{"uses": "example/action@v1"}]')"
  expect_fail "mutant: run step gets github.token" action_contract \
    "$(mutant token-swap '.runs.steps[1].env.GH_TOKEN = "${{ github.token }}"')"
  expect_fail "mutant: run step loses its if:" action_contract \
    "$(mutant no-if 'del(.runs.steps[1].if)')"
}
expect_pass "install.sh and run.sh are executable" scripts_executable

# fixture NAME JSON — write JSON to a fixture file under WORK and print its path.
fixture() {
  printf '%s' "$2" >"${WORK}/$1.json"
  printf '%s\n' "${WORK}/$1.json"
}

# result SUBJECT_NAME DIGEST — print one verification result naming SHA256SUMS
# and SUBJECT_NAME with DIGEST, as `gh attestation verify --format json` does.
result() {
  jq -cn --arg n "$1" --arg d "$2" '{verificationResult: {statement: {subject: [
    {name: "SHA256SUMS", digest: {sha256: "ddf35860797c"}},
    {name: $n, digest: {sha256: $d}}]}}}'
}

echo "== 1. attestation JSON check (offline)"
good="$(result "$SQUABBLE_ASSET" "$SQUABBLE_SHA256")"
wrong="$(result "$SQUABBLE_ASSET" "$(printf '0%.0s' {1..64})")"
other="$(result "some-other-asset" "$SQUABBLE_SHA256")"
expect_pass "real-shaped result" check_attestation_json "$(fixture good "[${good}]")"
expect_pass "two good results" check_attestation_json "$(fixture good2 "[${good},${good}]")"
expect_fail "empty array" check_attestation_json "$(fixture empty '[]')"
expect_fail "wrong digest for the asset" check_attestation_json "$(fixture wrong "[${wrong}]")"
expect_fail "asset not among the subjects" check_attestation_json "$(fixture other "[${other}]")"
expect_fail "one bad result among good ones" check_attestation_json "$(fixture mixed "[${good},${wrong}]")"
expect_fail "an object, not an array" check_attestation_json "$(fixture object "${good}")"
expect_fail "empty file" check_attestation_json "$(fixture blank '')"

echo "== 2. run.sh against a stand-in binary (offline)"
cat >"${WORK}/fake-squabble" <<'EOF'
#!/usr/bin/env bash
# Stand-in for squabble: print the argument count, then each argument on its
# own line, and exit with FAKE_RC.
printf '%s\n' "$#"
if (($# > 0)); then printf '%s\n' "$@"; fi
exit "${FAKE_RC:-0}"
EOF
chmod 0755 "${WORK}/fake-squabble"

# run_with ARGS [RC] — run run.sh on the stand-in with SQUABBLE_ARGS=ARGS and
# the stand-in exiting RC; its stdout lands in run.out, GITHUB_OUTPUT in
# run.gh, and run.sh's exit code is returned.
run_with() {
  : >"${WORK}/run.gh"
  SQUABBLE_BIN="${WORK}/fake-squabble" SQUABBLE_ARGS="$1" FAKE_RC="${2:-0}" \
    GITHUB_OUTPUT="${WORK}/run.gh" bash "$RUN_SH" >"${WORK}/run.out"
}

# ran_with RC STDOUT — succeed only if the last run_with exited RC, printed
# exactly STDOUT, and recorded exit-code=RC.
ran_with() {
  [[ "$(cat "${WORK}/run.out")" == "$2" ]] || return 1
  [[ "$(cat "${WORK}/run.gh")" == "exit-code=$1" ]]
}

# case_split — blank lines are dropped and each other line is one argument.
case_split() {
  run_with $'--a\n\nb c\n' || return 1
  ran_with 0 $'2\n--a\nb c'
}

# case_literal — no globbing, expansion or word splitting inside a line.
case_literal() {
  # shellcheck disable=SC2016  # the $HOME and backticks must stay literal
  local arg='--x=* $HOME `id` "q"'
  run_with "$arg" || return 1
  ran_with 0 "1"$'\n'"$arg"
}

# case_empty — empty SQUABBLE_ARGS runs the binary with no arguments.
case_empty() {
  run_with '' || return 1
  ran_with 0 "0"
}

# case_nonzero — a non-zero exit is passed through and recorded.
case_nonzero() {
  local rc=0
  run_with --v 2 || rc=$?
  [[ "$rc" == 2 ]] || return 1
  ran_with 2 $'1\n--v'
}

# case_missing_bin — a missing binary exits 1 and records nothing.
case_missing_bin() {
  local rc=0
  : >"${WORK}/run.gh"
  SQUABBLE_BIN="${WORK}/absent" SQUABBLE_ARGS=--v GITHUB_OUTPUT="${WORK}/run.gh" \
    bash "$RUN_SH" 2>/dev/null || rc=$?
  [[ "$rc" == 1 && ! -s "${WORK}/run.gh" ]]
}

expect_pass "blank lines dropped, one argument per line" case_split
expect_pass "a line is taken literally" case_literal
expect_pass "empty args: no arguments" case_empty
expect_pass "non-zero exit passed through and recorded" case_nonzero
expect_pass "missing binary refused, nothing recorded" case_missing_bin

echo "== 3. digest and attestation against the real asset and a tampered copy"
gh release download "$SQUABBLE_TAG" --repo "$SQUABBLE_REPO" \
  --pattern "$SQUABBLE_ASSET" --dir "${WORK}/real"
real="${WORK}/real/${SQUABBLE_ASSET}"
mkdir -p "${WORK}/tampered"
tampered="${WORK}/tampered/${SQUABBLE_ASSET}"
cp -- "$real" "$tampered"
printf '\0' >>"$tampered"
expect_pass "real asset: sha256 matches the pin" verify_digest "$real"
expect_fail "tampered copy: sha256" verify_digest "$tampered"
expect_pass "real asset: attestation" verify_attestation "$real"
expect_fail "tampered copy: attestation" verify_attestation "$tampered"

echo "== 4. install.sh end to end"
real_gh="$(command -v gh)"
mkdir -p "${WORK}/shim"
cat >"${WORK}/shim/gh" <<EOF
#!/usr/bin/env bash
# Test shim: serve the tampered copy for 'release download', pass all else through.
set -euo pipefail
if [[ "\${1:-}" == release && "\${2:-}" == download ]]; then
  dir=""
  while [[ \$# -gt 0 ]]; do
    if [[ "\$1" == --dir ]]; then dir="\$2"; fi
    shift
  done
  mkdir -p "\$dir"
  cp -- "${tampered}" "\$dir/${SQUABBLE_ASSET}"
  exit 0
fi
exec "${real_gh}" "\$@"
EOF
chmod 0755 "${WORK}/shim/gh"

# install_into DIR [PATH_PREFIX] — run install.sh with GITHUB_PATH/GITHUB_OUTPUT
# under DIR, optionally with PATH_PREFIX first on PATH.
install_into() {
  local dir="$1" prefix="${2:-}"
  mkdir -p "$dir"
  : >"${dir}/path"
  : >"${dir}/output"
  GITHUB_PATH="${dir}/path" GITHUB_OUTPUT="${dir}/output" RUNNER_TEMP="$dir" \
    PATH="${prefix:+${prefix}:}${PATH}" bash "$INSTALL_SH"
}

# nothing_recorded DIR — succeed only if install_into DIR left both files empty.
nothing_recorded() {
  [[ ! -s "${1}/path" && ! -s "${1}/output" ]]
}

# installed_and_recorded DIR — succeed only if DIR's GITHUB_OUTPUT names an
# executable with the pinned digest that lives in the GITHUB_PATH directory.
installed_and_recorded() {
  local dir="$1" bin
  bin="$(sed -n 's/^path=//p' "${dir}/output")"
  [[ -n "$bin" && -x "$bin" ]] || return 1
  [[ "$(cat "${dir}/path")" == "$(dirname "$bin")" ]] || return 1
  verify_digest "$bin"
}

expect_fail "install.sh with a tampered download" install_into "${WORK}/e2e-bad" "${WORK}/shim"
expect_pass "tampered install recorded nothing" nothing_recorded "${WORK}/e2e-bad"
expect_pass "install.sh with the real download" install_into "${WORK}/e2e-good"
expect_pass "real install recorded PATH and output" installed_and_recorded "${WORK}/e2e-good"

total=$((passed + failed))
echo "== ${passed} passed, ${failed} failed, ${total} of ${EXPECTED_CASES} planned cases ran"
if (( failed != 0 || total != EXPECTED_CASES )); then
  exit 1
fi
