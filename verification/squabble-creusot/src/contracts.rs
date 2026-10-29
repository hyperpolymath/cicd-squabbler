// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! The same invariants as [`crate::model`], stated to Creusot.
//!
//! Compiled only under `--features creusot` (and only by `cargo creusot`), so
//! the stable build never needs the nightly-only proof library. Each contract
//! mirrors one function in [`crate::model`] — keep the pair in step; the model
//! carries the tests that fail when the real implementation drifts.
//!
//! **Status: stated, not discharged.** No obligation here has been proved: this
//! environment has no Rust toolchain at all, and the repository's own Rust CI
//! does not start (zero-job `startup_failure`). See `../../docs/CREUSOT.adoc`.

use creusot_std::prelude::*;

/// `Green` ⇔ the required set is non-empty and every requirement is satisfied.
///
/// `total > 0` is stated as a precondition as well, so the proof does not have
/// to reason about the vacuous case: a gate with no requirements is
/// unprotected, and `Gate::evaluate` deliberately reports it `Blocked`.
/// Counts are widened before addition so every pair of `u32` inputs is safe.
#[requires(total > 0u32)]
#[ensures(result == (passed as u64 + skipped as u64 == total as u64))]
pub fn accepted(passed: u32, skipped: u32, total: u32) -> bool {
    passed as u64 + skipped as u64 == total as u64
}

/// A real green needs every requirement to have *passed*, not merely been
/// waived: a gate satisfied only by skips is evidence-free.
#[requires(total > 0u32)]
#[ensures(result == (passed == total))]
pub fn has_evidence(passed: u32, total: u32) -> bool {
    passed == total
}

/// A diagnosis neither creates nor removes acceptance: the decision depends
/// only on the realised runs, never on what the squabbler says about them.
///
/// `diagnosed` is deliberately free: the postcondition does not mention it.
#[requires(total > 0u32)]
#[ensures(result == (passed as u64 + skipped as u64 == total as u64))]
pub fn after_diagnosis(passed: u32, skipped: u32, total: u32, diagnosed: bool) -> bool {
    let _ = diagnosed;
    passed as u64 + skipped as u64 == total as u64
}

/// Admission of a coordinate: a covering pattern, or a GitHub-owned coordinate
/// under a posture that allows GitHub-owned actions.
#[ensures(result == (covered || github_owned))]
pub fn admits(covered: bool, github_owned: bool) -> bool {
    covered || github_owned
}

/// Admission is required exactly when admission is refused — no third answer,
/// and never a `true` invented for an unknown.
#[ensures(result == !(covered || github_owned))]
pub fn requires_admission(covered: bool, github_owned: bool) -> bool {
    !(covered || github_owned)
}

/// Admission only lets a check *start*: it changes nothing about acceptance, so
/// the green still has to come from the check genuinely running and passing.
#[requires(total > 0u32)]
#[ensures(result == (passed as u64 + skipped as u64 == total as u64))]
pub fn after_admission(passed: u32, skipped: u32, total: u32, admitted: bool) -> bool {
    let _ = admitted;
    passed as u64 + skipped as u64 == total as u64
}
