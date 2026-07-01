// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! The outcome contract. Exactly three terminal results — never a fourth that
//! quietly means "I gave up but called it green".

use crate::gate::RequiredCheck;
use crate::moves::Move;
use serde::{Deserialize, Serialize};

/// The landing strategies the user permits — the *arena*. Estate default is
/// `{Squash, Rebase}`; merge-commit is never offered (linear-history rulesets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingStrategy {
    Squash,
    Rebase,
}

/// The single terminal value of a squabble. There is no `Bypassed` variant by
/// design: the type cannot express "won by override".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum Outcome {
    /// The gate was legitimately satisfied and landed via a permitted strategy.
    Green {
        landed_via: LandingStrategy,
        moves_applied: Vec<Move>,
    },
    /// Red was improved but not won; handed back to the human with what changed.
    Yellow {
        moves_applied: Vec<Move>,
        still_unsatisfied: Vec<RequiredCheck>,
    },
    /// Could not win. A structured report is emitted for the human; nothing was
    /// bypassed, nothing was merged.
    Red { report: Report },
}

/// The human-facing report emitted when the squabbler cannot win the gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub summary: String,
    pub unsatisfied: Vec<RequiredCheck>,
    pub moves_attempted: Vec<Move>,
    /// Why each remaining requirement could not be satisfied legitimately.
    pub blockers: Vec<String>,
}
