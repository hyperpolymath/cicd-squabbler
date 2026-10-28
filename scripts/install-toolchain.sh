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
# Environment:
#   CREUSOT_VERSION   default 0.14.0 — must match the pin in
#                     verification/squabble-creusot/rust-toolchain.toml and
#                     .github/workflows/creusot.yml
#   CREUSOT_NIGHTLY   default nightly-2026-08-03 — same pairing
#
# Exit codes: 0 ready, 1 a requested step failed, 2 nothing was done (missing
# prerequisite — e.g. no network, no opam and no way to install it).
set -uo pipefail

creusot_version="${CREUSOT_VERSION:-0.14.0}"
creusot_nightly="${CREUSOT_NIGHTLY:-nightly-2026-08-03}"
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
  say "installing rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --default-toolchain stable --profile minimal \
    || { say "rustup install failed — is https://static.rust-lang.org reachable?"; exit 1; }
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
  workdir="$(mktemp -d)"
  say "cloning creusot v$creusot_version into $workdir"
  git clone --depth 1 --branch "v$creusot_version" \
    https://github.com/creusot-rs/creusot "$workdir/creusot" \
    || { say "could not clone creusot"; exit 1; }
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
