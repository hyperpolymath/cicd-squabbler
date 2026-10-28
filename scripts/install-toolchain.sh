#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
#
# install-toolchain.sh — install the Rust toolchain this repository needs, and
# the Creusot complement the estate requires for its Rust.
#
# Two toolchains, deliberately:
#
#   stable         builds and tests the workspace (rust-version = "1.85")
#   nightly-…      Creusot's pinned nightly, used only below verification/
#
# Creusot is NOT installed with `cargo install creusot`: upstream ships an
# `INSTALL` script that provisions Why3 and why3find through opam. `cargo
# install creusot` produces a `cargo creusot` that cannot reach a prover, which
# is worse than not installing it (a gate that cannot say no must report VOID,
# never green).
#
# Usage:
#   scripts/install-toolchain.sh              # stable + creusot nightly + Creusot
#   scripts/install-toolchain.sh --no-creusot # stable + nightly only (fast)
#   scripts/install-toolchain.sh --check      # report what is present, install nothing
#
# Nothing here pipes a network response into an interpreter. rustup is bootstrapped
# from the official rustup-init *binary*, whose published SHA-256 is fetched over
# TLS from the same origin and verified before anything is executed; Creusot is
# cloned at the commit the release tag points to, and that commit is verified
# after checkout before ./INSTALL runs. `curl https://sh.rustup.rs | sh` is the
# pattern SonarCloud rates as a vulnerability, and it is right to.
#
# Environment:
#   CREUSOT_VERSION   default 0.14.0 — must match the pin in
#                     verification/squabble-creusot/rust-toolchain.toml and
#                     .github/workflows/creusot.yml
#   CREUSOT_COMMIT    default the commit v0.14.0 points to; required for any
#                     version this script has no pin for (fail closed, never
#                     clone a moving tag)
#   CREUSOT_NIGHTLY   default nightly-2026-08-03 — same pairing
#   RUSTUP_DIST_SERVER  default https://static.rust-lang.org (https only —
#                     fetch() refuses any other scheme)
#
# Exit codes: 0 ready, 1 a requested step failed, 2 nothing was done (missing
# prerequisite — e.g. no network, no opam and no way to install it).
set -uo pipefail

creusot_version="${CREUSOT_VERSION:-0.14.0}"
creusot_nightly="${CREUSOT_NIGHTLY:-nightly-2026-08-03}"

# A tag is a moving name; a commit is not. Each supported release pins the commit
# its tag pointed to when this script was written, so a re-pointed tag cannot
# change what gets built and executed.
case "$creusot_version" in
  0.14.0) creusot_commit_default="65349025c67e69fcd7479f1f420fa5904f69bc27" ;;
  *)      creusot_commit_default="" ;;
esac
creusot_commit="${CREUSOT_COMMIT:-$creusot_commit_default}"
with_creusot=true
check_only=false

for arg in "$@"; do
  case "$arg" in
    --no-creusot) with_creusot=false ;;
    --check) check_only=true ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "install-toolchain.sh: unknown argument '$arg'" >&2; exit 2 ;;
  esac
done

say() { printf '%s\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }

# fetch <url> <dest> — writes to a file, never to a shell's stdin.
fetch() {
  if have curl; then
    curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
      -o "$2" "$1"
  elif have wget; then
    wget --https-only --quiet -O "$2" "$1"
  else
    return 1
  fi
}

sha256_file() {
  if have sha256sum; then sha256sum "$1" | awk '{print $1}'
  elif have shasum; then shasum -a 256 "$1" | awk '{print $1}'
  else return 1
  fi
}

# verify_sha256 <dir> <file> — compare <file> with the digest published beside it
# as <file>.sha256 (first field, so both "hash  name" and "hash *name" work).
verify_sha256() {
  local dir="$1" file="$2" want got
  [ -f "$dir/$file.sha256" ] || return 1
  want="$(awk 'NR==1 {print $1}' "$dir/$file.sha256")"
  case "$want" in ''|*[!0-9a-fA-F]*) return 1 ;; esac
  [ "${#want}" -eq 64 ] || return 1
  got="$(sha256_file "$dir/$file")" || return 1
  [ "$want" = "$got" ]
}

status=0
note() { printf '  %-22s %s\n' "$1" "$2"; }

if [ "$check_only" = true ]; then
  say "toolchain status"
  note "rustup"      "$(have rustup && rustup --version 2>/dev/null | head -1 || echo MISSING)"
  note "rustc"       "$(have rustc && rustc --version || echo MISSING)"
  note "cargo"       "$(have cargo && cargo --version || echo MISSING)"
  note "nightly"     "$(rustup toolchain list 2>/dev/null | grep -q "$creusot_nightly" && echo "$creusot_nightly present" || echo "$creusot_nightly MISSING")"
  note "cargo creusot" "$(have cargo && cargo creusot --help >/dev/null 2>&1 && echo present || echo MISSING)"
  note "why3"        "$(have why3 && why3 --version 2>/dev/null | head -1 || echo MISSING)"
  note "opam"        "$(have opam && opam --version 2>/dev/null || echo MISSING)"
  if [ "$with_creusot" = true ] \
     && { ! have cargo || ! cargo creusot --help >/dev/null 2>&1; }; then
    say ""
    say "Creusot obligations are STATED, NOT PROVED: run 'squabble' proof work with"
    say "'cargo creusot prove' in verification/squabble-creusot after installing."
    status=2
  fi
  exit "$status"
fi

# ---------------------------------------------------------------- rustup ----
if ! have rustup; then
  say "installing rustup (verified rustup-init binary; no pipe-to-shell)"
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)              rustup_target=x86_64-unknown-linux-gnu ;;
    Linux-aarch64|Linux-arm64) rustup_target=aarch64-unknown-linux-gnu ;;
    Darwin-x86_64)             rustup_target=x86_64-apple-darwin ;;
    Darwin-arm64)              rustup_target=aarch64-apple-darwin ;;
    *) say "unsupported platform $(uname -s)/$(uname -m) — install rustup manually, then re-run"; exit 2 ;;
  esac
  dist="${RUSTUP_DIST_SERVER:-https://static.rust-lang.org}/rustup/dist/$rustup_target"
  rustup_tmp="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$rustup_tmp'" EXIT INT TERM
  fetch "$dist/rustup-init" "$rustup_tmp/rustup-init" \
    || { say "could not download $dist/rustup-init — is $dist reachable?"; exit 1; }
  fetch "$dist/rustup-init.sha256" "$rustup_tmp/rustup-init.sha256" \
    || { say "could not download the published digest for rustup-init — refusing to execute an unverified binary"; exit 1; }
  verify_sha256 "$rustup_tmp" rustup-init \
    || { say "rustup-init does not match its published digest — refusing to execute it"; exit 1; }
  chmod 0755 "$rustup_tmp/rustup-init"
  "$rustup_tmp/rustup-init" -y --no-modify-path --default-toolchain none --profile minimal \
    || { say "rustup-init failed"; exit 1; }
  rm -rf "$rustup_tmp"
  trap - EXIT INT TERM
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env" 2>/dev/null || true
fi
have cargo || { say "cargo is still missing after installing rustup"; exit 1; }

say "ensuring the stable toolchain (rustfmt, clippy)"
rustup toolchain install stable --profile minimal -c rustfmt -c clippy \
  || { say "stable toolchain install failed"; exit 1; }

say "ensuring the Creusot nightly $creusot_nightly"
rustup toolchain install "$creusot_nightly" --profile minimal \
  -c rustfmt -c rustc-dev -c llvm-tools \
  || { say "nightly $creusot_nightly install failed"; exit 1; }

[ "$with_creusot" = true ] || { say "done (Creusot skipped by --no-creusot)"; exit 0; }

# --------------------------------------------------------------- creusot ----
if cargo creusot --help >/dev/null 2>&1; then
  say "Creusot already present: $(cargo creusot --version 2>/dev/null | head -1)"
else
  have opam || {
    say ""
    say "Creusot $creusot_version needs opam, Why3 and an SMT solver."
    say "Install opam first (https://opam.ocaml.org/doc/Install.html), or run:"
    say "  opam init --bare -y && opam switch create creusot 5.1.1"
    say "Then re-run this script. Nothing about the proof claims changes until"
    say "then: the obligations stay STATED, NOT PROVED."
    exit 2
  }
  [ -n "$creusot_commit" ] || {
    say ""
    say "No commit is pinned for Creusot $creusot_version, and a tag can be"
    say "re-pointed. Set CREUSOT_COMMIT to the commit v$creusot_version points"
    say "to, and add it to the pin table in docs/CREUSOT.adoc. Nothing is cloned"
    say "until then: the obligations stay STATED, NOT PROVED."
    exit 2
  }
  workdir="$(mktemp -d)"
  say "cloning creusot v$creusot_version at $creusot_commit"
  git clone --no-checkout https://github.com/creusot-rs/creusot "$workdir/creusot" \
    || { say "could not clone creusot"; exit 1; }
  (
    cd "$workdir/creusot" || exit 1
    git fetch --depth 1 origin "$creusot_commit" 2>/dev/null || git fetch origin --tags || exit 1
    git checkout --detach "$creusot_commit" >/dev/null 2>&1 || exit 1
    head_sha="$(git rev-parse HEAD)"
    [ "$head_sha" = "$creusot_commit" ] || {
      say "creusot checkout is $head_sha, expected $creusot_commit — refusing to run ./INSTALL"
      exit 1
    }
  ) || { say "could not check out creusot at $creusot_commit"; exit 1; }
  ( cd "$workdir/creusot" && ./INSTALL ) \
    || { say "creusot's ./INSTALL failed — see its output above"; exit 1; }
  rm -rf "$workdir"
  cargo creusot --help >/dev/null 2>&1 \
    || { say "cargo creusot is still unavailable after ./INSTALL"; exit 1; }
fi

say ""
say "ready. Prove the obligations (this is the only step that changes"
say "'stated' into 'proved', and it has not been run for this repository yet):"
say "  cd verification/squabble-creusot && cargo creusot prove"
say "Stable-Rust mirror of the same statements:"
say "  cargo test --manifest-path verification/squabble-creusot/Cargo.toml"
