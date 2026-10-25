#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
#
# verify-pin.sh — verify a downstream pin of one of the contracts in this
# directory, the way a consumer's CI should verify it.
#
#   bash contracts/bin/verify-pin.sh squabble 1.0.0
#   bash contracts/bin/verify-pin.sh proven-tests 1.0.0 --repo-dir .
#
# It checks three things and prints a pin block the consumer records:
#
#   1. the contract's REVISION file names exactly the requested revision;
#   2. every file in the contract directory matches its sha256 in SHASUMS;
#   3. no file exists in the contract directory that SHASUMS does not list.
#
# The pin block contains the revision id and the sha256 of SHASUMS itself, so a
# consumer can record one digest instead of one per file. That digest is what
# "pinned contract" means for the rest of these documents.
#
# Exit codes: 0 verified, 1 mismatch or drift, 2 usage or environment failure.

set -euo pipefail

readonly PROG="verify-pin.sh"
readonly EXIT_FAILED=1
readonly EXIT_USAGE=2

usage() {
  printf 'usage: %s <squabble|proven-tests> <revision> [--repo-dir DIR]\n' "$PROG" >&2
  printf '  revision is the first component of the revision id, e.g. 1.0.0\n' >&2
}

die() {
  printf '%s: %s\n' "$PROG" "$*" >&2
  exit "$EXIT_USAGE"
}

# sha256 FILE — lowercase hex digest, coreutils or macOS.
sha256() {
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

main() {
  local contract="" revision="" repo_dir=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --repo-dir) [[ $# -ge 2 ]] || die "--repo-dir needs a value"; repo_dir="$2"; shift 2 ;;
      -h|--help) usage; exit 0 ;;
      -*) usage; die "unknown option: $1" ;;
      *) if [[ -z "$contract" ]]; then contract="$1"
         elif [[ -z "$revision" ]]; then revision="$1"
         else usage; die "unexpected argument: $1"; fi
         shift ;;
    esac
  done
  [[ -n "$contract" && -n "$revision" ]] || { usage; exit "$EXIT_USAGE"; }

  local root
  if [[ -n "$repo_dir" ]]; then
    root="$(cd "$repo_dir" && pwd)"
  else
    root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  fi
  local dir="$root/$contract"
  [[ -d "$dir" ]] || die "no such contract directory: $dir"
  [[ -f "$dir/REVISION" ]] || die "$dir has no REVISION file"

  local id
  id="$(head -n 1 "$dir/REVISION" | tr -d '\r' | sed 's/[[:space:]]*$//')"
  if [[ -z "$id" ]]; then
    printf 'FAIL  %s/REVISION is empty\n' "$contract" >&2
    exit "$EXIT_FAILED"
  fi

  # The revision id is "<contract-name>/<version>". Accept the bare version and
  # the full id, refuse anything else: a consumer that pins 1.0.0 must not be
  # handed 1.1.0 because someone edited REVISION in place.
  local want_id
  case "$id" in
    */"$revision") want_id="$id" ;;
    *) want_id="$contract/$revision" ;;
  esac
  if [[ "$id" != "$want_id" ]]; then
    printf 'FAIL  %s/REVISION is %s, pinned %s\n' "$contract" "$id" "$want_id" >&2
    exit "$EXIT_FAILED"
  fi
  if [[ ! -f "$dir/SHASUMS" ]]; then
    printf 'FAIL  %s/SHASUMS is missing; the revision is unverifiable\n' "$contract" >&2
    exit "$EXIT_FAILED"
  fi

  if ! bash "$root/bin/make-shasums.sh" "$dir" --check; then
    printf 'FAIL  %s drifted from its published SHASUMS\n' "$contract" >&2
    exit "$EXIT_FAILED"
  fi

  local sums_digest
  sums_digest="$(sha256 "$dir/SHASUMS")"
  printf 'verified  %s at revision %s\n' "$contract" "$id"
  printf 'files     %s\n' "$(wc -l < "$dir/SHASUMS" | tr -d ' ')"
  printf '\nrecord this pin in the consumer repository:\n\n'
  printf '  contract: %s\n' "$id"
  printf '  shasums:  sha256:%s\n' "$sums_digest"
}

main "$@"
