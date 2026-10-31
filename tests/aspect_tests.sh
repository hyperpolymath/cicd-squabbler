#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
#
# RSR Standard Aspect Test Template
#
# Aspect tests validate cross-cutting architectural invariants that span
# the entire codebase. These are NOT functional tests — they verify that
# coding standards, safety rules, and structural contracts hold.
#
# Usage:
#   bash tests/aspect_tests.sh
#   just aspect
#
# Standard aspects (enable what applies to your project):
#   1. SPDX compliance — all source files have license headers
#   2. Dangerous patterns — no believe_me, assert_total, sorry, unsafeCoerce, etc.
#   3. ABI/FFI contract — declarations match exports
#   4. Thread safety — mutex in FFI modules
#   5. Error handling — no panic/unreachable in production paths

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_DIR"

PASS=0
FAIL=0
WARN=0

green() { printf '\033[32m%s\033[0m\n' "$*"; }
red()   { printf '\033[31m%s\033[0m\n' "$*"; }
yellow(){ printf '\033[33m%s\033[0m\n' "$*"; }
bold()  { printf '\033[1m%s\033[0m\n' "$*"; }

pass() { green "  PASS: $1"; PASS=$((PASS + 1)); }
fail() { red "  FAIL: $1"; FAIL=$((FAIL + 1)); }
warn() { yellow "  WARN: $1"; WARN=$((WARN + 1)); }

echo "═══════════════════════════════════════════════════════════════"
echo "  {{PROJECT}} — Aspect Tests (Cross-Cutting Concerns)"
echo "═══════════════════════════════════════════════════════════════"
echo ""

# ═══════════════════════════════════════════════════════════════════════
# Aspect 1: SPDX License Headers
# ═══════════════════════════════════════════════════════════════════════
bold "Aspect 1: SPDX license headers"

MISSING_SPDX=0
while IFS= read -r -d '' f; do
    if ! head -5 "$f" | grep -q "SPDX-License-Identifier"; then
        warn "Missing SPDX header: $f"
        MISSING_SPDX=$((MISSING_SPDX + 1))
    fi
done < <(find src/ -type f \( -name "*.rs" -o -name "*.zig" -o -name "*.res" -o -name "*.ex" -o -name "*.exs" -o -name "*.gleam" -o -name "*.idr" -o -name "*.sh" \) -print0 2>/dev/null)

if [ "$MISSING_SPDX" -eq 0 ]; then
    pass "All source files have SPDX headers"
else
    fail "$MISSING_SPDX files missing SPDX headers"
fi

# ═══════════════════════════════════════════════════════════════════════
# Aspect 2: Dangerous Patterns (BANNED)
# ═══════════════════════════════════════════════════════════════════════
bold "Aspect 2: Dangerous patterns"

# Idris2 dangerous patterns
DANGEROUS_IDRIS=$(grep -rn 'believe_me\|assert_total\|really_believe_me' src/abi/ 2>/dev/null | grep -v "^Binary" | grep -v "test" || true)
if [ -n "$DANGEROUS_IDRIS" ]; then
    fail "Dangerous Idris2 patterns found:"
    echo "$DANGEROUS_IDRIS" | head -5
else
    pass "No dangerous Idris2 patterns (believe_me, assert_total)"
fi

# Coq/Lean dangerous patterns
DANGEROUS_PROOF=$(grep -rn '\bAdmitted\b\|\bsorry\b\|\bunsafeCoerce\b\|\bObj\.magic\b' src/ verification/ 2>/dev/null | grep -v "test" | grep -v "comment" || true)
if [ -n "$DANGEROUS_PROOF" ]; then
    fail "Dangerous proof patterns found:"
    echo "$DANGEROUS_PROOF" | head -5
else
    pass "No dangerous proof patterns (Admitted, sorry, unsafeCoerce)"
fi

# ═══════════════════════════════════════════════════════════════════════
# Aspect 3: ABI/FFI Contract (if applicable)
# ═══════════════════════════════════════════════════════════════════════
# Uncomment if your project has Idris2 ABI + Zig FFI:

# bold "Aspect 3: ABI/FFI contract"
# if [ -d "src/abi" ] && [ -d "ffi/zig" ]; then
#     # Check that every exported function in Idris2 ABI has a Zig FFI implementation
#     ABI_EXPORTS=$(grep -h 'export' src/abi/*.idr 2>/dev/null | wc -l)
#     FFI_EXPORTS=$(grep -h 'pub export fn' ffi/zig/src/*.zig 2>/dev/null | wc -l)
#     if [ "$ABI_EXPORTS" -gt 0 ] && [ "$FFI_EXPORTS" -gt 0 ]; then
#         pass "ABI ($ABI_EXPORTS exports) and FFI ($FFI_EXPORTS exports) both present"
#     else
#         fail "ABI/FFI mismatch: $ABI_EXPORTS ABI exports, $FFI_EXPORTS FFI exports"
#     fi
# else
#     pass "ABI/FFI not applicable (no src/abi or ffi/zig)"
# fi

# ═══════════════════════════════════════════════════════════════════════
# Aspect 4: Error Handling (no raw panic in production code)
# ═══════════════════════════════════════════════════════════════════════
# Uncomment for Rust projects:

# bold "Aspect 4: Error handling"
# UNWRAP_COUNT=$(grep -rn '\.unwrap()' src/ 2>/dev/null | grep -v "test" | grep -v "example" | wc -l)
# if [ "$UNWRAP_COUNT" -gt 20 ]; then
#     warn "$UNWRAP_COUNT .unwrap() calls in src/ — consider replacing with ? or expect()"
# else
#     pass "Acceptable unwrap count: $UNWRAP_COUNT"
# fi

# ═══════════════════════════════════════════════════════════════════════
# Aspect 5: a diagnosis is not a path to green (squabble ≠ bypass)
# ═══════════════════════════════════════════════════════════════════════
# The execution-phase classifier explains *where* a run stopped. It must never
# be able to decide gate state, propose a move, or carry a tool's vocabulary.
# The Rust unit tests assert behaviour; this asserts the same properties
# structurally, on every run, without needing a toolchain.
bold "Aspect 5: diagnosis has no path to green"

EXEC_SRC="crates/squabble-core/src/execution.rs"
EXEC_DIRECTIVE=".machine_readable/bot_directives/execution_taxonomy.a2ml"
if [ -f "$EXEC_SRC" ]; then
    # Non-test code only, and comments stripped: the tests legitimately construct
    # inputs, and the doc comments legitimately *state* the invariant (naming
    # `GateState`/`Move` in prose is the point, not a violation).
    EXEC_PROD="$(awk '/#\[cfg\(test\)\]/{exit} {print}' "$EXEC_SRC" | grep -vE '^[[:space:]]*//')"

    if printf '%s' "$EXEC_PROD" | grep -qE '\bGateState\b|\bevaluate\(\)|gate::Gate\b'; then
        fail "execution.rs names a gate state or evaluates a gate — a phase is not a verdict"
    else
        pass "execution.rs cannot reach or name a gate state"
    fi

    if printf '%s' "$EXEC_PROD" | grep -qE '\bMove\b|moves::'; then
        fail "execution.rs references a Move — a classifier must propose nothing"
    else
        pass "execution.rs proposes no move"
    fi

    if [ -f "$EXEC_DIRECTIVE" ]; then
        # No step name may be compiled in: the directive is the only vocabulary,
        # except GitHub's own implicit job steps, which are primitives.
        LEAKED=""
        while IFS= read -r name; do
            case "$name" in
                "Set up job"|"Post job"|"Complete job") continue ;;
            esac
            if printf '%s' "$EXEC_PROD" | grep -Fq "\"$name\""; then
                LEAKED="$LEAKED $name"
            fi
        done < <(sed -nE 's/^[[:space:]]*"([^"]+)".*/\1/p' "$EXEC_DIRECTIVE")
        if [ -n "$LEAKED" ]; then
            fail "execution.rs compiles in step vocabulary from the directive:$LEAKED"
        else
            pass "no directive step name is compiled into execution.rs"
        fi
    else
        warn "no $EXEC_DIRECTIVE — the vocabulary test did not run"
    fi

    if printf '%s' "$EXEC_PROD" | grep -qE 'Command::new|std::process'; then
        fail "execution.rs runs a process — a classifier is pure"
    else
        pass "execution.rs is pure (no process, no IO)"
    fi
else
    warn "$EXEC_SRC absent — aspect 5 did not run"
fi

# ═══════════════════════════════════════════════════════════════════════
# Summary
# ═══════════════════════════════════════════════════════════════════════
echo ""
echo "═══════════════════════════════════════════════════════════════"
printf "  Results: "
green "PASS=$PASS" | tr -d '\n'
echo -n "  "
if [ "$FAIL" -gt 0 ]; then red "FAIL=$FAIL" | tr -d '\n'; else echo -n "FAIL=0"; fi
echo -n "  "
if [ "$WARN" -gt 0 ]; then yellow "WARN=$WARN"; else echo "WARN=0"; fi
echo ""
echo "═══════════════════════════════════════════════════════════════"

exit "$FAIL"
