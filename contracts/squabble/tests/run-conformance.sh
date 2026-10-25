#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
#
# run-conformance.sh — prove squabble-contract/1.0.0 with fixtures.
#
# Every case in fixtures/expectations.json is run against the published
# artefacts and its exit code, verdict, evidence_free list, reasons and blocking
# runs are compared. Three further cases check the revision marker, the
# determinism of the verdict document, and the SHASUMS manifest.
#
#   bash contracts/squabble/tests/run-conformance.sh
#
# Needs bash, coreutils and jq (1.6+). No network, no token, no Rust toolchain —
# which is what makes it runnable in a consumer's CI as well as here.
#
# Exit codes: 0 every planned case ran and passed, 1 a case failed or did not
# run, 2 the suite could not start (missing jq, missing fixture set).

set -euo pipefail

readonly CONTRACT="squabble-contract/1.0.0"
SUITE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXPECT="${SUITE_DIR}/fixtures/expectations.json"
ORACLE="${SUITE_DIR}/bin/legitimate-green"
AUDIT="${SUITE_DIR}/bin/audit-consumer-workflow.sh"
MAKE_SUMS="${SUITE_DIR}/../bin/make-shasums.sh"

for tool in jq bash; do
  command -v "$tool" >/dev/null 2>&1 || { printf 'missing required tool: %s\n' "$tool" >&2; exit 2; }
done
[[ -r "$EXPECT" ]] || { printf 'missing %s\n' "$EXPECT" >&2; exit 2; }
[[ -x "$ORACLE" ]] || { printf '%s is not executable\n' "$ORACLE" >&2; exit 2; }
[[ -r "$AUDIT" ]] || { printf 'missing %s\n' "$AUDIT" >&2; exit 2; }

passed=0
failed=0
ran=0

ok()   { printf 'ok    %s\n' "$1"; passed=$((passed + 1)); }
bad()  { printf 'FAIL  %s: %s\n' "$1" "$2"; failed=$((failed + 1)); }

# expect_eq NAME WANT GOT
expect_eq() {
  local name="$1" want="$2" got="$3"
  if [[ "$want" == "$got" ]]; then
    return 0
  fi
  bad "$name" "expected <$want>, got <$got>"
  return 1
}

oracle_case() {
  local i="$1"
  local name file want_exit out rc=0
  name="$(jq -r ".oracle[$i].name" "$EXPECT")"
  file="$(jq -r ".oracle[$i].file" "$EXPECT")"
  want_exit="$(jq -r ".oracle[$i].exit" "$EXPECT")"
  local -a args=()
  while IFS= read -r a; do args+=("$a"); done < <(jq -r ".oracle[$i].args[]?" "$EXPECT")

  ran=$((ran + 1))
  out="$("$ORACLE" "${args[@]+"${args[@]}"}" "$SUITE_DIR/$file" 2>/dev/null)" || rc=$?

  local problem=""
  if [[ "$rc" != "$want_exit" ]]; then
    problem="exit $rc, expected $want_exit"
  elif ! printf '%s' "$out" | jq -e . >/dev/null 2>&1; then
    problem="stdout is not JSON"
  else
    local want_verdict got_verdict want_lg got_lg
    want_verdict="$(jq -r ".oracle[$i].verdict" "$EXPECT")"
    got_verdict="$(printf '%s' "$out" | jq -r '.verdict')"
    want_lg="$(jq -r ".oracle[$i].legitimate_green" "$EXPECT")"
    got_lg="$(printf '%s' "$out" | jq -r '.legitimate_green')"
    local want_block got_block want_ef got_ef want_reasons got_reasons
    want_block="$(jq -c '.oracle['"$i"'].blocking_runs | sort' "$EXPECT")"
    got_block="$(printf '%s' "$out" | jq -c '[.blocking[].run] | sort')"
    want_ef="$(jq -c '.oracle['"$i"'].evidence_free | sort' "$EXPECT")"
    got_ef="$(printf '%s' "$out" | jq -c '.evidence_free | sort')"
    want_reasons="$(jq -c '.oracle['"$i"'].reasons | sort' "$EXPECT")"
    got_reasons="$(printf '%s' "$out" | jq -c '.reasons | sort')"

    if [[ "$want_verdict" != "$got_verdict" ]]; then
      problem="verdict <$got_verdict>, expected <$want_verdict>"
    elif [[ "$want_lg" != "$got_lg" ]]; then
      problem="legitimate_green <$got_lg>, expected <$want_lg>"
    elif [[ "$want_block" != "$got_block" ]]; then
      problem="blocking runs <$got_block>, expected <$want_block>"
    elif [[ "$want_ef" != "$got_ef" ]]; then
      problem="evidence_free <$got_ef>, expected <$want_ef>"
    elif [[ "$want_reasons" != "$got_reasons" ]]; then
      problem="reasons <$got_reasons>, expected <$want_reasons>"
    elif [[ "$(printf '%s' "$out" | jq -r '.contract')" != "$CONTRACT" ]]; then
      problem="output names contract $(printf '%s' "$out" | jq -r '.contract'), expected $CONTRACT"
    elif [[ "$want_lg" == "false" && "$rc" == "0" ]]; then
      problem="a non-green verdict exited 0"
    fi
  fi

  if [[ -z "$problem" ]]; then
    printf 'ok    oracle: %s\n' "$name"
    passed=$((passed + 1))
  else
    printf 'FAIL  oracle: %s: %s\n' "$name" "$problem"
    failed=$((failed + 1))
  fi
}

audit_case() {
  local i="$1"
  local name file want_exit out rc=0
  name="$(jq -r ".audit[$i].name" "$EXPECT")"
  file="$(jq -r ".audit[$i].file" "$EXPECT")"
  want_exit="$(jq -r ".audit[$i].exit" "$EXPECT")"

  ran=$((ran + 1))
  out="$(bash "$AUDIT" "$SUITE_DIR/$file" 2>/dev/null)" || rc=$?

  local problem=""
  if [[ "$rc" != "$want_exit" ]]; then
    problem="exit $rc, expected $want_exit"
  else
    local code
    while IFS= read -r code; do
      [[ -z "$code" ]] && continue
      if ! printf '%s\n' "$out" | grep -qE "^${code}([[:space:]]|$)"; then
        problem="missing finding $code"
        break
      fi
    done < <(jq -r ".audit[$i].codes[]?" "$EXPECT")
    if [[ -z "$problem" && "$want_exit" == "0" && -n "$out" ]]; then
      problem="clean case produced findings: $out"
    fi
  fi

  if [[ -z "$problem" ]]; then
    printf 'ok    audit:  %s\n' "$name"
    passed=$((passed + 1))
  else
    printf 'FAIL  audit:  %s: %s\n' "$name" "$problem"
    failed=$((failed + 1))
  fi
}

main() {
  local n i

  n="$(jq -r '.oracle | length' "$EXPECT")"
  for ((i = 0; i < n; i++)); do oracle_case "$i"; done

  n="$(jq -r '.audit | length' "$EXPECT")"
  for ((i = 0; i < n; i++)); do audit_case "$i"; done

  # ── the revision marker, and the document it produces ──────────────────
  ran=$((ran + 1))
  if [[ "$(head -n 1 "$SUITE_DIR/REVISION" | tr -d '\r')" == "$CONTRACT" ]]; then
    printf 'ok    revision: REVISION names %s\n' "$CONTRACT"
    passed=$((passed + 1))
  else
    printf 'FAIL  revision: REVISION is %s, expected %s\n' \
      "$(head -n 1 "$SUITE_DIR/REVISION")" "$CONTRACT"
    failed=$((failed + 1))
  fi

  # ── determinism: the verdict document is byte-identical across runs ─────
  ran=$((ran + 1))
  local a b
  a="$("$ORACLE" "$SUITE_DIR/fixtures/gate/all-passed.bound.json" 2>/dev/null)" || true
  b="$("$ORACLE" "$SUITE_DIR/fixtures/gate/all-passed.bound.json" 2>/dev/null)" || true
  if [[ -n "$a" && "$a" == "$b" ]]; then
    printf 'ok    determinism: two runs produced identical documents\n'
    passed=$((passed + 1))
  else
    printf 'FAIL  determinism: two runs of the same fixture differ\n'
    failed=$((failed + 1))
  fi

  # ── the manifest: published bytes have not drifted ─────────────────────
  ran=$((ran + 1))
  if bash "$MAKE_SUMS" "$SUITE_DIR" --check 2>/dev/null; then
    printf 'ok    manifest: SHASUMS matches every published file\n'
    passed=$((passed + 1))
  else
    printf 'FAIL  manifest: SHASUMS does not match the files on disk\n'
    failed=$((failed + 1))
  fi

  local planned
  planned=$(( $(jq -r '.oracle | length' "$EXPECT") + $(jq -r '.audit | length' "$EXPECT") + 3 ))

  printf '\n%d passed, %d failed, %d/%d planned cases ran\n' \
    "$passed" "$failed" "$ran" "$planned"
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
