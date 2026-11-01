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
//! # Status: stated, not discharged
//!
//! These obligations have been **written, never proved**. No run has reached
//! Why3: the environment this crate was added in has no Rust toolchain, and
//! this repository's own Rust CI currently dies as a zero-job
//! `startup_failure`, so CI has not compiled the workspace either. The shared
//! estate gate has a real-tool canary validated upstream; that canary proves a
//! fixture, not this crate. An installer failure means these obligations were
//! not exercised, and this repo's execution taxonomy reports the observed
//! Creusot installer step as `PrerequisiteMissing`. The shared gate's aggregate
//! `Creusot verdict` still needs to distinguish that from a failed proof and
//! report that no proof was performed. Do not describe this crate as
//! verified until a run of this crate's obligations succeeds; the honest phrase
//! is **stated, not proved**.
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
