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
use moves::Move;

/// A diagnosis: the current gate state plus a proposed legitimate move per
/// unsatisfied requirement. The host decides whether to apply them.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    Move::GroundTruthCheckNames { workflow: format!("(emitting `{required_context}`)") }
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
}
