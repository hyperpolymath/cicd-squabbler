// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! The gate model — the formal heart of `squabble ≠ bypass`, expressed in the
//! Rust type system. The SPARK sibling (`spark/`) proves the same invariant
//! mechanically: **the only transition into `Green` is a required check that
//! actually ran and passed.**
//!
//! There is deliberately no constructor, method, or transition on [`Gate`] that
//! reaches [`GateState::Green`] by removing a required context, renaming a check
//! away, or toggling an admin override. Those are *bypasses*; they are not
//! representable as paths to green here.

use serde::{Deserialize, Serialize};

/// The realised result of a single check run on the head commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRun {
    /// The check has not reported on the current head commit.
    Missing,
    /// The check is queued or in progress.
    Pending,
    /// The check ran to completion and failed.
    Failed,
    /// The check ran to completion and passed. The *only* green-bearing state.
    Passed,
}

/// A context the branch ruleset requires before a PR may land, paired with the
/// realised run that is meant to satisfy it. A requirement is satisfied **iff**
/// a run is bound to it (correct name, on the head commit) and that run
/// [`CheckRun::Passed`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredCheck {
    /// The exact context name the ruleset requires (e.g. `scan / gitleaks`).
    pub required_context: String,
    /// The realised run currently bound to that context, if any was found.
    pub run: CheckRun,
}

impl RequiredCheck {
    pub fn new(required_context: impl Into<String>, run: CheckRun) -> Self {
        Self { required_context: required_context.into(), run }
    }

    /// A requirement is satisfied only by a bound run that passed. This is the
    /// single predicate the whole engine trusts; everything else is plumbing.
    #[inline]
    pub fn is_satisfied(&self) -> bool {
        matches!(self.run, CheckRun::Passed)
    }
}

/// Where the gate currently sits. `Green` is computed, never asserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateState {
    /// At least one required check is missing/pending — the gate is stuck.
    Blocked,
    /// At least one required check ran and failed.
    Red,
    /// Every required check ran and passed. Landing is legitimate.
    Green,
}

/// The full gate: the set of required checks and their realised runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate {
    pub checks: Vec<RequiredCheck>,
}

impl Gate {
    pub fn new(checks: Vec<RequiredCheck>) -> Self {
        Self { checks }
    }

    /// Compute the gate state from the realised runs. This function is the Rust
    /// mirror of the SPARK `Evaluate` and carries the load-bearing invariant:
    ///
    /// * `Green`  ⇔ every required check `Passed`.
    /// * `Red`    ⇔ some required check `Failed` (and none missing/pending).
    /// * `Blocked` otherwise (something missing or pending).
    ///
    /// Crucially, there is no input by which "fewer required checks" or "an
    /// override flag" yields `Green`: green is a property of the *runs*, not of
    /// the requirement set's size or any admin capability.
    pub fn evaluate(&self) -> GateState {
        if self.checks.iter().all(RequiredCheck::is_satisfied) {
            // Vacuous truth on an empty requirement set is intentionally NOT
            // green: a gate with no required checks is unprotected, not won.
            if self.checks.is_empty() {
                return GateState::Blocked;
            }
            return GateState::Green;
        }
        if self.checks.iter().any(|c| matches!(c.run, CheckRun::Failed)) {
            return GateState::Red;
        }
        GateState::Blocked
    }

    /// The named requirements that are not yet satisfied — the squabbler's work
    /// list. Ordering is stable (declaration order) for reproducible reports.
    pub fn unsatisfied(&self) -> impl Iterator<Item = &RequiredCheck> {
        self.checks.iter().filter(|c| !c.is_satisfied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ck(name: &str, run: CheckRun) -> RequiredCheck {
        RequiredCheck::new(name, run)
    }

    #[test]
    fn all_passed_is_green() {
        let g = Gate::new(vec![ck("a", CheckRun::Passed), ck("b", CheckRun::Passed)]);
        assert_eq!(g.evaluate(), GateState::Green);
    }

    #[test]
    fn a_failure_is_red() {
        let g = Gate::new(vec![ck("a", CheckRun::Passed), ck("b", CheckRun::Failed)]);
        assert_eq!(g.evaluate(), GateState::Red);
    }

    #[test]
    fn a_missing_check_is_blocked_not_green() {
        // The deadlock class v0.1 targets: a required context with no run bound.
        let g = Gate::new(vec![ck("a", CheckRun::Passed), ck("b", CheckRun::Missing)]);
        assert_eq!(g.evaluate(), GateState::Blocked);
    }

    #[test]
    fn empty_requirement_set_is_never_green() {
        // squabble ≠ bypass: dropping all required contexts must NOT read green.
        let g = Gate::new(vec![]);
        assert_eq!(g.evaluate(), GateState::Blocked);
    }
}
