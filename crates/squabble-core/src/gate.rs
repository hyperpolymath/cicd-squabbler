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


/// Why a required context shows [`CheckRun::Missing`].
///
/// `Missing` is the gate's most common stuck state and its least actionable one:
/// "nothing reported" says nothing about what to change. Each variant here was
/// measured on this estate on 2026-07-29, and each has a different remedy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingCause {
    /// No job anywhere emits this exact name — the context is invented, or the
    /// case differs. Measured: `hypatia-scan` (no such job) and `codeql` where
    /// the real job is `CodeQL`.
    NoSuchJob,

    /// The action repository that would emit it has been DELETED. An
    /// unresolvable `uses:` ref produces **no check run at all** — not a red one
    /// — so the board reads green while the job never ran. Measured across 69
    /// repos pinning `a2ml-validate-action` / `k9-validate-action` (both 404).
    DeadActionPin,

    /// `allowed_actions: selected` with an EMPTY `patterns_allowed` refuses every
    /// non-GitHub-owned `uses:` at parse time. The run dies as `startup_failure`
    /// with zero jobs, so nothing reports. This is a repository *settings* fault:
    /// no workflow edit can fix it.
    ActionsPolicyRefusal,

    /// The workflow's `jobs:` map is present but empty — every job block
    /// commented out. An empty `jobs:` map is invalid, so the run fails with
    /// zero jobs. Measured in 20 repos' `e2e.yml`.
    EmptyJobsMap,

    /// The producing job exists and runs, but never on `pull_request` — it is
    /// push-, schedule- or `dynamic`-triggered. It therefore cannot satisfy a
    /// **PR** gate, however healthy it looks on the default branch.
    ///
    /// Measured: `Dependabot`, required on 25 repos, emitted 10 check-runs on
    /// `main` (GitHub's managed `dependabot-updates` runner) and **0** on any PR
    /// head. Sampling `main` scores it healthy; sampling PR heads shows the truth.
    NotTriggeredOnPullRequest,

    /// Not yet diagnosed.
    Unknown,
}

impl MissingCause {
    /// One-line remediation. Deliberately concrete: a diagnosis a human cannot
    /// act on is only marginally better than none.
    pub fn remedy(&self) -> &'static str {
        match self {
            Self::NoSuchJob =>
                "Remove the context, or rename it to match a real job exactly (names are case-sensitive).",
            Self::DeadActionPin =>
                "The action repo is gone. Repoint `uses:` at its live location; do NOT vendor it per-consumer.",
            Self::ActionsPolicyRefusal =>
                "Repository settings: populate `patterns_allowed`, or set allowed_actions to `all`. No workflow edit can fix this.",
            Self::EmptyJobsMap =>
                "The workflow has no uncommented jobs. Instantiate one, or delete the workflow.",
            Self::NotTriggeredOnPullRequest =>
                "Add `pull_request` to the producing workflow's triggers, or stop requiring it on PRs.",
            Self::Unknown =>
                "Diagnose by sampling PR-head check-runs AND commit statuses, not the default branch.",
        }
    }
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
    /// Why `run` is [`CheckRun::Missing`], when known. `None` for any other
    /// state. `#[serde(default)]` keeps previously-serialised gates readable.
    #[serde(default)]
    pub missing_cause: Option<MissingCause>,
}

impl RequiredCheck {
    pub fn new(required_context: impl Into<String>, run: CheckRun) -> Self {
        Self {
            required_context: required_context.into(),
            run,
            missing_cause: None,
        }
    }

    /// A requirement is satisfied only by a bound run that passed. This is the
    /// single predicate the whole engine trusts; everything else is plumbing.
    #[inline]
    /// Attach a diagnosis for a missing context.
    pub fn with_cause(mut self, cause: MissingCause) -> Self {
        self.missing_cause = Some(cause);
        self
    }

    /// The remedy for this requirement, when it is missing and diagnosed.
    pub fn remedy(&self) -> Option<&'static str> {
        match (self.run, self.missing_cause) {
            (CheckRun::Missing, Some(c)) => Some(c.remedy()),
            _ => None,
        }
    }

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
        if self
            .checks
            .iter()
            .any(|c| matches!(c.run, CheckRun::Failed))
        {
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

    #[test]
    fn missing_cause_is_optional_and_does_not_alter_gate_state() {
        // A diagnosis explains a stuck gate; it must never move it.
        let undiagnosed = Gate::new(vec![RequiredCheck::new("scan / gitleaks", CheckRun::Missing)]);
        let diagnosed = Gate::new(vec![RequiredCheck::new("scan / gitleaks", CheckRun::Missing)
            .with_cause(MissingCause::DeadActionPin)]);
        assert_eq!(undiagnosed.evaluate(), GateState::Blocked);
        assert_eq!(diagnosed.evaluate(), GateState::Blocked);
        assert_eq!(diagnosed.evaluate(), undiagnosed.evaluate());
    }

    #[test]
    fn remedy_is_offered_only_for_diagnosed_missing_checks() {
        assert!(RequiredCheck::new("x", CheckRun::Missing).remedy().is_none());
        assert!(RequiredCheck::new("x", CheckRun::Passed)
            .with_cause(MissingCause::NoSuchJob)
            .remedy()
            .is_none());
        assert!(RequiredCheck::new("x", CheckRun::Missing)
            .with_cause(MissingCause::NotTriggeredOnPullRequest)
            .remedy()
            .is_some());
    }

    #[test]
    fn a_diagnosed_missing_check_still_cannot_reach_green() {
        // The load-bearing invariant: no diagnosis is a bypass.
        for cause in [
            MissingCause::NoSuchJob,
            MissingCause::DeadActionPin,
            MissingCause::ActionsPolicyRefusal,
            MissingCause::EmptyJobsMap,
            MissingCause::NotTriggeredOnPullRequest,
            MissingCause::Unknown,
        ] {
            let g = Gate::new(vec![
                RequiredCheck::new("a", CheckRun::Passed),
                RequiredCheck::new("b", CheckRun::Missing).with_cause(cause),
            ]);
            assert_ne!(g.evaluate(), GateState::Green, "{cause:?} must not reach Green");
        }
    }
}
