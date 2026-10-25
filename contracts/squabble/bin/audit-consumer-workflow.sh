#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
#
# audit-consumer-workflow.sh — lint a consumer's workflow against
# squabble-contract/1.0.0 §8, before a human has to.
#
#   audit-consumer-workflow.sh .github/workflows/gate.yml
#
# It refuses, with a stable finding code:
#
#   E-UNPINNED-ACTION     the squabble action is not pinned to a 40-hex commit
#   E-MOVING-REF          a `uses:` ref of @main / @master / @HEAD
#   E-WRITE-PERMISSION    write-all, or any `*: write` scope
#   E-NO-PERMISSIONS      no top-level `permissions:` block at all
#   E-MASKED-STEP         continue-on-error: true, `|| true`, or `set +e`
#   E-BYPASS-COMMAND      a command that changes the gate instead of winning it
#   E-OUT-OF-CONTRACT-SUBCOMMAND  fight / board / inbox-sweep as an argument line
#   E-PRIVILEGED-TRIGGER  pull_request_target
#   E-NO-ACTION           the workflow never uses the pinned action
#   E-NO-SAMPLER          it never samples a gate (fetch / verify-satisfied / chains)
#   E-NO-ORACLE           it never asks legitimate-green, so it gates on `done`
#
# FINDINGS ARE A LINT, NOT A SANDBOX. This reads text and refuses patterns it
# knows; a workflow can be wrong in ways this does not model. The real boundary
# is the permission set: a read-only token cannot merge, re-run or edit
# anything, whatever the YAML says. Run both.
#
# Exit codes: 0 clean, 3 findings, 2 usage/environment failure. Finding lines go
# to stdout as `<code>  <basename>:<line>  <text>`; the summary goes to stderr.
#
# Contract revision: squabble-contract/1.0.0.

set -euo pipefail

readonly EXIT_OK=0
readonly EXIT_FINDINGS=3
readonly EXIT_USAGE=2

usage() {
  printf 'usage: audit-consumer-workflow.sh FILE.yml\n' >&2
}

die() {
  printf 'audit-consumer-workflow: %s\n' "$*" >&2
  exit "$EXIT_USAGE"
}

main() {
  if [[ $# -ne 1 ]]; then usage; exit "$EXIT_USAGE"; fi
  case "$1" in -h|--help) usage; exit 0 ;; -*) usage; die "unknown option: $1" ;; esac
  local file="$1"
  [[ -r "$file" ]] || die "cannot read $file"
  local base
  base="$(basename "$file")"

  # Findings are buffered so the same (code, line) cannot be reported twice by
  # two patterns that both describe one defect, and so output is stable.
  local -a findings=()

  # report CODE LINE TEXT — record one finding.
  report() {
    findings+=("$(printf '%s\t%04d\t%s' "$1" "$2" "$3")")
  }

  # collect CODE REGEX — report every line of $file matching REGEX.
  collect() {
    local code="$1" re="$2" line text
    while IFS=: read -r line text; do
      [[ -z "$line" ]] && continue
      report "$code" "$line" "$(printf '%s' "$text" | sed 's/^[[:space:]]*//')"
    done < <(grep -nEi -- "$re" "$file" 2>/dev/null || true)
  }

  # ── pins ────────────────────────────────────────────────────────────────
  if grep -qEi 'uses:[[:space:]]*[^#]*cicd-squabbler' "$file" 2>/dev/null; then
    local line text
    while IFS=: read -r line text; do
      [[ -z "$line" ]] && continue
      if ! printf '%s\n' "$text" | grep -Eq '@[0-9a-fA-F]{40}([[:space:]]|$)'; then
        report "E-UNPINNED-ACTION" "$line" "$(printf '%s' "$text" | sed 's/^[[:space:]]*//')"
      fi
    done < <(grep -nEi 'uses:[[:space:]]*[^#]*cicd-squabbler' "$file")
  else
    report "E-NO-ACTION" "0" "no step uses hyperpolymath/cicd-squabbler/.github/actions/squabble"
  fi

  collect "E-MOVING-REF" 'uses:[[:space:]]*[^#]*@(main|master|HEAD)([[:space:]]|$)'
  collect "E-WRITE-PERMISSION" '(^|[[:space:]])write-all([[:space:]]|$)|[a-z-]+:[[:space:]]*["'"'"']?write["'"'"']?([[:space:]]|$)'
  if ! grep -qE '^permissions:' "$file" 2>/dev/null; then
    report "E-NO-PERMISSIONS" "0" "no top-level permissions block; the token's default scopes are not this contract's floor"
  fi

  # ── a gate that can fail must be able to fail ────────────────────────────
  collect "E-MASKED-STEP" 'continue-on-error:[[:space:]]*true'
  collect "E-MASKED-STEP" '\|\|[[:space:]]*(true|:)([[:space:]]|$)'
  collect "E-MASKED-STEP" 'set[[:space:]]+\+e'

  # ── bypasses ────────────────────────────────────────────────────────────
  collect "E-BYPASS-COMMAND" '--admin([[:space:]]|$)'
  collect "E-BYPASS-COMMAND" 'enforce_admins'
  collect "E-BYPASS-COMMAND" 'squabble[[:space:]]+[^|]*--apply'
  collect "E-BYPASS-COMMAND" '(gh[[:space:]]+api|api\.github\.com)[^|]*(-X|--method)[[:space:]]*(PUT|PATCH|POST|DELETE)'
  collect "E-BYPASS-COMMAND" 'gh[[:space:]]+(run[[:space:]]+rerun|workflow[[:space:]]+run)'
  collect "E-OUT-OF-CONTRACT-SUBCOMMAND" '^[[:space:]]*(fight|board|inbox-sweep)([[:space:]]|$)'
  collect "E-OUT-OF-CONTRACT-SUBCOMMAND" 'squabble[[:space:]]+(fight|board|inbox-sweep)([[:space:]]|$)'
  collect "E-PRIVILEGED-TRIGGER" 'pull_request_target'

  # ── the read-only surface must actually be used ─────────────────────────
  if ! grep -qE '(^|[[:space:]])squabble[[:space:]]+(fetch|diagnose|verify-satisfied|chains)([[:space:]]|$)' "$file" 2>/dev/null; then
    report "E-NO-SAMPLER" "0" "no read-only squabble subcommand (fetch/diagnose/verify-satisfied/chains) is invoked"
  fi
  if ! grep -qE 'legitimate-green' "$file" 2>/dev/null; then
    report "E-NO-ORACLE" "0" "legitimate-green is never invoked; gating on \`done\` is a weaker claim (CONTRACT.adoc §5)"
  fi

  if (( ${#findings[@]} > 0 )); then
    local row
    while IFS= read -r row; do
      printf '%s  %s:%s  %s\n' \
        "$(printf '%s' "$row" | cut -f1)" \
        "$base" \
        "$(printf '%s' "$row" | cut -f2 | sed 's/^0*//;s/^$/0/')" \
        "$(printf '%s' "$row" | cut -f3)"
    done < <(printf '%s\n' "${findings[@]}" | sort -u)
    printf '%d finding(s) in %s\n' "$(printf '%s\n' "${findings[@]}" | sort -u | wc -l | tr -d ' ')" "$base" >&2
    exit "$EXIT_FINDINGS"
  fi
  printf 'clean  %s\n' "$base" >&2
  exit "$EXIT_OK"
}

main "$@"
