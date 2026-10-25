#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
#
# make-shasums.sh — write <contract-dir>/SHASUMS: the sha256 of every file in
# that contract directory, so a downstream repository can pin the contract by
# content instead of by a branch name.
#
#   bash contracts/bin/make-shasums.sh contracts/squabble
#   bash contracts/bin/make-shasums.sh contracts/proven-tests --check
#
# SHASUMS lists paths relative to the contract directory, sorted, one per line
# as "<sha256>  <path>" — the format `sha256sum -c` reads. SHASUMS itself is
# never listed (a file cannot contain its own digest).
#
# --check verifies the existing SHASUMS instead of writing it, and exits 1 on
#   any mismatch, missing file or unlisted file. That is what the conformance
#   suites run, so a published revision cannot drift silently.
#
# Exit codes: 0 ok, 1 verification failed, 2 usage or environment failure.

set -euo pipefail

readonly PROG="make-shasums"
readonly EXIT_FAILED=1
readonly EXIT_USAGE=2

usage() {
  printf 'usage: %s <contract-dir> [--check]\n' "$PROG" >&2
}

die() {
  printf '%s: %s\n' "$PROG" "$*" >&2
  exit "$EXIT_USAGE"
}

# digest FILE — print the sha256 of FILE as lowercase hex, on any box that has
# either coreutils (sha256sum) or the perl-script shasum macOS ships.
digest() {
  local out
  if command -v sha256sum >/dev/null 2>&1; then
    out="$(sha256sum -- "$1")"
  elif command -v shasum >/dev/null 2>&1; then
    out="$(shasum -a 256 -- "$1")"
  else
    die "neither sha256sum nor shasum is available"
  fi
  printf '%s\n' "${out%% *}"
}

# listed_files DIR — every regular file under DIR except SHASUMS, relative to
# DIR, sorted. `find` order is not portable, so it is sorted here rather than
# trusted; the same set must come out on every machine.
listed_files() {
  local dir="$1"
  ( cd "$dir" && find . -type f ! -name SHASUMS -print ) \
    | sed 's|^\./||' \
    | LC_ALL=C sort
}

# write_sums DIR — write DIR/SHASUMS.
#
# The list is built before the manifest exists and the manifest is written
# outside DIR then moved in, so the manifest can never list a half-written
# copy of itself. (It did, once: SHASUMS.tmp appeared in its own manifest and
# the digest check then failed on the temp file it had just deleted.)
write_sums() {
  local dir="$1" tmp f
  tmp="$(mktemp "${TMPDIR:-/tmp}/shasums.XXXXXX")"
  listed_files "$dir" | while IFS= read -r f; do
    printf '%s  %s\n' "$(digest "$dir/$f")" "$f"
  done > "$tmp"
  mv -- "$tmp" "$dir/SHASUMS"
}

# check_sums DIR — verify DIR/SHASUMS against the files on disk. Fails on a
# changed file, a deleted file, and a file present but unlisted: an attacker
# adding a file to a "pinned" directory must not pass unnoticed.
check_sums() {
  local dir="$1" line want path have rc=0
  local -a seen=()
  if [[ ! -f "$dir/SHASUMS" ]]; then
    printf '%s: %s/SHASUMS does not exist\n' "$PROG" "$dir" >&2
    return "$EXIT_FAILED"
  fi
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    want="${line%% *}"
    path="${line#*  }"
    if [[ ! -f "$dir/$path" ]]; then
      printf 'MISSING  %s\n' "$path" >&2
      rc="$EXIT_FAILED"
      continue
    fi
    have="$(digest "$dir/$path")"
    if [[ "$have" != "$want" ]]; then
      printf 'CHANGED  %s (listed %s, on disk %s)\n' "$path" "$want" "$have" >&2
      rc="$EXIT_FAILED"
    fi
    seen+=("$path")
  done < "$dir/SHASUMS"

  while IFS= read -r path; do
    local known=""
    local s
    for s in "${seen[@]+"${seen[@]}"}"; do
      if [[ "$s" == "$path" ]]; then known="yes"; break; fi
    done
    if [[ -z "$known" ]]; then
      printf 'UNLISTED %s\n' "$path" >&2
      rc="$EXIT_FAILED"
    fi
  done < <(listed_files "$dir")
  return "$rc"
}

main() {
  local dir="" check="no"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --check) check="yes"; shift ;;
      -h|--help) usage; exit 0 ;;
      -*) usage; die "unknown option: $1" ;;
      *) if [[ -n "$dir" ]]; then usage; die "one contract directory only"; fi; dir="$1"; shift ;;
    esac
  done
  [[ -n "$dir" ]] || { usage; die "missing <contract-dir>"; }
  [[ -d "$dir" ]] || die "not a directory: $dir"
  # A contract directory without a REVISION file is not a contract, so this
  # refuses rather than producing digests for some random tree.
  [[ -f "$dir/REVISION" ]] || die "$dir has no REVISION file; refusing to digest it"

  if [[ "$check" == "yes" ]]; then
    check_sums "$dir"
  else
    write_sums "$dir"
    printf 'wrote %s/SHASUMS (%s files)\n' "$dir" "$(wc -l < "$dir/SHASUMS" | tr -d ' ')" >&2
  fi
}

main "$@"
