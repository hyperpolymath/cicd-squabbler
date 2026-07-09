// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble-core` — the detachable CI/CD fight engine.
//!
//! Core principle: **squabble ≠ bypass.** Every green the engine declares is
//! backed by the gate's own required check passing legitimately — never by an
//! admin override, an `enforce_admins` toggle, or removing/renaming-away a
//! required context. An admin-merge cheats past the gate; a squabble *wins* it.
//!
//! The engine is pure and host-agnostic: it `diagnose`s a [`gate::Gate`],
//! `propose`s a [`moves::Move`] for each unsatisfied requirement, and reports
//! progress as an [`outcome::Outcome`]. The side-effecting parts (git, `gh`,
//! GitHub Checks API) live in `squabble-cli` and `squabble-app`, which call
//! into this crate. This crate has **no** hypatia or estate dependency — the
//! detachability is the whole point.

pub mod gate;
pub mod moves;
pub mod outcome;

use gate::{Gate, GateState};
use moves::{LicenceFinding, Move};
use serde::{Deserialize, Serialize};

/// A diagnosis: the current gate state plus a proposed legitimate move per
/// unsatisfied requirement. The host decides whether to apply them.
/// Serialisable so hosts (CLI `--json`, the App's HTTP API) can emit it as
/// part of the evidence trail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnosis {
    pub state: GateState,
    pub proposed: Vec<Move>,
}

/// Diagnose a stuck gate and propose one legitimate move per unsatisfied
/// requirement. This is pure: it reads the gate and proposes; it applies
/// nothing. The proposal heuristic is deliberately conservative — when the
/// engine cannot recognise *why* a requirement is unsatisfied, it proposes
/// ground-truthing rather than guessing.
pub fn diagnose(gate: &Gate) -> Diagnosis {
    let state = gate.evaluate();
    let proposed = gate
        .unsatisfied()
        .map(|c| propose_for(&c.required_context))
        .collect();
    Diagnosis { state, proposed }
}

/// Map an unsatisfied required context to the most conservative legitimate
/// move. v0.1 starts every unknown deadlock at ground-truthing the names; the
/// host then re-diagnoses with the realised names bound.
fn propose_for(required_context: &str) -> Move {
    // A licence-policy gate is a distinct class: the resolution is doctrine-
    // constrained (author-allowed vs owner-manual), so route it to a
    // LicencePolicyDrift rather than generic check-name ground-truthing. The
    // pure engine cannot scan the tree, so it starts at NeedsGroundTruth; the
    // host's licence scanner re-diagnoses with the concrete finding.
    let lc = required_context.to_ascii_lowercase();
    if lc.contains("licence")
        || lc.contains("license")
        || lc.contains("spdx")
        || lc.contains("reuse")
    {
        return Move::LicencePolicyDrift {
            check: required_context.to_string(),
            finding: LicenceFinding::NeedsGroundTruth,
        };
    }
    Move::GroundTruthCheckNames {
        workflow: format!("(emitting `{required_context}`)"),
    }
}

/// Diagnose a stuck gate using host-supplied classification hints.
///
/// `diagnose` is deliberately name-only: [`gate::Gate`] carries nothing about
/// *why* a check is unsatisfied, so its default proposal falls back to
/// [`propose_for`]'s heuristics. A host with more context (fetched workflow
/// source, a check's own failure history, known-bad reusable-workflow
/// patterns — see `hyperpolymath/hypatia#566`) can supply a `hints` map from
/// `required_context` to the specific [`Move`] it has already classified,
/// and this function uses that instead of guessing. Contexts absent from
/// `hints` fall back to [`propose_for`], so this is a strict refinement of
/// `diagnose`, never a divergence from it.
pub fn diagnose_with_hints(
    gate: &Gate,
    hints: &std::collections::HashMap<String, Move>,
) -> Diagnosis {
    let state = gate.evaluate();
    let proposed = gate
        .unsatisfied()
        .map(|c| {
            hints
                .get(&c.required_context)
                .cloned()
                .unwrap_or_else(|| propose_for(&c.required_context))
        })
        .collect();
    Diagnosis { state, proposed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gate::{CheckRun, RequiredCheck};

    #[test]
    fn green_gate_proposes_nothing() {
        let g = Gate::new(vec![RequiredCheck::new("a", CheckRun::Passed)]);
        let d = diagnose(&g);
        assert_eq!(d.state, GateState::Green);
        assert!(d.proposed.is_empty());
    }

    #[test]
    fn blocked_gate_proposes_one_move_per_unsatisfied() {
        let g = Gate::new(vec![
            RequiredCheck::new("a", CheckRun::Passed),
            RequiredCheck::new("b", CheckRun::Missing),
            RequiredCheck::new("c", CheckRun::Pending),
        ]);
        let d = diagnose(&g);
        assert_eq!(d.state, GateState::Blocked);
        assert_eq!(d.proposed.len(), 2);
    }

    #[test]
    fn licence_policy_context_proposes_a_licence_drift_move() {
        let g = Gate::new(vec![
            RequiredCheck::new("build", CheckRun::Passed),
            RequiredCheck::new("licence-policy / spdx-headers", CheckRun::Missing),
        ]);
        let d = diagnose(&g);
        assert_eq!(d.state, GateState::Blocked);
        assert!(matches!(
            d.proposed.as_slice(),
            [Move::LicencePolicyDrift { .. }]
        ));
    }

    #[test]
    fn hinted_context_uses_the_hint_not_ground_truthing() {
        let g = Gate::new(vec![RequiredCheck::new(
            "governance / Validate Hypatia Baseline",
            CheckRun::Failed,
        )]);
        let mut hints = std::collections::HashMap::new();
        hints.insert(
            "governance / Validate Hypatia Baseline".to_string(),
            Move::FlagNonFunctionalGate {
                check: "governance / Validate Hypatia Baseline".into(),
                evidence: "main red for 5+ days independent of any PR".into(),
            },
        );
        let d = diagnose_with_hints(&g, &hints);
        assert_eq!(
            d.proposed,
            vec![Move::FlagNonFunctionalGate {
                check: "governance / Validate Hypatia Baseline".into(),
                evidence: "main red for 5+ days independent of any PR".into(),
            }]
        );
    }

    #[test]
    fn unhinted_context_still_falls_back_to_ground_truthing() {
        let g = Gate::new(vec![RequiredCheck::new(
            "some / other check",
            CheckRun::Missing,
        )]);
        let d = diagnose_with_hints(&g, &std::collections::HashMap::new());
        assert_eq!(
            d.proposed,
            vec![Move::GroundTruthCheckNames {
                workflow: "(emitting `some / other check`)".into()
            }]
        );
    }
}
