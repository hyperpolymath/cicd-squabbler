// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! The outcome contract. Exactly three terminal results — never a fourth that
//! quietly means "I gave up but called it green".

use crate::gate::RequiredCheck;
use crate::moves::{EscalationKind, ExpertGroup, Move, OwnershipDisposition};
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

/// The human-facing report emitted when the squabbler cannot win the gate
/// outright. It is also the **evidence manifest** required by `AGENTIC.a2ml`
/// (`evidence-per-step`, `no-silent-skip`): every red is accounted for as
/// either a self-win move the squabbler attempted, an escalation to an expert
/// group, or an owner assignment — nothing is silently dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub summary: String,
    pub unsatisfied: Vec<RequiredCheck>,
    /// The legitimate self-win moves the squabbler proposed/attempted (the reds
    /// inside its own lane — CI/gate configuration).
    pub moves_attempted: Vec<Move>,
    /// Reds handed to an expert group because they are out of the squabbler's
    /// lane (code fixes, proofs, scans). Each is a hand-off, never a win.
    #[serde(default)]
    pub escalations: Vec<Escalation>,
    /// Checks the squabbler put into the debate with a named owner — things that
    /// should be in CI, are misconfigured, or are owned by an upstream repo.
    #[serde(default)]
    pub owner_assignments: Vec<OwnerAssignment>,
    /// Full-fidelity results of summoning experts for escalations (one entry
    /// per expert call, success or failure). Typed so no evidence is lost to
    /// prose truncation and downstream consumers can parse verdicts back out;
    /// the escalation's `evidence` string carries only the human narration.
    #[serde(default)]
    pub expert_verdicts: Vec<ExpertVerdict>,
    /// Why each remaining requirement could not be satisfied legitimately.
    pub blockers: Vec<String>,
}

/// One expert call made while summoning, recorded verbatim. `ok` follows the
/// fail-closed rule: it is `true` only when the expert both responded AND its
/// body carries no error signal — transport success alone is not a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpertVerdict {
    /// The required-context the escalation concerns.
    pub check: String,
    /// The cartridge/service that was summoned (host-side identifier).
    pub cartridge: String,
    /// The tool invoked on it.
    pub tool: String,
    /// Whether the call genuinely succeeded (transport AND body).
    pub ok: bool,
    /// Honest framing of what a success *means* (no overclaim — e.g.
    /// "assessment only, actuation external").
    pub meaning: String,
    /// The raw response body (JSON text) on success, or the failure
    /// description. Never truncated.
    pub verdict: String,
}

/// A hand-off of one red to a specialist [`ExpertGroup`] — the typed projection
/// of a [`Move::EscalateToExpert`] into the report's evidence manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Escalation {
    pub check: String,
    pub group: ExpertGroup,
    pub obligation: EscalationKind,
    pub evidence: String,
}

/// The attribution of one check to a responsible owner — the typed projection
/// of a [`Move::AssignGateOwner`] into the report's evidence manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerAssignment {
    pub check: String,
    pub owner: String,
    pub disposition: OwnershipDisposition,
    pub rationale: String,
}

impl Escalation {
    /// Project a [`Move::EscalateToExpert`] into an [`Escalation`]; any other
    /// move yields `None`. This keeps the report's typed sections and the move
    /// list a single source of truth.
    pub fn from_move(m: &Move) -> Option<Self> {
        match m {
            Move::EscalateToExpert {
                check,
                group,
                obligation,
                evidence,
            } => Some(Escalation {
                check: check.clone(),
                group: *group,
                obligation: *obligation,
                evidence: evidence.clone(),
            }),
            _ => None,
        }
    }
}

impl OwnerAssignment {
    /// Project a [`Move::AssignGateOwner`] into an [`OwnerAssignment`]; any other
    /// move yields `None`.
    pub fn from_move(m: &Move) -> Option<Self> {
        match m {
            Move::AssignGateOwner {
                check,
                owner,
                disposition,
                rationale,
            } => Some(OwnerAssignment {
                check: check.clone(),
                owner: owner.clone(),
                disposition: disposition.clone(),
                rationale: rationale.clone(),
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::moves::{EscalationKind, ExpertGroup, OwnershipDisposition};

    #[test]
    fn escalation_projects_only_from_escalate_move() {
        let m = Move::EscalateToExpert {
            check: "lint-shell".into(),
            group: ExpertGroup::HypatiaFleet,
            obligation: EscalationKind::DispatchFix,
            evidence: "shellcheck".into(),
        };
        assert_eq!(Escalation::from_move(&m).unwrap().check, "lint-shell");
        assert!(Escalation::from_move(&Move::GroundTruthCheckNames {
            workflow: "ci.yml".into()
        })
        .is_none());
    }

    #[test]
    fn owner_assignment_projects_only_from_assign_move() {
        let m = Move::AssignGateOwner {
            check: "gov".into(),
            owner: "hyperpolymath/standards".into(),
            disposition: OwnershipDisposition::OwnedUpstream {
                repo: "hyperpolymath/standards".into(),
            },
            rationale: "reusable".into(),
        };
        assert_eq!(
            OwnerAssignment::from_move(&m).unwrap().owner,
            "hyperpolymath/standards"
        );
        assert!(
            OwnerAssignment::from_move(&Move::ResolveAndRerun { branch: "x".into() }).is_none()
        );
    }

    #[test]
    fn report_with_new_sections_round_trips() {
        let report = Report {
            summary: "gate not yet won".into(),
            unsatisfied: vec![],
            moves_attempted: vec![],
            escalations: vec![Escalation {
                check: "lint-shell".into(),
                group: ExpertGroup::HypatiaFleet,
                obligation: EscalationKind::DispatchFix,
                evidence: "shellcheck".into(),
            }],
            owner_assignments: vec![OwnerAssignment {
                check: "gov".into(),
                owner: "hyperpolymath/standards".into(),
                disposition: OwnershipDisposition::OwnedUpstream {
                    repo: "hyperpolymath/standards".into(),
                },
                rationale: "reusable".into(),
            }],
            expert_verdicts: vec![ExpertVerdict {
                check: "lint-shell".into(),
                cartridge: "panic-attack-mcp".into(),
                tool: "panic_attack_scan".into(),
                ok: true,
                meaning: "weak-point scan".into(),
                verdict: r#"{"findings":[]}"#.into(),
            }],
            blockers: vec![],
        };
        let json = serde_json::to_string(&report).expect("serialise");
        let back: Report = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(report, back);
    }

    #[test]
    fn report_deserialises_without_new_sections_for_back_compat() {
        // Older reports had no escalations/owner_assignments/expert_verdicts;
        // #[serde(default)] must keep them parseable.
        let legacy = r#"{"summary":"x","unsatisfied":[],"moves_attempted":[],"blockers":[]}"#;
        let report: Report = serde_json::from_str(legacy).expect("legacy parse");
        assert!(report.escalations.is_empty());
        assert!(report.owner_assignments.is_empty());
        assert!(report.expert_verdicts.is_empty());
    }
}
