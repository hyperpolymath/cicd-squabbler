#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
#
# collect-evidence.sh — turn ONE captured `cargo test` run into a
# proven-tests-evidence/1.0.0 submission.
#
#   just cargo-test 2>&1 | tee test.log
#   collect-evidence.sh \
#     --repo "$GITHUB_REPOSITORY" --commit "$GITHUB_SHA" \
#     --log test.log --exit-code "$cargo_exit" \
#     --command-argv '["cargo","test","--","--test-threads=1"]' \
#     --started-at 2026-10-25T10:00:00Z --finished-at 2026-10-25T10:00:31Z \
#     --runner ubuntu-24.04 --rust 1.85.0 \
#     --coords .proven/coords.json > evidence.json
#
# The collector is the *reference* producer, not the gate: verify-evidence is
# the gate. What this script refuses, it refuses loudly rather than guessing:
#
#   * a log whose libtest summary disagrees with the per-test lines;
#   * a log with no per-test lines at all (an empty run is not evidence);
#   * a run whose exit code disagrees with the failures in the log;
#   * a test the coordinates file does not map — an unmapped test gets no
#     coordinate, because a coordinate invented here would be a fabricated cell.
#
# Exit codes: 0 evidence written, 3 the log or the coordinates are inconsistent
# (nothing is written), 2 usage or environment failure.
#
# Contract revision: proven-tests-evidence/1.0.0.

set -euo pipefail

readonly EXIT_OK=0
readonly EXIT_REFUSED=3
readonly EXIT_USAGE=2
readonly COLLECTOR_VERSION="1.0.0"

usage() {
  cat >&2 <<'USAGE'
usage: collect-evidence.sh --repo OWNER/NAME --commit <40-hex> --log FILE
                           --exit-code N --coords FILE
                           --started-at ISO --finished-at ISO
                           (--command-argv '["cargo","test"]' | --command "cargo test")
                           [--runner NAME] [--rust VERSION] [--idris2 VERSION]
                           [--out FILE]
  ISO is YYYY-MM-DDTHH:MM:SSZ (UTC, seconds). Both are required: an invented
  timestamp would be an invented execution.
USAGE
}

die() {
  printf 'collect-evidence: %s\n' "$*" >&2
  exit "$EXIT_USAGE"
}

refuse() {
  printf 'collect-evidence: refused — %s\n' "$*" >&2
  exit "$EXIT_REFUSED"
}

main() {
  local repo="" commit="" log="" exit_code="" coords="" started="" finished=""
  local command="" command_argv="" runner="" rust="" idris2="" out=""

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --repo)         [[ $# -ge 2 ]] || die "--repo needs a value"; repo="$2"; shift 2 ;;
      --commit)       [[ $# -ge 2 ]] || die "--commit needs a value"; commit="$2"; shift 2 ;;
      --log)          [[ $# -ge 2 ]] || die "--log needs a value"; log="$2"; shift 2 ;;
      --exit-code)    [[ $# -ge 2 ]] || die "--exit-code needs a value"; exit_code="$2"; shift 2 ;;
      --coords)       [[ $# -ge 2 ]] || die "--coords needs a value"; coords="$2"; shift 2 ;;
      --started-at)   [[ $# -ge 2 ]] || die "--started-at needs a value"; started="$2"; shift 2 ;;
      --finished-at)  [[ $# -ge 2 ]] || die "--finished-at needs a value"; finished="$2"; shift 2 ;;
      --command)      [[ $# -ge 2 ]] || die "--command needs a value"; command="$2"; shift 2 ;;
      --command-argv) [[ $# -ge 2 ]] || die "--command-argv needs a value"; command_argv="$2"; shift 2 ;;
      --runner)       [[ $# -ge 2 ]] || die "--runner needs a value"; runner="$2"; shift 2 ;;
      --rust)         [[ $# -ge 2 ]] || die "--rust needs a value"; rust="$2"; shift 2 ;;
      --idris2)       [[ $# -ge 2 ]] || die "--idris2 needs a value"; idris2="$2"; shift 2 ;;
      --out)          [[ $# -ge 2 ]] || die "--out needs a value"; out="$2"; shift 2 ;;
      -h|--help)      usage; exit 0 ;;
      *)              usage; die "unknown argument: $1" ;;
    esac
  done

  for required in repo commit log exit_code coords started finished; do
    [[ -n "${!required}" ]] || { usage; die "missing --${required//_/-}"; }
  done
  [[ "$repo" =~ ^[^/[:space:]]+/[^/[:space:]]+$ ]] || die "--repo must be OWNER/NAME"
  [[ "$commit" =~ ^[0-9a-f]{40}$ ]] || die "--commit must be 40 lowercase hex characters"
  [[ "$exit_code" =~ ^[0-9]+$ ]] || die "--exit-code must be a number"
  local iso_re='^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$'
  [[ "$started" =~ $iso_re ]] || die "--started-at must be YYYY-MM-DDTHH:MM:SSZ"
  [[ "$finished" =~ $iso_re ]] || die "--finished-at must be YYYY-MM-DDTHH:MM:SSZ"
  [[ -r "$log" ]] || die "cannot read $log"
  [[ -r "$coords" ]] || die "cannot read $coords"
  command -v jq >/dev/null 2>&1 || die "jq (1.6 or later) is required"
  jq -e . "$coords" >/dev/null 2>&1 || die "$coords is not valid JSON"
  if [[ -n "$command_argv" ]]; then
    jq -e 'type == "array" and length > 0 and all(.[]; type == "string")' <<<"$command_argv" >/dev/null 2>&1 \
      || die "--command-argv must be a non-empty JSON array of strings"
  elif [[ -n "$command" ]]; then
    # Whitespace splitting: documented limitation. Use --command-argv when an
    # argument contains a space.
    command_argv="$(jq -R -s 'split("[[:space:]]+"; "g") | map(select(length > 0))' <<<"$command")"
  else
    die "one of --command-argv or --command is required"
  fi

  # ── parse libtest ───────────────────────────────────────────────────────
  local parsed summary
  parsed="$(awk '/^test .* \.\.\. (ok|FAILED|ignored)$/ {
      name = $0
      sub(/^test /, "", name); sub(/ \.\.\. .*$/, "", name)
      outcome = $0; sub(/^.* \.\.\. /, "", outcome)
      printf "%s\t%s\t%d\n", name, outcome, NR
    }' "$log")"
  [[ -n "$parsed" ]] || refuse "no per-test lines in $log: a run that executed nothing is not evidence"

  summary="$(grep -E '^test result: ' "$log" | tail -n 1 || true)"
  [[ -n "$summary" ]] || refuse "no libtest summary line ('test result: ...') in $log"

  local s_passed s_failed s_ignored
  s_passed="$(sed -nE 's/^test result: (ok|FAILED)\. ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored.*/\2/p' <<<"$summary")"
  s_failed="$(sed -nE 's/^test result: (ok|FAILED)\. ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored.*/\3/p' <<<"$summary")"
  s_ignored="$(sed -nE 's/^test result: (ok|FAILED)\. ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored.*/\4/p' <<<"$summary")"
  [[ -n "$s_passed" && -n "$s_failed" && -n "$s_ignored" ]] || refuse "cannot read the counts from '$summary'"

  local p_passed p_failed p_ignored
  p_passed="$(awk -F'\t' '$2 == "ok"' <<<"$parsed" | wc -l | tr -d ' ')"
  p_failed="$(awk -F'\t' '$2 == "FAILED"' <<<"$parsed" | wc -l | tr -d ' ')"
  p_ignored="$(awk -F'\t' '$2 == "ignored"' <<<"$parsed" | wc -l | tr -d ' ')"

  if [[ "$s_passed" != "$p_passed" || "$s_failed" != "$p_failed" || "$s_ignored" != "$p_ignored" ]]; then
    refuse "the summary says ${s_passed} passed/${s_failed} failed/${s_ignored} ignored, the per-test lines say ${p_passed}/${p_failed}/${p_ignored}"
  fi
  if [[ "$exit_code" != "0" && "$p_failed" == "0" ]]; then
    refuse "exit code ${exit_code} with no failing test: the harness failed outside the tests, so this run evidences nothing about the suite"
  fi
  if [[ "$exit_code" == "0" && "$p_failed" != "0" ]]; then
    refuse "exit code 0 with ${p_failed} failing test(s): the log and the exit code disagree"
  fi
  if [[ "$started" > "$finished" ]]; then
    refuse "--finished-at precedes --started-at"
  fi

  local digest logname
  digest="$( (command -v sha256sum >/dev/null 2>&1 && sha256sum -- "$log") || shasum -a 256 -- "$log")"
  digest="${digest%% *}"
  logname="$(basename "$log")"

  local parsed_json
  parsed_json="$(jq -R -s 'split("\n") | map(select(length > 0) | split("\t") | { name: .[0], outcome: .[1], line: (.[2] | tonumber) })' <<<"$parsed")"

  # ── assemble ────────────────────────────────────────────────────────────
  local doc
  doc="$(jq -n \
    --argjson parsed "$parsed_json" \
    --slurpfile coords "$coords" \
    --arg repo "$repo" --arg commit "$commit" \
    --argjson exit_code "$exit_code" \
    --argjson argv "$command_argv" \
    --arg started "$started" --arg finished "$finished" \
    --arg digest "sha256:${digest}" --arg logname "$logname" \
    --arg runner "$runner" --arg rust "$rust" --arg idris2 "$idris2" \
    --arg version "$COLLECTOR_VERSION" '
    def dur_ms: (((($finished | fromdateiso8601) - ($started | fromdateiso8601)) * 1000) | floor);
    def result_of($o): if $o == "ok" then "passed" elif $o == "FAILED" then "failed" else "skipped" end;
    ( $coords[0].tests // {} ) as $map
    | [ $parsed[]
        | . as $p
        | ($map[$p.name] // null) as $c
        | if $c == null then { unmapped: $p.name }
          else
            { id: $p.name,
              kind: $c.kind,
              coord: $c.coord,
              result: result_of($p.outcome),
              execution: {
                ran: ($p.outcome != "ignored"),
                command: $argv,
                runner: (if $runner == "" then null else $runner end),
                started_at: $started,
                finished_at: $finished,
                duration_ms: dur_ms,
                # Per-test attributed status, not the aggregate: a passing test
                # inside a failing run was still a passing test. The aggregate is
                # `run.exit_code` below, and the gate checks the two agree.
                exit_code: (if $p.outcome == "ok" then 0 elif $p.outcome == "FAILED" then 1 else null end),
                observed_output_digest: $digest
              },
              claim: $c.claim,
              evidence: ($c.evidence // {}),
              fixtures: ($c.fixtures // null),
              anchor: { log: $logname, line: $p.line }
            }
            + (if result_of($p.outcome) == "failed"
                 then { detail: ("libtest reported FAILED for this test; the captured log at line " + ($p.line | tostring) + " is the record") }
               else {} end)
            + (if result_of($p.outcome) == "skipped"
                 then { skip_reason: "libtest: the test carries #[ignore]; it did not run, so it evidences nothing" }
               else {} end)
          end ] as $rows
    | ( [ $rows[] | select(.unmapped?) | .unmapped ] ) as $unmapped
    | ( [ $rows[] | select(.unmapped? | not) ] ) as $tests
    | { contract: { name: "hyperpolymath/proven-tests/evidence", revision: "1.0.0", major: 1 },
        subject: { repo: $repo, commit: $commit,
                   producer: { tool: "proven-tests collect-evidence", version: $version } },
        toolchain: { runner: (if $runner == "" then null else $runner end),
                     rust: (if $rust == "" then null else $rust end),
                     idris2: (if $idris2 == "" then null else $idris2 end) },
        taxonomy: { revision: "proven-tests/taxonomy/1", cells: 238 },
        run: {
          command: $argv,
          runner: (if $runner == "" then null else $runner end),
          started_at: $started,
          finished_at: $finished,
          duration_ms: dur_ms,
          exit_code: $exit_code,
          observed_output_digest: $digest
        },
        tests: $tests,
        counts: {
          records: ($tests | length),
          executed: ([ $tests[] | select(.execution.ran) ] | length),
          passed: ([ $tests[] | select(.result == "passed") ] | length),
          failed: ([ $tests[] | select(.result == "failed") ] | length),
          error: ([ $tests[] | select(.result == "error") ] | length),
          skipped: ([ $tests[] | select(.result == "skipped") ] | length),
          not_run: ([ $tests[] | select(.result == "not_run") ] | length),
          excluded_debt: ([ $tests[] | select((.fixtures.debt_ref? // "") != "") ] | length)
        },
        _unmapped: $unmapped }
  ')" || die "could not assemble the submission (check --command-argv and the coords file)"

  local unmapped
  unmapped="$(jq -r '._unmapped[]?' <<<"$doc")"
  if [[ -n "$unmapped" ]]; then
    printf 'collect-evidence: refused — these executed tests have no coordinate in %s:\n' "$coords" >&2
    printf '  %s\n' $unmapped >&2
    printf 'A coordinate invented by this script would be a fabricated cell. Map them, or do not submit.\n' >&2
    exit "$EXIT_REFUSED"
  fi

  doc="$(jq 'del(._unmapped)' <<<"$doc")"
  if [[ -n "$out" ]]; then
    printf '%s\n' "$doc" > "$out"
    printf 'collect-evidence: wrote %s (%s tests, %s executed)\n' \
      "$out" "$(jq -r '.counts.records' <<<"$doc")" "$(jq -r '.counts.executed' <<<"$doc")" >&2
  else
    printf '%s\n' "$doc"
  fi
  exit "$EXIT_OK"
}

main "$@"
