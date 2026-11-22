#!/bin/sh
# SPDX-License-Identifier: MPL-2.0
# Offline validator for squabbler corpus cases. Needs jq; no other dependencies.
# Usage: check-case.sh FILE.json [FILE.json ...]
# Exit 0 = every file silent. Exit 1 = at least one finding or unreadable file.
set -u

here=$(cd "$(dirname "$0")" && pwd)
rules="$here/check-case.jq"

if ! command -v jq >/dev/null 2>&1; then
    echo "check-case.sh: jq is required" >&2
    exit 2
fi
if [ "$#" -eq 0 ]; then
    echo "usage: check-case.sh FILE.json [FILE.json ...]" >&2
    exit 2
fi

rc=0
for path in "$@"; do
    if ! jq -e . "$path" >/dev/null 2>&1; then
        printf '%s: V000 unreadable or not valid JSON\n' "$path"
        rc=1
        continue
    fi
    name=$(basename "$path")
    if ! out=$(jq -r --arg fn "$name" -f "$rules" "$path"); then
        printf '%s: V000 rule evaluation failed\n' "$path"
        rc=1
        continue
    fi
    if [ -n "$out" ]; then
        printf '%s\n' "$out" | while IFS= read -r line; do
            printf '%s: %s\n' "$path" "$line"
        done
        rc=1
    fi
done
exit "$rc"
