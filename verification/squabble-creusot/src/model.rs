// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! The invariants, as ordinary executable Rust.
//!
//! Each function here is the *statement* — the same proposition
//! [`crate::contracts`] states to Creusot, in a form that compiles on stable
//! Rust and can be checked against the real implementation. That pairing is the
//! point: the contracted module says what must be true, and the `#[test]`s at
//! the bottom of this file fail loudly if the real `squabble-core` ever stops
//! matching it.
//!
//! Nothing here is a proof, and the tests are not a prover. See
//! `../../docs/CREUSOT.adoc` for what has and has not been discharged.

/// Ruleset acceptance: the gate is `Green` exactly when the required set is
/// non-empty and **every** requirement is satisfied — where satisfied is
/// `Passed` or `Skipped`/`Neutral` (GitHub counts a skip as meeting the
/// requirement, so the gate does too).
///
/// The non-emptiness clause is load-bearing: the vacuous truth of "all zero
/// requirements passed" would otherwise read as a win, and a branch with no
/// required checks is unprotected, not won.
/// Counts are widened before addition so every pair of `u32` inputs is safe.
pub fn accepted(passed: u32, skipped: u32, total: u32) -> bool {
    total > 0 && passed as u64 + skipped as u64 == total as u64
}

/// Evidence-bearing acceptance: a **real** green needs every requirement to
/// have actually passed. A gate satisfied only by skips is `Green` by the
/// ruleset and evidence-free in fact; `squabble` reports the difference rather
/// than letting a skipped required scan read as a scan that ran.
pub fn has_evidence(passed: u32, total: u32) -> bool {
    total > 0 && passed == total
}

/// The caller's admission decision: a coordinate may start when a policy
/// pattern covers it, or when it is GitHub-owned and the posture says so.
pub fn admits(covered: bool, github_owned: bool) -> bool {
    covered || github_owned
}

/// Admission is required exactly when admission is refused — no third state,
/// no "unknown but let it through". Anything the squabbler cannot establish
/// leaves this decision to the caller; it never guesses a `true`.
pub fn requires_admission(covered: bool, github_owned: bool) -> bool {
    !admits(covered, github_owned)
}

/// Repointing a coordinate carries the pin **verbatim**: a SHA stays that SHA,
/// a tag stays that tag. Repointing is not re-pinning.
pub fn repointed_pin(pin: &str) -> &str {
    pin
}

#[cfg(test)]
mod tests {
    use super::*;
    use squabble_core::chains::{EdgeKind, Pin};
    use squabble_core::gate::{CheckRun, Gate, GateState, MissingCause, RequiredCheck};
    use squabble_core::startup::{
        attribute_zero_job, AllowList, CanonicalResolution, Coordinate, StartupFailureClass,
        ZeroJobAttribution,
    };

    const SHA: &str = "8f2ee50841e216cd8c192eeb68953118190f105c";

    fn gate_of(runs: &[CheckRun]) -> Gate {
        Gate::new(
            runs.iter()
                .enumerate()
                .map(|(i, r)| RequiredCheck::new(format!("c{i}"), *r))
                .collect(),
        )
    }

    /// The model agrees with the real evaluator over every combination of
    /// realised runs for one to three required checks, and over the empty set.
    #[test]
    fn accepted_matches_the_real_gate_evaluator() {
        let variants = [
            CheckRun::Missing,
            CheckRun::Pending,
            CheckRun::Failed,
            CheckRun::Skipped,
            CheckRun::Passed,
        ];
        let mut cases: Vec<Vec<CheckRun>> = vec![Vec::new()];
        for a in variants {
            cases.push(vec![a]);
            for b in variants {
                cases.push(vec![a, b]);
                for c in variants {
                    cases.push(vec![a, b, c]);
                }
            }
        }
        for runs in cases {
            let passed = runs.iter().filter(|r| **r == CheckRun::Passed).count() as u32;
            let skipped = runs.iter().filter(|r| **r == CheckRun::Skipped).count() as u32;
            let total = runs.len() as u32;
            let real = gate_of(&runs).evaluate() == GateState::Green;
            assert_eq!(
                accepted(passed, skipped, total),
                real,
                "model disagrees with Gate::evaluate for {runs:?}"
            );
        }
    }

    /// Large counts must neither panic nor wrap into a falsely accepted gate.
    #[test]
    fn acceptance_handles_count_boundaries() {
        for (passed, skipped, total, expected) in [
            (0, 0, 0, false),
            (u32::MAX, 0, u32::MAX, true),
            (0, u32::MAX, u32::MAX, true),
            (u32::MAX - 1, 1, u32::MAX, true),
            (u32::MAX, 1, u32::MAX, false),
            (u32::MAX, 1, 0, false),
            (u32::MAX, 2, 1, false),
            (2, u32::MAX, 1, false),
            (u32::MAX, u32::MAX, u32::MAX - 1, false),
        ] {
            assert_eq!(
                accepted(passed, skipped, total),
                expected,
                "passed={passed}, skipped={skipped}, total={total}"
            );
        }
    }

    /// A skip satisfies the ruleset but carries no evidence, and the model says
    /// both things at once — which is exactly the distinction the real gate
    /// exposes through `evidence_free`.
    #[test]
    fn a_skipped_only_gate_is_accepted_yet_evidenceless() {
        let g = gate_of(&[CheckRun::Skipped, CheckRun::Passed]);
        assert_eq!(g.evaluate(), GateState::Green);
        assert!(accepted(1, 1, 2));
        assert!(!has_evidence(1, 2));
        assert_eq!(g.evidence_free().count(), 1);
    }

    /// No diagnosis is a path to green: a waived-by-diagnosis requirement is
    /// still `Missing`, whatever cause is attached to it.
    #[test]
    fn no_diagnosed_cause_makes_a_missing_check_green() {
        let causes = [
            MissingCause::NoSuchJob,
            MissingCause::DeadActionPin,
            MissingCause::ActionsPolicyRefusal,
            MissingCause::MovedUpstreamCoordinate,
            MissingCause::AllowListAdmissionRequired,
            MissingCause::EmptyJobsMap,
            MissingCause::NotTriggeredOnPullRequest,
            MissingCause::Unknown,
        ];
        for cause in causes {
            let g = Gate::new(vec![
                RequiredCheck::new("a", CheckRun::Passed),
                RequiredCheck::new("b", CheckRun::Missing).with_cause(cause),
            ]);
            assert_ne!(g.evaluate(), GateState::Green, "{cause:?} must not be green");
            // ...and the model says the same thing about the same shape.
            assert!(!accepted(1, 0, 2), "{cause:?}");
        }
    }

    /// Admission is refused exactly when `missing_pattern` offers a pattern:
    /// the report never invents an admission gap, and never hides one.
    #[test]
    fn requires_admission_is_the_complement_of_covering() {
        let allow = AllowList {
            patterns: vec!["owner/repo@*".into(), "wide/*".into(), "actions/*".into()],
            github_owned_allowed: true,
        };
        for coord in [
            "owner/repo",
            "wide/anything",
            "actions/checkout",
            "github/codeql-action",
            "other/repo",
            "nobody/nothing",
        ] {
            let covered = allow.covers(coord);
            assert_eq!(
                requires_admission(covered, false),
                allow.missing_pattern(coord).is_some(),
                "admission model disagrees for {coord}"
            );
            assert_eq!(admits(covered, false), covered);
        }
    }

    /// The two zero-job classes stay distinct all the way to the gate cause,
    /// and an unattributed run names no cause at all.
    #[test]
    fn the_zero_job_classes_map_to_distinct_causes() {
        let moved = attribute_zero_job(
            "hyperpolymath/hypatia",
            "wf.yml",
            &format!("old/stds/.github/workflows/wf.yml@{SHA}"),
            EdgeKind::ReusableWorkflow,
            &CanonicalResolution::Moved {
                canonical: "new/stds".into(),
            },
            None,
        );
        let admission = attribute_zero_job(
            "hyperpolymath/hypatia",
            "wf.yml",
            &format!("new/stds/.github/workflows/wf.yml@{SHA}"),
            EdgeKind::ReusableWorkflow,
            &CanonicalResolution::Canonical,
            Some(&AllowList {
                patterns: Vec::new(),
                github_owned_allowed: true,
            }),
        );
        let unknown = ZeroJobAttribution {
            repo: "hyperpolymath/hypatia".into(),
            workflow: "wf.yml".into(),
            coordinate: "(none attributed)".into(),
            canonical: None,
            required_allow_list_pattern: None,
            class: StartupFailureClass::Unattributed,
            evidence: "unattributed".into(),
            sequencing: Vec::new(),
        };
        assert_eq!(
            moved.missing_cause(),
            Some(MissingCause::MovedUpstreamCoordinate)
        );
        assert_eq!(
            admission.missing_cause(),
            Some(MissingCause::AllowListAdmissionRequired)
        );
        assert_ne!(moved.missing_cause(), admission.missing_cause());
        assert_eq!(unknown.missing_cause(), None);
    }

    /// A repoint keeps the pin — the same characters, not a resolved version of
    /// them — and keeps the subpath it was written with.
    #[test]
    fn a_repoint_carries_the_pin_verbatim() {
        let raw = format!("old/stds/.github/workflows/gov.yml@{SHA}");
        let c = Coordinate::parse(&raw).expect("parse");
        let rewritten = c.rewritten("new/stds");
        assert_eq!(
            rewritten,
            format!("new/stds/.github/workflows/gov.yml@{SHA}")
        );
        assert_eq!(
            Pin::parse(repointed_pin(c.pin.as_str())),
            Pin::Sha(SHA.to_string())
        );
        // A tag pin is carried just as literally.
        let tag = Coordinate::parse("old/stds/.github/workflows/gov.yml@v2").unwrap();
        assert_eq!(tag.rewritten("new/stds"), "new/stds/.github/workflows/gov.yml@v2");
    }
}
