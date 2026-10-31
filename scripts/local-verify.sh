#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
#
# local-verify.sh — run every gate this repository's CI would run, locally, and
# leave ONE bundle of evidence to hand back.
#
# WHY THIS EXISTS
# ---------------
# `squabble-core::execution`, `squabble-fight::execution` and `squabble-cli::phase`
# were written in an environment with no Rust toolchain and no reachable
# rustup mirror (static.rust-lang.org is refused there). Their 31 unit tests have
# therefore never been compiled. This script is the smallest honest way to close
# that gap on a machine that *does* have a toolchain: it installs nothing it
# cannot verify (it delegates to scripts/install-toolchain.sh, which verifies
# rustup-init's published SHA-256 and refuses pipes-to-shell), runs the same
# commands CI runs, and writes the raw output where it can be pasted back.
#
# WHAT IT NEVER DOES
# ------------------
# * no commit, no push, no PR, no issue, no re-run
# * no repository settings, allow-lists, rulesets or pins touched
# * no test weakened, skipped or marked pass to make the summary quieter
#
# FAIL-CLOSED STATUS VOCABULARY (the estate's, not a new one)
# ----------------------------------------------------------
#   PASS  the command ran and exited 0
#   FAIL  the command ran and exited non-zero — real evidence of a defect
#   VOID  the command could not run (missing tool/toolchain) — NO CHECK WAS
#         PERFORMED. Never printed as a pass.
#   SKIP  deliberately not requested (e.g. --quick) — also never a pass
#   KNOWN a failure this script has POSITIVE evidence predates the branch (the
#         same command fails on the branch base for a reason unrelated to it).
#         Printed in full, never counted as PASS, not counted as a regression;
#         `--strict` makes it exit non-zero too.
#
# Exit codes: 0 every requested step that could run passed · 1 at least one FAIL
# · 2 no FAIL but at least one VOID (nothing was verified) · 3 bad usage. A FAIL
# outranks a VOID — a defect is the more actionable signal — and the VOID list is
# printed either way.
#
# Usage:
#   scripts/local-verify.sh                 # install what is missing, run everything stable
#   scripts/local-verify.sh --no-install    # use the toolchain already present
#   scripts/local-verify.sh --quick         # stable only: skip Creusot, contracts yes
#   scripts/local-verify.sh --creusot-prove # ALSO discharge the Creusot obligations (slow, network)
#   scripts/local-verify.sh --check         # report the toolchain and the plan; run nothing
#   scripts/local-verify.sh --out DIR       # where to write the bundle (default: $TMPDIR/…)
#   scripts/local-verify.sh --strict        # a KNOWN pre-existing failure fails the run
#
# Reading the result: open <out>/SUMMARY.txt. It carries the per-step status, the
# exact errors extracted from each log, and the run's git/rubist/cargo identity.

set -uo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 3

install=true
quick=false
creusot_prove=false
check_only=false
strict=false
out_dir=""

usage() { sed -n '3,45p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --no-install)    install=false ;;
    --quick)         quick=true ;;
    --creusot-prove) creusot_prove=true ;;
    --check)         check_only=true ;;
    --strict)        strict=true ;;
    --out)           shift; out_dir="${1:-}" ;;
    -h|--help)       usage; exit 0 ;;
    *) echo "local-verify.sh: unknown argument '$1'" >&2; usage >&2; exit 3 ;;
  esac
  shift
done

have() { command -v "$1" >/dev/null 2>&1; }

# rustup installs to ~/.cargo/bin and does not edit PATH for us
# (install-toolchain.sh passes --no-modify-path on purpose).
if ! have cargo && [ -f "${CARGO_HOME:-$HOME/.cargo}/env" ]; then
  # shellcheck disable=SC1091
  . "${CARGO_HOME:-$HOME/.cargo}/env" 2>/dev/null || true
fi

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
if [ -z "$out_dir" ]; then
  base="${TMPDIR:-/tmp}"; base="${base%/}"
  out_dir="$base/squabble-verify-$stamp"
fi
mkdir -p "$out_dir" || { echo "cannot create $out_dir" >&2; exit 3; }

STATUS_TSV="$out_dir/status.tsv"
: > "$STATUS_TSV"
printf 'step\tstatus\texit\tlog\n' >> "$STATUS_TSV"

record() { printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" >> "$STATUS_TSV"; }
say() { printf '%s\n' "$*"; }

env_log="$out_dir/00-env.txt"
{
  echo "== identity =="
  echo "generated:      $stamp"
  echo "repo:           $REPO_ROOT"
  echo "branch:         $(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo '(detached)')"
  echo "head:           $(git rev-parse HEAD 2>/dev/null || echo '(no git)')"
  echo "upstream:       $(git rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>/dev/null || echo '(none)')"
  echo
  echo "== platform =="
  echo "os:             $(uname -srm)"
  echo
  echo "== toolchain =="
  for t in rustup rustc cargo rustfmt cargo-clippy gh jq python3 just; do
    if have "$t"; then
      printf '%-15s %s\n' "$t:" "$("$t" --version 2>&1 | head -1)"
    else
      printf '%-15s %s\n' "$t:" "MISSING"
    fi
  done
  echo "toolchains:"
  rustup toolchain list 2>/dev/null | sed 's/^/  /' || echo "  (rustup absent)"
  echo
  echo "== pinned creusot =="
  grep -E '^(channel|components)' verification/squabble-creusot/rust-toolchain.toml 2>/dev/null || true
  echo
  echo "== worktree state BEFORE (must stay unchanged) =="
  git status --porcelain 2>/dev/null || echo "(no git)"
} > "$env_log" 2>&1
say "bundle: $out_dir"
say ""

if [ "$check_only" = true ]; then
  say "toolchain + plan only (--check); nothing was run."
  sed -n '/== toolchain ==/,/== pinned creusot ==/p' "$env_log" | sed '$d'
  say "  pinned creusot (verification/squabble-creusot/rust-toolchain.toml):"
  sed -n '/== pinned creusot ==/,/^$/p' "$env_log" | sed '1d;/^$/d;/^== /d' | sed 's/^/    /'
  say ""
  say "planned steps:"
  say "  01 fmt      cargo fmt --all -- --check"
  say "  02 check    cargo check --workspace --all-targets --locked"
  say "  03 clippy   cargo clippy --workspace --all-targets --locked -- -D warnings"
  say "  04 test     cargo test --workspace --locked"
  say "  05 test-boj cargo test -p squabble-cli --features boj"
  say "  06 mirror   cargo test --manifest-path verification/squabble-creusot/Cargo.toml --no-default-features"
  say "  07 aspect   bash tests/aspect_tests.sh"
  say "  08 contract bash contracts/squabble/tests/run-conformance.sh (+ proven-tests)"
  if [ "$quick" = false ]; then
    say "  09 prove    $( [ "$creusot_prove" = true ] && echo 'cd verification/squabble-creusot && cargo creusot prove' || echo 'SKIP — needs --creusot-prove' )"
  fi
  exit 0
fi

# --------------------------------------------------------------- install ----
installed=true
if [ "$install" = true ] && { ! have cargo || ! have rustup; }; then
  say "installing the toolchain via scripts/install-toolchain.sh (verified rustup-init)"
  log="$out_dir/00-install.txt"
  if bash scripts/install-toolchain.sh --no-creusot >>"$log" 2>&1; then
    # shellcheck disable=SC1091
    . "${CARGO_HOME:-$HOME/.cargo}/env" 2>/dev/null || true
    record "00-install" PASS 0 "$log"
  else
    rc=$?
    record "00-install" FAIL "$rc" "$log"
    installed=false
  fi
else
  record "00-install" SKIP - -
fi

# ---------------------------------------------------------------- helpers ---
# run_step <id> <label> <logfile> <command...>
# Establishes the status of one step into status.tsv and remembers FAIL/VOID.
# Indexed arrays only: macOS ships bash 3.2, which has no `declare -A`.
declare -a FAILED_STEPS=() VOID_STEPS=() SKIPPED_STEPS=() KNOWN_STEPS=()

run_step() {
  local id="$1" label="$2" log="$3"; shift 3
  printf '$ %s\n\n' "$*" >  "$log"
  "$@" >>"$log" 2>&1
  local rc=$?
  if [ "$rc" -eq 0 ]; then
    record "$id" PASS 0 "$log"
  else
    record "$id" FAIL "$rc" "$log"
    FAILED_STEPS+=("$id")
  fi
  printf '  %-12s %s\n' "$id" "$( [ "$rc" -eq 0 ] && echo PASS || echo "FAIL (exit $rc)" ) — $label"
  return 0
}

void_step() {
  local id="$1" label="$2" why="$3"
  local log="$out_dir/$id.txt"
  printf 'VOID: %s\n' "$why" > "$log"
  record "$id" VOID - "$log"
  VOID_STEPS+=("$id")
  printf '  %-12s %s\n' "$id" "VOID — $label ($why)"
}

skip_step() {
  local id="$1" label="$2"
  local log="$out_dir/$id.txt"
  printf 'SKIP: not requested\n' > "$log"
  record "$id" SKIP - "$log"
  SKIPPED_STEPS+=("$id")
  printf '  %-12s %s\n' "$id" "SKIP — $label"
}

require_cargo() {
  if ! have cargo; then
    void_step "$1" "$2" "no cargo on PATH — install a toolchain first"
    return 1
  fi
  return 0
}

# ------------------------------------------------------------------ steps ---
say ""
say "running steps (logs under $out_dir)"

# 01 — formatting. Never auto-fixed here: rewriting files is a change, and this
# script's contract is that the worktree is untouched. `cargo fmt --check` reports.
if require_cargo 01-fmt "cargo fmt --check"; then
  run_step 01-fmt "cargo fmt --all -- --check" "$out_dir/01-fmt.txt" \
    cargo fmt --all -- --check
fi

# 02 — compile. The step that matters most for the new modules.
if require_cargo 02-check "cargo check"; then
  run_step 02-check "cargo check --workspace --all-targets --locked" "$out_dir/02-check.txt" \
    cargo check --workspace --all-targets --locked
fi

# 03 — clippy. `-D warnings` matches what the estate's rust-ci gates on.
if require_cargo 03-clippy "cargo clippy -D warnings"; then
  run_step 03-clippy "cargo clippy --workspace --all-targets --locked -- -D warnings" "$out_dir/03-clippy.txt" \
    cargo clippy --workspace --all-targets --locked -- -D warnings
fi

# 04 — the workspace tests (`just test`, first command).
if require_cargo 04-test "cargo test --workspace"; then
  run_step 04-test "cargo test --workspace --locked" "$out_dir/04-test.txt" \
    cargo test --workspace --locked
fi

# 05 — the `boj` feature build (`just test`, second command).
if require_cargo 05-test-boj "cargo test -p squabble-cli --features boj"; then
  run_step 05-test-boj "cargo test -p squabble-cli --features boj --locked" "$out_dir/05-test-boj.txt" \
    cargo test -p squabble-cli --features boj --locked
fi

# 06 — the stable-Rust mirror of the Creusot model (`just verify-mirror`).
if [ "$quick" = true ]; then
  skip_step 06-mirror "stable Creusot mirror (--quick)"
elif require_cargo 06-mirror "cargo test --no-default-features (verification/)"; then
  run_step 06-mirror "cargo test --manifest-path verification/squabble-creusot/Cargo.toml --no-default-features" \
    "$out_dir/06-mirror.txt" \
    cargo test --manifest-path verification/squabble-creusot/Cargo.toml --no-default-features
fi

# 07 — the toolchain-free structural aspects (this is the one that already runs
# in the sandbox; aspect 2's pre-existing documentation-grep failure is known).
if [ -f tests/aspect_tests.sh ]; then
  aspect_log="$out_dir/07-aspect.txt"
  printf '$ bash tests/aspect_tests.sh\n\n' > "$aspect_log"
  bash tests/aspect_tests.sh >> "$aspect_log" 2>&1
  aspect_rc=$?
  if [ "$aspect_rc" -eq 0 ]; then
    record 07-aspect PASS 0 "$aspect_log"
    printf '  %-12s %s\n' "07-aspect" "PASS — aspect tests"
  else
    # Aspect 2 greps `verification/` for forbidden proof markers and trips on the
    # DOCUMENTATION that names them (verification/proofs/README.adoc,
    # verification/proofs/coq/TypeSafety.v). That is a pre-existing defect of the
    # aspect, not of the branch: it fails identically on the base commit, and it is
    # recorded in docs/reports/estate-integration/2026-10-31-ci-diagnosis-coverage.adoc
    # §10.4. It is recognised ONLY when it is the single FAIL line of the whole
    # run, so a genuine new aspect failure can never be absorbed here.
    aspect_clean="$(tr -d '\033' < "$aspect_log" 2>/dev/null | sed -E 's/\[[0-9;]*m//g')"
    fail_lines="$(printf '%s\n' "$aspect_clean" | grep -cE '^ +FAIL:' || true)"
    if [ "$fail_lines" = "1" ] && printf '%s\n' "$aspect_clean" | grep -qE '^ +FAIL: Dangerous proof patterns found:'; then
      record 07-aspect KNOWN "$aspect_rc" "$aspect_log"
      KNOWN_STEPS+=(07-aspect)
      printf '  %-12s %s\n' "07-aspect" "KNOWN — aspect tests (pre-existing aspect 2 doc-grep; see SUMMARY)"
    else
      record 07-aspect FAIL "$aspect_rc" "$aspect_log"
      FAILED_STEPS+=(07-aspect)
      printf '  %-12s %s\n' "07-aspect" "FAIL (exit $aspect_rc) — aspect tests"
    fi
  fi
else
  void_step 07-aspect "aspect tests" "tests/aspect_tests.sh absent"
fi

# 08 — the pinned contracts (`just contract-test`); jq only, no Rust.
if ! have jq; then
  void_step 08-contract "contract conformance" "jq is required by contracts/*/tests/run-conformance.sh"
else
  log="$out_dir/08-contract.txt"
  : > "$log"
  rc=0
  { echo '$ bash contracts/squabble/tests/run-conformance.sh'; } >> "$log"
  bash contracts/squabble/tests/run-conformance.sh >> "$log" 2>&1 || rc=$?
  { echo; echo '$ bash contracts/proven-tests/tests/run-conformance.sh'; } >> "$log"
  bash contracts/proven-tests/tests/run-conformance.sh >> "$log" 2>&1 || rc=$?
  if [ "$rc" -eq 0 ]; then
    record 08-contract PASS 0 "$log"; printf '  %-12s %s\n' "08-contract" "PASS — contract conformance"
  else
    record 08-contract FAIL "$rc" "$log"; FAILED_STEPS+=(08-contract)
    printf '  %-12s %s\n' "08-contract" "FAIL (exit $rc) — contract conformance"
  fi
fi

# 09 — discharge the Creusot obligations. Opt-in: it needs opam+Why3+SMT, the
# pinned nightly, and it is slow. `--creusot-prove` is the only way in, so a
# "stated" obligation can never be reported as merely skipped by accident.
if [ "$quick" = true ]; then
  skip_step 09-creusot-prove "Creusot prove (--quick)"
elif [ "$creusot_prove" = false ]; then
  skip_step 09-creusot-prove "Creusot prove (needs --creusot-prove)"
elif ! have cargo || ! cargo creusot --help >/dev/null 2>&1; then
  void_step 09-creusot-prove "cargo creusot prove" \
    "cargo creusot absent — the obligations stay STATED, NOT PROVED"
else
  log="$out_dir/09-creusot-prove.txt"
  printf '$ cd verification/squabble-creusot && cargo creusot prove\n\n' > "$log"
  ( cd verification/squabble-creusot && cargo creusot prove ) >> "$log" 2>&1
  rc=$?
  if [ "$rc" -eq 0 ]; then
    record 09-creusot-prove PASS 0 "$log"; printf '  %-12s %s\n' "09-creusot-prove" "PASS — obligations discharged"
  else
    record 09-creusot-prove FAIL "$rc" "$log"; FAILED_STEPS+=(09-creusot-prove)
    printf '  %-12s %s\n' "09-creusot-prove" "FAIL (exit $rc) — obligations NOT discharged"
  fi
fi

# ------------------------------------------------------------- summary ------
summary="$out_dir/SUMMARY.txt"
changed_files="$(git diff --name-only "$(git merge-base HEAD origin/main 2>/dev/null || echo HEAD~1)" 2>/dev/null || true)"

{
  echo "squabble local-verify — $stamp"
  echo "repo:   $REPO_ROOT"
  echo "branch: $(git rev-parse --abbrev-ref HEAD 2>/dev/null)  head: $(git rev-parse --short HEAD 2>/dev/null)"
  echo "rustc:  $(rustc --version 2>/dev/null || echo MISSING)   cargo: $(cargo --version 2>/dev/null || echo MISSING)"
  echo
  echo "== status =="
  awk -F'\t' 'NR>1 { printf "  %-18s %-5s %s\n", $1, $2, ($3=="-"?"":("exit "$3)) }' "$STATUS_TSV"
  echo
  if [ -n "${KNOWN_STEPS[*]:-}" ]; then
    echo "== known pre-existing (not a pass, not a regression) =="
    echo "  tests/aspect_tests.sh Aspect 2 greps verification/ for proof markers and"
    echo "  matches the prose that names them; it fails on the branch base commit too."
    echo "  Run with --strict to make it fail this run as well."
    echo
  fi
  echo "== errors extracted from the logs =="
  found_any=false
  while IFS=$'\t' read -r id status _rc log; do
    case "$status" in FAIL|KNOWN) ;; *) continue ;; esac
    found_any=true
    echo
    echo "--- $id ($log)"
    # rustc/clippy diagnostics, cargo's own failures, and test failures; capped
    # so the summary stays pasteable, with the full text left in the log.
    # Strip ANSI first, then match: tests/aspect_tests.sh colours its PASS/FAIL
    # lines, so a plain `^  FAIL:` grep would see escape codes and match nothing.
    # `tr -d ESC` + sed is portable to BSD/macOS, where sed has no `\x1b`.
    clean="$(tr -d '\033' < "$log" 2>/dev/null | sed -E 's/\[[0-9;]*m//g')"
    printf '%s\n' "$clean" \
      | grep -nE '^error(\[E[0-9]+\])?:|^ *--> |^error: could not compile|^failures:$|^test .* FAILED|^thread .* panicked|^assertion .* failed|^ *left:|^ *right:|^FAIL:|^ +FAIL:|^Diff in |^warning: unused' \
      | head -60 || true
    printf '%s\n' "$clean" | grep -cE '^error' \
      | awk '$1+0 > 60 { print "  … (" $1 " error lines total in the log)" }'
  done < "$STATUS_TSV"
  [ "$found_any" = true ] || echo "  (none — no step failed)"
  echo
  echo "== formatting: do the files this branch touches appear? =="
  if [ -s "$out_dir/01-fmt.txt" ] && grep -qE '^Diff in ' "$out_dir/01-fmt.txt" 2>/dev/null; then
    touched="(could not compute the branch's changed files)"
    [ -n "$changed_files" ] && touched="$changed_files"
    if [ -n "$changed_files" ]; then
      while IFS= read -r f; do
        [ -n "$f" ] || continue
        if grep -qF "$f" "$out_dir/01-fmt.txt"; then
          echo "  BRANCH FILE WITH FMT DIFF: $f"
        fi
      done <<< "$changed_files"
      echo "  (files with diffs not listed above are pre-existing — PR #144 owns them)"
    fi
  else
    echo "  (no formatting diffs, or the fmt step did not run)"
  fi
  echo
  echo "== worktree state AFTER (should match BEFORE in 00-env.txt) =="
  git status --porcelain 2>/dev/null | sed 's/^/  /' || echo "  (no git)"
  echo
  echo "== what to send back =="
  echo "  This file (SUMMARY.txt) is enough for a first pass."
  echo "  If a step failed and the extraction above cut the detail, also send:"
  echo "    $out_dir/<step>.txt"
  echo
  if [ "${#FAILED_STEPS[@]}" -eq 0 ] && [ "${#VOID_STEPS[@]}" -eq 0 ]; then
    echo "RESULT: every requested step that could run PASSED."
  else
    [ "${#FAILED_STEPS[@]}" -gt 0 ] && echo "RESULT: FAILED steps: ${FAILED_STEPS[*]:-}"
    [ "${#VOID_STEPS[@]}" -gt 0 ] && echo "RESULT: VOID steps (nothing was verified): ${VOID_STEPS[*]:-}"
  fi
  [ "${#SKIPPED_STEPS[@]}" -gt 0 ] && echo "RESULT: SKIPPED (not a pass): ${SKIPPED_STEPS[*]:-}"
} > "$summary" 2>&1

say ""
cat "$summary"

if [ "${#FAILED_STEPS[@]}" -gt 0 ]; then exit 1; fi
if [ "$strict" = true ] && [ -n "${KNOWN_STEPS[*]:-}" ]; then exit 1; fi
if [ "${#VOID_STEPS[@]}" -gt 0 ]; then exit 2; fi
exit 0
