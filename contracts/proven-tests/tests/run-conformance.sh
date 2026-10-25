#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
#
# run-conformance.sh — prove proven-tests-evidence/1.0.0 with fixtures.
#
#   bash contracts/proven-tests/tests/run-conformance.sh
#
# Two populations are exercised:
#
#   verify   committed evidence documents against bin/verify-evidence, comparing
#            admit/refuse, green, derived coverage and the rule ids that fired;
#   collect  committed libtest logs and coordinate maps against
#            bin/collect-evidence.sh, then the submission the collector produced
#            against bin/verify-evidence, so the producer→gate path is tested and
#            not only the gate.
#
# Two further cases check the revision marker and the SHASUMS manifest.
#
# Needs bash, coreutils, awk and jq (1.6+). No Rust and no Idris2: the point of
# the contract is that a submitter needs neither, and the suite proves it by
# running without them.
#
# Exit codes: 0 every planned case ran and passed, 1 a case failed or did not
# run, 2 the suite could not start.

set -euo pipefail

readonly CONTRACT="proven-tests-evidence/1.0.0"
SUITE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXPECT="${SUITE_DIR}/fixtures/expectations.json"
VERIFY="${SUITE_DIR}/bin/verify-evidence"
COLLECT="${SUITE_DIR}/bin/collect-evidence.sh"
MAKE_SUMS="${SUITE_DIR}/../bin/make-shasums.sh"

readonly FIX_COMMIT="1111111111111111111111111111111111111111"
readonly FIX_REPO="hyperpolymath/ordering-demo"
readonly FIX_STARTED="2026-10-25T10:00:00Z"
readonly FIX_FINISHED="2026-10-25T10:00:02Z"

for tool in jq bash awk; do
  command -v "$tool" >/dev/null 2>&1 || { printf 'missing required tool: %s\n' "$tool" >&2; exit 2; }
done
[[ -r "$EXPECT" ]] || { printf 'missing %s\n' "$EXPECT" >&2; exit 2; }
[[ -x "$VERIFY" ]] || { printf '%s is not executable\n' "$VERIFY" >&2; exit 2; }
[[ -x "$COLLECT" ]] || { printf '%s is not executable\n' "$COLLECT" >&2; exit 2; }

passed=0
failed=0
ran=0
WORK="$(mktemp -d "${TMPDIR:-/tmp}/proven-conformance.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

ok()  { printf 'ok    %s\n' "$1"; passed=$((passed + 1)); }
bad() { printf 'FAIL  %s: %s\n' "$1" "$2"; failed=$((failed + 1)); }

# compare_report NAME REPORT WANT_JSON — compare the fields that matter.
compare_report() {
  local name="$1" report="$2" want_exit="$3" want_adm="$4" want_green="$5" want_cells="$6" want_rules="$7"
  local got_adm got_green got_cells got_rules
  got_adm="$(printf '%s' "$report" | jq -r '.admissible' 2>/dev/null || echo unparsed)"
  got_green="$(printf '%s' "$report" | jq -r '.green' 2>/dev/null || echo unparsed)"
  got_cells="$(printf '%s' "$report" | jq -r '.coverage.cells_covered' 2>/dev/null || echo unparsed)"
  got_rules="$(printf '%s' "$report" | jq -c '[.findings[].rule] | unique | sort' 2>/dev/null || echo unparsed)"

  local problem=""
  if [[ "$got_adm" == "unparsed" ]]; then
    problem="stdout is not a report document"
  elif [[ "$want_adm" != "$got_adm" ]]; then
    problem="admissible <$got_adm>, expected <$want_adm>"
  elif [[ "$want_green" != "$got_green" ]]; then
    problem="green <$got_green>, expected <$want_green>"
  elif [[ "$want_cells" != "$got_cells" ]]; then
    problem="coverage cells <$got_cells>, expected <$want_cells>"
  elif [[ "$want_rules" != "$got_rules" ]]; then
    problem="rules <$got_rules>, expected <$want_rules>"
  elif [[ "$want_exit" == "0" && "$got_adm" != "true" ]]; then
    problem="exit 0 with an inadmissible report"
  fi

  if [[ -z "$problem" ]]; then
    printf 'ok    verify:  %s\n' "$name"
    passed=$((passed + 1))
  else
    printf 'FAIL  verify:  %s: %s\n' "$name" "$problem"
    failed=$((failed + 1))
  fi
}

verify_case() {
  local i="$1"
  local name file want_exit want_adm want_green want_cells want_rules out rc=0
  name="$(jq -r ".verify[$i].name" "$EXPECT")"
  file="$(jq -r ".verify[$i].file" "$EXPECT")"
  want_exit="$(jq -r ".verify[$i].exit" "$EXPECT")"
  want_adm="$(jq -r ".verify[$i].admissible" "$EXPECT")"
  want_green="$(jq -r ".verify[$i].green" "$EXPECT")"
  want_cells="$(jq -r ".verify[$i].cells" "$EXPECT")"
  want_rules="$(jq -c '.verify['"$i"'].rules | sort' "$EXPECT")"
  local -a args=()
  while IFS= read -r a; do args+=("$a"); done < <(jq -r ".verify[$i].args[]?" "$EXPECT")

  ran=$((ran + 1))
  out="$("$VERIFY" "${args[@]+"${args[@]}"}" "$SUITE_DIR/$file" 2>/dev/null)" || rc=$?
  if [[ "$rc" != "$want_exit" ]]; then
    printf 'FAIL  verify:  %s: exit %s, expected %s\n' "$name" "$rc" "$want_exit"
    failed=$((failed + 1))
    return
  fi
  compare_report "$name" "$out" "$want_exit" "$want_adm" "$want_green" "$want_cells" "$want_rules"
}

collect_case() {
  local i="$1"
  local name log coords exit_code expect_exit out rc=0
  name="$(jq -r ".collect[$i].name" "$EXPECT")"
  log="$(jq -r ".collect[$i].log" "$EXPECT")"
  coords="$(jq -r ".collect[$i].coords" "$EXPECT")"
  exit_code="$(jq -r ".collect[$i].exit_code" "$EXPECT")"
  expect_exit="$(jq -r ".collect[$i].expect_exit" "$EXPECT")"

  ran=$((ran + 1))
  out="$("$COLLECT" \
    --repo "$FIX_REPO" --commit "$FIX_COMMIT" \
    --log "$SUITE_DIR/$log" --exit-code "$exit_code" \
    --coords "$SUITE_DIR/$coords" \
    --command-argv '["cargo","test","--test","ordering","--","--test-threads=1"]' \
    --started-at "$FIX_STARTED" --finished-at "$FIX_FINISHED" \
    --runner ubuntu-24.04 --rust 1.85.0 2>/dev/null)" || rc=$?

  if [[ "$rc" != "$expect_exit" ]]; then
    printf 'FAIL  collect: %s: exit %s, expected %s\n' "$name" "$rc" "$expect_exit"
    failed=$((failed + 1))
    return
  fi
  if [[ "$expect_exit" != "0" ]]; then
    if [[ -n "$out" ]]; then
      printf 'FAIL  collect: %s: refused but wrote a document to stdout\n' "$name"
      failed=$((failed + 1))
    else
      printf 'ok    collect: %s (refused, and wrote nothing)\n' "$name"
      passed=$((passed + 1))
    fi
    return
  fi

  # The producer's own output must then satisfy the gate.
  printf '%s\n' "$out" > "$WORK/evidence.json"
  local t_args t_exit t_adm t_green t_cells
  t_exit="$(jq -r ".collect[$i].then.exit" "$EXPECT")"
  t_adm="$(jq -r ".collect[$i].then.admissible" "$EXPECT")"
  t_green="$(jq -r ".collect[$i].then.green" "$EXPECT")"
  t_cells="$(jq -r ".collect[$i].then.cells" "$EXPECT")"
  local -a targs=()
  while IFS= read -r a; do targs+=("$a"); done < <(jq -r ".collect[$i].then.args[]?" "$EXPECT")

  local vrc=0 vout
  vout="$("$VERIFY" "${targs[@]+"${targs[@]}"}" "$WORK/evidence.json" 2>/dev/null)" || vrc=$?
  if [[ "$vrc" != "$t_exit" ]]; then
    printf 'FAIL  collect: %s: collected evidence verified with exit %s, expected %s\n' "$name" "$vrc" "$t_exit"
    failed=$((failed + 1))
    return
  fi
  compare_report "$name (collected, then verified)" "$vout" "$t_exit" "$t_adm" "$t_green" "$t_cells" '[]'
}

main() {
  local n i

  n="$(jq -r '.verify | length' "$EXPECT")"
  for ((i = 0; i < n; i++)); do verify_case "$i"; done

  n="$(jq -r '.collect | length' "$EXPECT")"
  for ((i = 0; i < n; i++)); do collect_case "$i"; done

  ran=$((ran + 1))
  if [[ "$(head -n 1 "$SUITE_DIR/REVISION" | tr -d '\r')" == "$CONTRACT" ]]; then
    printf 'ok    revision: REVISION names %s\n' "$CONTRACT"
    passed=$((passed + 1))
  else
    printf 'FAIL  revision: REVISION is %s, expected %s\n' "$(head -n 1 "$SUITE_DIR/REVISION")" "$CONTRACT"
    failed=$((failed + 1))
  fi

  ran=$((ran + 1))
  if bash "$MAKE_SUMS" "$SUITE_DIR" --check 2>/dev/null; then
    printf 'ok    manifest: SHASUMS matches every published file\n'
    passed=$((passed + 1))
  else
    printf 'FAIL  manifest: SHASUMS does not match the files on disk\n'
    failed=$((failed + 1))
  fi

  local planned
  planned=$(( $(jq -r '.verify | length' "$EXPECT") + $(jq -r '.collect | length' "$EXPECT") + 2 ))

  printf '\n%d passed, %d failed, %d/%d planned cases ran\n' "$passed" "$failed" "$ran" "$planned"
  if [[ "$failed" != "0" ]]; then
    printf 'conformance: FAILED\n' >&2
    exit 1
  fi
  if [[ "$ran" != "$planned" ]]; then
    printf 'conformance: REFUSED — %d of %d planned cases ran; a case that does not run is not a pass\n' \
      "$ran" "$planned" >&2
    exit 1
  fi
  printf 'conformance: %s OK\n' "$CONTRACT"
}

main "$@"
