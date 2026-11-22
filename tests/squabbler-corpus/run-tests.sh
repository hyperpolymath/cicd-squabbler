#!/bin/sh
# SPDX-License-Identifier: MPL-2.0
# Fire and silence tests for check-case.sh. Offline. Needs jq.
# Exit 0 only if every test behaves as expected.
set -u

here=$(cd "$(dirname "$0")" && pwd)
checker="$here/check-case.sh"
base="$here/cases/SQ-CASE-0012.json"
work=$(mktemp -d "${TMPDIR:-/tmp}/squabbler-corpus.XXXXXX")
trap 'rm -rf "$work"' EXIT INT TERM

n=0
fails=0
fail() { fails=$((fails + 1)); printf 'FAIL %s\n' "$1"; }

# codes_for FILTER [BASENAME]: write the mutated base case, run the checker, print its rule codes.
codes_for() {
    name=${2:-SQ-CASE-0012.json}
    dir=$(mktemp -d "$work/c.XXXXXX")
    jq "$1" "$base" > "$dir/$name" || { echo "BADFILTER"; return; }
    "$checker" "$dir/$name" | sed -E 's/^.*: (V[0-9]{3}) .*$/\1/' | sort -u | tr '\n' ' ' | sed 's/ $//'
}

# Fire tests: each mutation must produce exactly its own rule code.
fire() {
    n=$((n + 1))
    got=$(codes_for "$2" "${3:-SQ-CASE-0012.json}")
    if [ "$got" != "$1" ]; then fail "FIRE $1: expected exactly [$1], got [$got]"; fi
}

# Silence tests: each near miss must produce no findings.
silent() {
    n=$((n + 1))
    got=$(codes_for "$2")
    if [ -n "$got" ]; then fail "SILENT '$1': expected no findings, got [$got]"; fi
}

fire V001 'del(.job)'
fire V002 '.classification.group = "nonsense"'
# V003: the file name must equal the case id. Checked with a file named SQ-CASE-9999.json.
fire V003 '.' 'SQ-CASE-9999.json'
fire V004 '.evidence.log_excerpt = ([range(31) | "x"] | join("\n")) | .evidence.error_strings = ["x"]'
fire V005 '.fix.description = "use ghp_AAAAAAAAAAAAAAAAAAAAAAAA to push"'
fire V006 '.fix.description = "ask someone@example.org"'
fire V007 '.fix.description = "runner build01.internal is slow"'
fire V009 '.evidence.error_strings = [] | .classification.confidence = "proven"'
fire V010 '.fix.requires_owner_go = false'
fire V011 '.observed_at = "20 Nov 2026"'
fire V012 '.evidence.log_excerpt = "unrelated line" | .evidence.error_strings = ["not in the excerpt"]'

silent "plain case" '.'
silent "hash-like text, no token prefix" '.fix.description = "see commit c519c452 and sha256 6cbeb457"'
silent "ghp inside a word" '.fix.description = "ghpath is a variable name"'
silent "private-looking but public host" '.fix.description = "archive.ubuntu.com timed out"'
silent "version number, not an address" '.fix.description = "upload-artifact v7.0.2 and 10.2 seconds"'
silent "none owner without owner go" '.fix.owner = "none" | .fix.requires_owner_go = false'
silent "null observed_at" '.observed_at = null'
silent "hypothesis without strings" '.evidence.error_strings = [] | .classification.confidence = "hypothesis"'
silent "30-line excerpt" '.evidence.log_excerpt = ([range(30) | "x"] | join("\n")) | .evidence.error_strings = ["x"]'
silent "action@version is not an email" '.fix.description = "actions/upload-artifact@v7.0.2 is missing"'
silent "pr null (main)" '.pr = null'

# Real cases must stay silent, and there must be exactly 15 of them.
real=0
for p in "$here"/cases/*.json; do
    n=$((n + 1))
    real=$((real + 1))
    if ! out=$("$checker" "$p"); then fail "SILENT $(basename "$p"): $out"; fi
done
n=$((n + 1))
if [ "$real" -ne 15 ]; then fail "expected 15 cases, found $real"; fi

printf '%d/%d tests behaved as expected (%s)\n' "$((n - fails))" "$n" "11 fire, 11 near-miss silence, $real real cases silent"
[ "$fails" -eq 0 ]
