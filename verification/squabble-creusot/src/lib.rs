// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble-creusot` — the Creusot complement to `squabble-core`.
//!
//! The estate requires Rust that carries deductive-verification obligations,
//! the Rust twin of the SPARK proofs in `spark/`. This crate is that
//! complement for the squabbler: a **parallel verification artefact** that
//! restates the load-bearing invariants of the engine so a prover can be asked
//! whether they hold, rather than only a test suite.
//!
//! # What is here
//!
//! * [`model`] — the invariants as ordinary Rust, plus `#[test]`s that fail if
//!   the real `squabble-core` stops agreeing with them. This half compiles and
//!   runs on **stable** Rust and needs no Creusot installation.
//! * `contracts` — the same statements as Creusot contracts, compiled by
//!   default so a prove run cannot omit them (see the `creusot` feature note in
//!   `Cargo.toml`; the stable half builds with `--no-default-features`). It has
//!   a **literal** `#[requires]`/`#[ensures]` pair per obligation, which is what
//!   the estate gate counts.
//!
//! # Status: source-bound hosted proof receipt
//!
//! Run 41909189438, proof job 126852164109, discharged this verification crate
//! at merge revision f9499a8de0b8c525459a669e2cdbcacf83a49b85. It rejected the
//! false control, proved the true control, and reported Proved (13 files), a
//! file-level result rather than a theorem count. The gate's zero trusted-axiom
//! field counts lines with the exact trusted-attribute spelling in tracked Rust
//! files only; it does not count dependency or prover axioms. See docs/CREUSOT.adoc
//! for the spelling, full source and gate pins:
//! <https://github.com/hyperpolymath/cicd-squabbler/actions/runs/41909189438/job/126852164109>.
//!
//! That receipt does not prove the whole operational engine or later source.
//! The stable mirror tests remain a separate validation step, and a new
//! revision needs its own qualification before receiving a proof claim.
//!
//! # Why a separate crate
//!
//! `creusot-std` is nightly-only, and this package is excluded from the root
//! workspace so the stable `--locked` build and `cargo audit` never see it.
//! Its own `rust-toolchain.toml` pins the nightly that Creusot v0.14.0 is built
//! against. Nothing here is a runtime dependency of `squabble`.

pub mod model;

/// The contracts, compiled only when Creusot translates the crate.
#[cfg(feature = "creusot")]
pub mod contracts;
