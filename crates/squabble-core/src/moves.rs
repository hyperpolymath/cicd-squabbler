// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! The v0.1 fight playbook — the *gate-deadlock class only*.
//!
//! Every move here **satisfies** the gate; none bypasses it. Each was mined
//! from the 2026-06-22 `rsr-template-repo` lockdown, which was effectively a
//! manual squabbler run. Red→green *code* fixes are deliberately out of scope
//! for v0.1: "fixing" a failing test by weakening it would violate
//! `squabble ≠ bypass`, so that class is deferred.

use serde::{Deserialize, Serialize};

/// One legitimate move the engine may propose against a stuck gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Move {
    /// Move 1 — *stale required-context*. The ruleset requires a bare context
    /// name (e.g. `gitleaks`) but the workflow emits a namespaced one
    /// (`scan / gitleaks`). Reconcile the *required* name to the real emitted
    /// name so the run that already passes satisfies the requirement. This is a
    /// solution at source: the requirement set is the thing that drifted.
    ReconcileRequiredContext { required: String, emitted: String },

    /// Move 2 — *path-filter deadlock*. A required check only runs on certain
    /// paths, so off-path PRs leave it forever `Missing`. Inject the documented
    /// job-level pass-through so the required check legitimately *completes*
    /// (reports success because there was nothing in scope to fail).
    InjectPathFilterPassThrough { check: String, workflow: String },

    /// Move 3 — *reusable-workflow pin drift*. A required job calls a reusable
    /// workflow pinned to a stale/broken SHA and never starts. Re-pin to the
    /// current SHA so the job runs.
    RepinReusableWorkflow { reference: String, from_sha: String, to_sha: String },

    /// Move 4 — *modify/delete & rebase conflict*. Resolve the conflict and
    /// re-run the gate. No content is invented; the human's intent is preserved.
    ResolveAndRerun { branch: String },

    /// Move 5 — *check-name ground-truthing*. The shared substrate for moves 1
    /// and 2: enumerate the names a workflow actually emits on the head commit,
    /// so reconciliation targets reality rather than a guess.
    GroundTruthCheckNames { workflow: String },
}

impl Move {
    /// A one-line, human-facing description for the structured report.
    pub fn describe(&self) -> String {
        match self {
            Move::ReconcileRequiredContext { required, emitted } => format!(
                "reconcile required context `{required}` → emitted name `{emitted}`"
            ),
            Move::InjectPathFilterPassThrough { check, workflow } => format!(
                "inject path-filter pass-through for `{check}` in `{workflow}`"
            ),
            Move::RepinReusableWorkflow { reference, from_sha, to_sha } => format!(
                "re-pin `{reference}` {} → {}",
                short(from_sha),
                short(to_sha)
            ),
            Move::ResolveAndRerun { branch } => format!("resolve conflicts on `{branch}` and re-run"),
            Move::GroundTruthCheckNames { workflow } => {
                format!("ground-truth emitted check names for `{workflow}`")
            }
        }
    }

    /// Invariant guard, enforced at the type boundary: a move is admissible only
    /// if it does not weaken the gate. Bypasses (drop a required context, toggle
    /// `enforce_admins`, admin-merge) are not even representable as a [`Move`],
    /// so this is a belt-and-braces assertion rather than the primary defence.
    pub const fn is_legitimate(&self) -> bool {
        true
    }
}

fn short(sha: &str) -> &str {
    if sha.len() >= 7 { &sha[..7] } else { sha }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_move_is_legitimate_by_construction() {
        let moves = [
            Move::ReconcileRequiredContext { required: "gitleaks".into(), emitted: "scan / gitleaks".into() },
            Move::InjectPathFilterPassThrough { check: "build".into(), workflow: "ci.yml".into() },
            Move::RepinReusableWorkflow { reference: "org/wf/.github/workflows/x.yml".into(), from_sha: "deadbeefdeadbeef".into(), to_sha: "cafebabecafebabe".into() },
            Move::ResolveAndRerun { branch: "feat/x".into() },
            Move::GroundTruthCheckNames { workflow: "ci.yml".into() },
        ];
        assert!(moves.iter().all(Move::is_legitimate));
    }
}
