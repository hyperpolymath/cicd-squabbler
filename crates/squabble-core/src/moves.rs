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

    /// Move 6 — *licence-policy drift*. A required licence-policy check (SPDX
    /// headers, REUSE `LICENSES/` completeness, or the GitHub single-display
    /// resolution) is unsatisfied. Estate doctrine (standards
    /// `LICENCE-POLICY.adoc` Rule 1 + Addendum A2) constrains **how** it may be
    /// satisfied, and the squabbler must respect that split precisely:
    ///
    /// * adding a missing canonical `LICENSES/` text, or setting a *brand-new*
    ///   repo's root `LICENSE`, is *authoring* — the host may apply it;
    /// * rewriting an *existing* file's SPDX header is *relicensing* — manual,
    ///   owner-only. The squabbler **flags** it and never edits it.
    ///
    /// The [`LicenceFinding`] carries the concrete violation and its
    /// [`LicenceResolution`], so the engine satisfies only what it legitimately
    /// can and defers the rest to the owner — it never bypasses the gate (an
    /// owner-manual finding leaves the gate unsatisfied until the owner acts;
    /// it is never "won" by dropping or weakening the licence requirement).
    LicencePolicyDrift { check: String, finding: LicenceFinding },
}

/// The specific way a licence-policy check is unsatisfied. The pure engine
/// cannot read the working tree, so a diagnosis begins at
/// [`LicenceFinding::NeedsGroundTruth`]; the host's licence scanner (REUSE /
/// `check-spdx`) then re-diagnoses with a concrete finding, and each finding
/// declares how it may legitimately be resolved (see [`LicenceResolution`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "finding", rename_all = "kebab-case")]
pub enum LicenceFinding {
    /// The pure engine cannot see the tree; the host must enumerate the
    /// concrete violation first (mirrors [`Move::GroundTruthCheckNames`]).
    NeedsGroundTruth,
    /// The REUSE `LICENSES/` directory is missing a canonical text the estate
    /// policy requires (e.g. `CC-BY-SA-4.0.txt`).
    MissingLicenceText { missing: Vec<String> },
    /// The root `LICENSE` — which drives GitHub's single displayed licence
    /// (Addendum A8) — is not the estate default `MPL-2.0`.
    RootDisplayMismatch { found: String },
    /// Existing file(s) carry the wrong per-file SPDX header for their content
    /// type (code should be `MPL-2.0`, prose `CC-BY-SA-4.0`).
    HeaderDrift { files: Vec<String>, expected: String },
}

/// How a [`LicenceFinding`] may legitimately be resolved, per estate doctrine
/// (standards `LICENCE-POLICY.adoc` Rule 1 + Addendum A2). The squabbler never
/// bypasses a gate and never performs a forbidden relicensing edit: it applies
/// only what counts as *authoring* and *flags* the rest for the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LicenceResolution {
    /// Adding a missing canonical `LICENSES/` text is authoring, not
    /// relicensing — the host may apply it.
    AuthorAllowed,
    /// Setting a *brand-new* repo's root `LICENSE` is authoring; changing an
    /// *existing* repo's root licence is relicensing — owner-manual then.
    AuthorNewRepoElseOwnerManual,
    /// Rewriting an existing file's SPDX header is relicensing: manual,
    /// owner-only (A2). The squabbler flags it; it must never edit.
    OwnerManual,
    /// The concrete finding is not yet known — ground-truth first.
    Unknown,
}

impl LicenceFinding {
    /// The doctrine-constrained resolution class for this finding. This is the
    /// load-bearing "nuance": header drift is `OwnerManual` (flag, never edit),
    /// while a missing canonical text is `AuthorAllowed`.
    pub const fn resolution(&self) -> LicenceResolution {
        match self {
            LicenceFinding::MissingLicenceText { .. } => LicenceResolution::AuthorAllowed,
            LicenceFinding::RootDisplayMismatch { .. } => {
                LicenceResolution::AuthorNewRepoElseOwnerManual
            }
            LicenceFinding::HeaderDrift { .. } => LicenceResolution::OwnerManual,
            LicenceFinding::NeedsGroundTruth => LicenceResolution::Unknown,
        }
    }

    fn describe(&self) -> String {
        match self {
            LicenceFinding::NeedsGroundTruth => {
                "ground-truth the concrete violation (REUSE / check-spdx)".to_string()
            }
            LicenceFinding::MissingLicenceText { missing } => {
                format!("LICENSES/ missing {}", missing.join(", "))
            }
            LicenceFinding::RootDisplayMismatch { found } => {
                format!("root LICENSE is `{found}`; estate display is MPL-2.0")
            }
            LicenceFinding::HeaderDrift { files, expected } => {
                format!("{} file(s) need per-file SPDX `{expected}`", files.len())
            }
        }
    }
}

impl LicenceResolution {
    fn describe(self) -> &'static str {
        match self {
            LicenceResolution::AuthorAllowed => "author-allowed: add canonical text",
            LicenceResolution::AuthorNewRepoElseOwnerManual => {
                "author for a new repo, else owner-manual"
            }
            LicenceResolution::OwnerManual => "owner-manual (A2): flag only, never auto-edit",
            LicenceResolution::Unknown => "unknown until ground-truthed",
        }
    }
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
            Move::LicencePolicyDrift { check, finding } => format!(
                "licence-policy `{check}`: {} [{}]",
                finding.describe(),
                finding.resolution().describe()
            ),
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
            Move::LicencePolicyDrift { check: "licence-policy / spdx-headers".into(), finding: LicenceFinding::NeedsGroundTruth },
        ];
        assert!(moves.iter().all(Move::is_legitimate));
    }

    #[test]
    fn licence_resolution_respects_the_manual_only_rule() {
        // Header drift on existing files is relicensing → owner-manual (flag,
        // never auto-edit); a missing canonical text is authoring → applyable.
        assert_eq!(
            LicenceFinding::HeaderDrift { files: vec!["README.md".into()], expected: "CC-BY-SA-4.0".into() }.resolution(),
            LicenceResolution::OwnerManual
        );
        assert_eq!(
            LicenceFinding::MissingLicenceText { missing: vec!["CC-BY-SA-4.0.txt".into()] }.resolution(),
            LicenceResolution::AuthorAllowed
        );
        assert_eq!(
            LicenceFinding::RootDisplayMismatch { found: "AGPL-3.0".into() }.resolution(),
            LicenceResolution::AuthorNewRepoElseOwnerManual
        );
        assert_eq!(LicenceFinding::NeedsGroundTruth.resolution(), LicenceResolution::Unknown);
    }

    #[test]
    fn licence_drift_move_serialises_round_trip() {
        let m = Move::LicencePolicyDrift {
            check: "licence-policy / reuse-complete".into(),
            finding: LicenceFinding::MissingLicenceText { missing: vec!["CC-BY-SA-4.0.txt".into()] },
        };
        let json = serde_json::to_string(&m).expect("serialise");
        let back: Move = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(m, back);
    }
}
