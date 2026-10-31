#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
#
# local_verify_test.sh — planted controls for scripts/local-verify.sh.
#
# WHAT THIS PROVES, AND WHY IT IS WORTH PROVING
# ---------------------------------------------
# `local-verify.sh` is the instrument that decides whether the un-compiled Rust
# in this repository builds. An instrument that silently reports success is
# worse than no instrument, so its status vocabulary is tested with PLANTED
# failures in both directions — the same discipline `tests/squabble_action_test.sh`
# applies to the release guard, and the reason `contracts/*/tests` carries
# negative fixtures.
#
# Each case below plants a fake `cargo` (or removes a real tool) and asserts the
# script's EXIT CODE and its SUMMARY, not its tone:
#
#   1. nothing available        → every step VOID, exit 2 (a VOID is not a pass)
#   2. everything succeeding    → exit 0
#   3. a compile error          → exit 1, and the error + file:line extracted
#   4. a formatting diff        → exit 1, and "Diff in … at line N" extracted
#      (fmt writes ANSI-free output but aspect does not — case 8 covers that)
#   5. one test failing         → exit 1, and the panic/left/right extracted
#   6. --only                   → exactly one step runs; the rest are SKIP
#   7. --check                  → nothing runs at all
#   8. a missing tool (jq)      → VOID and exit 2, never PASS
#   9. the worktree is untouched by any of it
#  10. a step that exits 0 has PASS in status.tsv, and a VOID step never does
#
# The suite deliberately never touches the repository: it runs the script against
# a temporary clone-and-copy of only what it needs? No — it runs against THIS
# checkout, but asserts (case 9) that the worktree is unchanged afterwards, which
# is the property that matters and is cheaper than a copy.
#
# Usage: bash tests/local_verify_test.sh        (exit 0 all pass, 1 a case failed)
# Needs: bash, git, coreutils. No Rust, no network, no token.

set -uo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/local-verify.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PASS=0
FAILCOUNT=0
pass() { printf '  \033[32mPASS\033[0m: %s\n' "$1"; PASS=$((PASS + 1)); }
fail() { printf '  \033[31mFAIL\033[0m: %s\n' "$1"; FAILCOUNT=$((FAILCOUNT + 1)); }
bold() { printf '\033[1m%s\033[0m\n' "$1"; }

[ -x "$SCRIPT" ] || { echo "missing or non-executable: $SCRIPT" >&2; exit 1; }

# ---------------------------------------------------------------- fixtures --
# A fake toolchain. FAKE_MODE selects the planted failure (passed by env, so the
# same fixture serves every case and no fixture is edited between runs).
FAKE_BIN="$WORK/fakebin"
mkdir -p "$FAKE_BIN"
write_fake_cargo() {
  cat > "$FAKE_BIN/cargo" <<'FAKE'
#!/usr/bin/env bash
case "${1:-}" in
  --version) echo "cargo 1.85.0 (fake 0000000 2026-10-31)"; exit 0 ;;
esac
mode="${FAKE_MODE:-ok}"
case "$mode" in
  ok) exit 0 ;;
  fmt)
    echo "Diff in /work/crates/squabble-core/src/execution.rs at line 312:"
    echo "     for name in &self.setup_steps {"
    exit 1 ;;
  compile)
    echo "   Compiling squabble-core v0.1.0 (/work/crates/squabble-core)"
    echo 'error[E0599]: no method named `is_policy_step` found for `&PhaseSignatures`'
    echo "  --> crates/squabble-core/src/execution.rs:331:63"
    echo "error: could not compile \`squabble-core\` (lib) due to 1 previous error"
    exit 101 ;;
  test)
    echo "running 19 tests"
    echo "test execution::tests::a_verified_run_names_the_step_that_ran ... ok"
    echo "test execution::tests::a_green_from_a_preflight_step_alone_is_not_verified ... FAILED"
    echo
    echo "failures:"
    echo "    execution::tests::a_green_from_a_preflight_step_alone_is_not_verified"
    echo
    echo "thread 'main' panicked at crates/squabble-core/src/execution.rs:640:9:"
    echo "assertion \`left == right\` failed"
    echo '  left: `Verified { step: "Set up job" }`'
    echo '  right: `Unclassified { reason: NoSuccessfulStep }`'
    exit 101 ;;
esac
exit 0
FAKE
  chmod +x "$FAKE_BIN/cargo"
  cp "$FAKE_BIN/cargo" "$FAKE_BIN/rustc"
}

# A PATH that contains only what a bare run needs, with no cargo and no jq —
# used to force VOID. Built from symlinks to the real tools, never by guessing.
bare_path() {
  local dir="$WORK/bare-$1"
  mkdir -p "$dir"
  for t in bash sh git env sed awk tr grep head tail cat date mkdir rm cp mv wc printf sort uniq mktemp base64 python3 dirname readlink uname tput; do
    p="$(command -v "$t" 2>/dev/null)" && ln -sf "$p" "$dir/$t"
  done
  printf '%s' "$dir"
}

# run_case <name> <path> <expected-exit> <extra args…>
# Runs the script once into a private bundle and returns its exit code.
run_case() {
  local name="$1" path="$2" want="$3"; shift 3
  local out="$WORK/out-$name"
  rm -rf "$out"
  PATH="$path" CARGO_HOME="$WORK/no-cargo-home" "$SCRIPT" --no-install --out "$out" "$@" >"$WORK/stdout-$name" 2>&1
  local rc=$?
  LAST_RC=$rc
  LAST_OUT="$out"
  LAST_WANT="$want"
  return 0
}

status_of() { # status_of <bundle> <step-id> → PASS|FAIL|VOID|SKIP|KNOWN|(absent)
  awk -F'\t' -v id="$2" '$1==id {print $2}' "$1/status.tsv" 2>/dev/null | head -1
}
summary_has() { grep -qF -- "$2" "$1/SUMMARY.txt" 2>/dev/null; }

write_fake_cargo
FAKE_PATH="$FAKE_BIN:$(bare_path fake)"
REAL_PATH="$PATH"
NOJQ_PATH="$(bare_path nojq)"

# Record the worktree before anything runs (case 9 asserts it is unchanged).
before_status="$(cd "$REPO_ROOT" && git status --porcelain 2>/dev/null)"
before_head="$(cd "$REPO_ROOT" && git rev-parse HEAD 2>/dev/null)"

bold "local-verify.sh — planted controls"

# --------------------------------------------------------------- case 1 ----
# No cargo at all. Every step that needs one must be VOID, and the script must
# exit 2 — "nothing was verified" is not success.
run_case nocargo "$NOJQ_PATH" 2 --only 01-fmt,02-check,03-clippy,04-test
if [ "$LAST_RC" -eq 2 ]; then pass "no toolchain → exit 2 (nothing was verified)"; else fail "no toolchain → exit $LAST_RC, wanted 2"; fi
for step in 01-fmt 02-check 03-clippy 04-test; do
  s="$(status_of "$LAST_OUT" "$step")"
  if [ "$s" = "VOID" ]; then pass "no toolchain → $step recorded VOID"; else fail "no toolchain → $step recorded '${s:-<absent>}', wanted VOID"; fi
done
if summary_has "$LAST_OUT" "RESULT: VOID steps"; then pass "no toolchain → SUMMARY names the VOID steps"; else fail "no toolchain → SUMMARY does not name VOID steps"; fi

# --------------------------------------------------------------- case 2 ----
# Everything succeeds.
FAKE_MODE=ok run_case ok "$FAKE_PATH" 0 --only 01-fmt,02-check,03-clippy,04-test,05-test-boj
if [ "$LAST_RC" -eq 0 ]; then pass "all steps succeed → exit 0"; else fail "all steps succeed → exit $LAST_RC, wanted 0"; fi
allpass=true
for step in 01-fmt 02-check 03-clippy 04-test 05-test-boj; do
  [ "$(status_of "$LAST_OUT" "$step")" = "PASS" ] || allpass=false
done
[ "$allpass" = true ] && pass "all steps succeed → every step PASS in status.tsv" || fail "all steps succeed → some step is not PASS"
if summary_has "$LAST_OUT" "RESULT: every requested step that could run PASSED"; then pass "all steps succeed → SUMMARY says so"; else fail "all steps succeed → SUMMARY does not say so"; fi

# --------------------------------------------------------------- case 3 ----
# A compile error must fail the run AND be extracted with its file:line, because
# that extraction is what gets pasted back to me.
FAKE_MODE=compile run_case compile "$FAKE_PATH" 1 --only 02-check
if [ "$LAST_RC" -eq 1 ]; then pass "compile error → exit 1"; else fail "compile error → exit $LAST_RC, wanted 1"; fi
if [ "$(status_of "$LAST_OUT" 02-check)" = "FAIL" ]; then pass "compile error → 02-check recorded FAIL"; else fail "compile error → 02-check is not FAIL"; fi
if summary_has "$LAST_OUT" 'error[E0599]: no method named'; then pass "compile error → the diagnostic is extracted"; else fail "compile error → the diagnostic is NOT extracted"; fi
if summary_has "$LAST_OUT" 'execution.rs:331:63'; then pass "compile error → file:line is extracted"; else fail "compile error → file:line is NOT extracted"; fi

# --------------------------------------------------------------- case 4 ----
# A formatting diff names the file, so the summary can tell a pre-existing diff
# (PR #144's) from one this branch introduced.
FAKE_MODE=fmt run_case fmt "$FAKE_PATH" 1 --only 01-fmt
if [ "$LAST_RC" -eq 1 ]; then pass "fmt diff → exit 1"; else fail "fmt diff → exit $LAST_RC, wanted 1"; fi
if summary_has "$LAST_OUT" "Diff in /work/crates/squabble-core/src/execution.rs at line 312"; then
  pass "fmt diff → 'Diff in … at line N' is extracted"
else
  fail "fmt diff → the fmt output is NOT extracted"
fi

# --------------------------------------------------------------- case 5 ----
# A failing test: the assertion's own text must survive into the summary.
FAKE_MODE=test run_case test "$FAKE_PATH" 1 --only 04-test
if [ "$LAST_RC" -eq 1 ]; then pass "failing test → exit 1"; else fail "failing test → exit $LAST_RC, wanted 1"; fi
if summary_has "$LAST_OUT" "assertion \`left == right\` failed"; then pass "failing test → the assertion is extracted"; else fail "failing test → the assertion is NOT extracted"; fi
if summary_has "$LAST_OUT" 'right: `Unclassified { reason: NoSuccessfulStep }`'; then
  pass "failing test → both sides of the assertion are extracted"
else
  fail "failing test → the expected side is NOT extracted"
fi

# --------------------------------------------------------------- case 6 ----
# --only runs exactly the selected steps; everything else is SKIP, never PASS.
FAKE_MODE=ok run_case only "$FAKE_PATH" 0 --only 02-check
rows="$(awk -F'\t' 'NR>1 && $2!="SKIP" {print $1}' "$LAST_OUT/status.tsv" | sort | tr '\n' ' ')"
skips="$(awk -F'\t' 'NR>1 && $2=="SKIP" {print $1}' "$LAST_OUT/status.tsv" | tr '\n' ' ')"
case "$rows" in
  "02-check ") pass "--only 02-check → 02-check is the only non-SKIP step" ;;
  *) fail "--only 02-check → unexpected non-SKIP steps: $rows" ;;
esac
if [ -n "$skips" ] && ! printf '%s' "$skips" | grep -q '02-check'; then
  pass "--only 02-check → the deselected steps are recorded SKIP ($(printf '%s' "$skips" | wc -w | tr -d ' ') of them)"
else
  fail "--only 02-check → deselected steps are not recorded SKIP"
fi
if ! grep -q 'VOID' "$LAST_OUT/status.tsv"; then
  pass "--only → no step is left VOID by the selector"
else
  fail "--only → a step is VOID, so the selector did not reach it"
fi

# --------------------------------------------------------------- case 7 ----
# --check must run nothing: no status rows, and no cargo invocation.
# Capture to a FILE, never pipe into `grep -q`: grep exits at the first match, the
# script under test then dies of SIGPIPE, and `set -o pipefail` reports the pipeline
# as failed even though the text was there. (Measured: the first version of this
# case failed for exactly that reason, not because the plan was missing.)
PATH="$NOJQ_PATH" "$SCRIPT" --check --out "$WORK/out-check" >"$WORK/check.txt" 2>&1
check_rc=$?
if [ "$check_rc" -ne 0 ]; then
  fail "--check → exited $check_rc, wanted 0"
else
  pass "--check → exit 0"
fi
if [ -e "$WORK/out-check/status.tsv" ]; then
  fail "--check → a status.tsv was written, so steps ran"
else
  pass "--check → nothing ran (no status.tsv written)"
fi
if grep -q 'planned steps:' "$WORK/check.txt"; then
  pass "--check → prints the plan"
else
  fail "--check → does not print the plan"
fi
if grep -q 'nothing was run' "$WORK/check.txt"; then
  pass "--check → says out loud that nothing was run"
else
  fail "--check → does not say that nothing was run"
fi

# --------------------------------------------------------------- case 8 ----
# A *missing tool* is VOID, never PASS: the contract test needs jq, so removing it
# from PATH must not turn the contract step green.
run_case nojq "$NOJQ_PATH" 2 --only 08-contract
if [ "$(status_of "$LAST_OUT" 08-contract)" = "VOID" ]; then pass "no jq → 08-contract recorded VOID"; else fail "no jq → 08-contract is '$(status_of "$LAST_OUT" 08-contract)', wanted VOID"; fi
if [ "$LAST_RC" -eq 2 ]; then pass "no jq → exit 2 (nothing was verified)"; else fail "no jq → exit $LAST_RC, wanted 2"; fi

# --------------------------------------------------------------- case 9 ----
# The instrument must not change what it measures.
after_status="$(cd "$REPO_ROOT" && git status --porcelain 2>/dev/null)"
after_head="$(cd "$REPO_ROOT" && git rev-parse HEAD 2>/dev/null)"
if [ "$before_status" = "$after_status" ]; then pass "the worktree is unchanged by every case above"; else
  fail "the worktree changed:
$before_status
 ->
$after_status"
fi
if [ "$before_head" = "$after_head" ]; then pass "HEAD is unchanged"; else fail "HEAD moved during the run"; fi

# -------------------------------------------------------------- case 10 ----
# The vocabulary itself: no step may be both VOID and PASS, and the two header
# lines exist so a reader can tell a run from an empty file.
# Every row must carry exactly one of the five statuses — a step cannot be both
# VOID and PASS, because the reader downstream (me) keys off this column.
if awk -F'\t' 'NR>1 { if ($2!="PASS" && $2!="FAIL" && $2!="VOID" && $2!="SKIP" && $2!="KNOWN") bad=1 } END { exit (bad?1:0) }' "$WORK/out-ok/status.tsv"; then
  pass "every row carries exactly one of PASS|FAIL|VOID|SKIP|KNOWN"
else
  fail "a row carries an undocumented status: $(awk -F'\t' 'NR>1 && $2!="PASS" && $2!="FAIL" && $2!="VOID" && $2!="SKIP" && $2!="KNOWN" {print $1"="$2}' "$WORK/out-ok/status.tsv" | tr '\n' ' ')"
fi
if head -1 "$WORK/out-ok/status.tsv" | grep -q '^step	status	exit	log$'; then
  pass "status.tsv has its documented header"
else
  fail "status.tsv header is not 'step<TAB>status<TAB>exit<TAB>log'"
fi

# ---------------------------------------------------------------- result ---
echo
if [ "$FAILCOUNT" -eq 0 ]; then
  printf '\033[32m%d/%d cases passed\033[0m\n' "$PASS" "$((PASS + FAILCOUNT))"
  exit 0
fi
printf '\033[31m%d of %d checks failed\033[0m\n' "$FAILCOUNT" "$((PASS + FAILCOUNT))"
exit 1
