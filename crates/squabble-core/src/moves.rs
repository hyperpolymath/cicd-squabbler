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
    RepinReusableWorkflow {
        reference: String,
        from_sha: String,
        to_sha: String,
    },

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
    LicencePolicyDrift {
        check: String,
        finding: LicenceFinding,
    },

    /// Move 7 — *non-functional gate*. The required check's own script fails
    /// unconditionally regardless of any exemption/baseline file present — it
    /// cannot be won by any change on the PR side, because the deadlock lives
    /// in the shared check's implementation, not in the PR. Mined from
    /// `hyperpolymath/hypatia#566` (2026-07-01): `standards`'s
    /// `validate-hypatia-baseline` job never read `.hypatia-baseline.json`'s
    /// content, so it failed on every PR regardless of what was already
    /// accepted — confirmed by `main` itself failing the same check for 5+
    /// days independent of any PR. Not auto-appliable: fixing the check
    /// requires human review of the shared reusable workflow it lives in.
    /// This move's job is *recognition* — stop re-diagnosing the same
    /// unfixable deadlock as `GroundTruthCheckNames` on every run — and
    /// *escalation*, so the host can hand the human a report naming the
    /// check and the evidence rather than proposing a PR-side move that can
    /// never win.
    FlagNonFunctionalGate { check: String, evidence: String },

    /// Move 8 — *escalate to an expert group*. The failing check is genuinely
    /// red for a reason **outside the squabbler's lane** — an application/code
    /// fix (a `red→green code-fixer` is what this repo explicitly IS-NOT), a
    /// proof obligation, or a security finding that needs a real scanner. Hand
    /// it to a named [`ExpertGroup`] (in the estate, these resolve to boj-server
    /// cartridges: hypatia-mcp / fleet-mcp / echidna / panic-attack) with an
    /// [`EscalationKind`] and evidence.
    ///
    /// This is a **hand-off, never a win**: applying it constructs no
    /// [`crate::gate::CheckRun::Passed`] and drops no required context, so it
    /// cannot move the gate to [`crate::gate::GateState::Green`] by itself. It
    /// exists so the squabbler can assemble the case for its "big guns" instead
    /// of either faking a green or silently giving up (`fail-closed`,
    /// `no-silent-skip`). If the expert group is unreachable, the host still
    /// records this move as fail-closed evidence in the report.
    EscalateToExpert {
        check: String,
        group: ExpertGroup,
        obligation: EscalationKind,
        evidence: String,
    },

    /// Move 9 — *assign a gate owner*. Surface a check that either **should be
    /// in CI but is not yet** anyone's responsibility, or is **structurally
    /// misconfigured** (a path-filtered required check that strands every PR as
    /// `Expected`, a duplicate of an upstream gate, a reusable workflow owned in
    /// another repo). Name who is responsible via [`OwnershipDisposition`], with
    /// a one-line rationale.
    ///
    /// This is **pure annotation, never a path to green**: it satisfies no
    /// check and weakens no requirement. It is how the squabbler "argues the
    /// wider case" — putting into the debate the things that *should* be checked
    /// and calling up whoever must own them — without ever bypassing the gate.
    /// A check left to an upstream owner stays unsatisfied until that owner
    /// acts; it is never "won" by re-attributing it.
    AssignGateOwner {
        check: String,
        owner: String,
        disposition: OwnershipDisposition,
        rationale: String,
    },
}

/// A specialist group the squabbler can summon when a gate is out of its lane.
/// The squabbler never embeds these; in the estate each maps to an existing
/// boj-server MCP cartridge (or a bundle of them), so calling in the "big guns"
/// stays lightweight. The mapping itself lives in the host, keeping
/// `squabble-core` free of any estate/boj dependency (detachability).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExpertGroup {
    /// Analysis only — confidence scoring, recipe lookup, dispatch strategy
    /// (estate: `ci-cd/hypatia-mcp`).
    Hypatia,
    /// Analyse **and** actuate — hypatia's judgement plus the fleet's fixers and
    /// PR-openers (estate: `hypatia-mcp` + `fleet/fleet-mcp` + robot-repo-automaton).
    HypatiaFleet,
    /// Formal proof / verification obligations (estate: `formal-verification/echidna-llm-mcp`).
    Proof,
    /// Static-analysis / weak-point scanning (estate: `security/panic-attack-mcp`).
    Security,
}

/// What the squabbler is asking an [`ExpertGroup`] to *do*. Kept coarse on
/// purpose: the squabbler states the obligation, the host/cartridge decides how.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EscalationKind {
    /// "Is this a known pattern, and how confident should we be in a fix?"
    AssessConfidence,
    /// "Fix this (code/config outside my lane) and open the PR."
    DispatchFix,
    /// "Verify this claim/obligation holds."
    VerifyClaim,
    /// "Scan for the real underlying weak points."
    Scan,
}

/// Who is responsible for a surfaced check. `MisconfiguredGate` and
/// `OwnedUpstream` carry the concrete detail so the report is actionable
/// (`evidence-per-step`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "kebab-case")]
pub enum OwnershipDisposition {
    /// A check that *ought* to run here but no workflow provides it yet — the
    /// squabbler puts it into the debate for someone to own.
    ShouldBeAddedToCi,
    /// The check runs but its own configuration strands PRs (e.g. a path-filter
    /// on a required check, or a duplicate of an upstream gate).
    MisconfiguredGate { detail: String },
    /// The check is produced by a reusable workflow owned in another repo; the
    /// fix belongs there, not on this PR.
    OwnedUpstream { repo: String },
    /// No current owner could be attributed — surfaced rather than swallowed.
    Ownerless,
}

impl ExpertGroup {
    /// A short human label naming the group and its remit.
    pub const fn label(self) -> &'static str {
        match self {
            ExpertGroup::Hypatia => "hypatia (analysis)",
            ExpertGroup::HypatiaFleet => "hypatia+fleet (analyse+fix)",
            ExpertGroup::Proof => "proof (echidna)",
            ExpertGroup::Security => "security (panic-attack)",
        }
    }
}

impl EscalationKind {
    pub const fn label(self) -> &'static str {
        match self {
            EscalationKind::AssessConfidence => "assess-confidence",
            EscalationKind::DispatchFix => "dispatch-fix",
            EscalationKind::VerifyClaim => "verify-claim",
            EscalationKind::Scan => "scan",
        }
    }
}

impl OwnershipDisposition {
    /// A short kebab-case label for the disposition (for compact report lines).
    pub const fn label(&self) -> &'static str {
        match self {
            OwnershipDisposition::ShouldBeAddedToCi => "should-be-added-to-ci",
            OwnershipDisposition::MisconfiguredGate { .. } => "misconfigured-gate",
            OwnershipDisposition::OwnedUpstream { .. } => "owned-upstream",
            OwnershipDisposition::Ownerless => "ownerless",
        }
    }

    fn describe(&self) -> String {
        match self {
            OwnershipDisposition::ShouldBeAddedToCi => "should be added to CI".to_string(),
            OwnershipDisposition::MisconfiguredGate { detail } => {
                format!("misconfigured gate: {detail}")
            }
            OwnershipDisposition::OwnedUpstream { repo } => format!("owned upstream in `{repo}`"),
            OwnershipDisposition::Ownerless => "no current owner".to_string(),
        }
    }
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
    HeaderDrift {
        files: Vec<String>,
        expected: String,
    },
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
            Move::ReconcileRequiredContext { required, emitted } => {
                format!("reconcile required context `{required}` → emitted name `{emitted}`")
            }
            Move::InjectPathFilterPassThrough { check, workflow } => {
                format!("inject path-filter pass-through for `{check}` in `{workflow}`")
            }
            Move::RepinReusableWorkflow {
                reference,
                from_sha,
                to_sha,
            } => format!(
                "re-pin `{reference}` {} → {}",
                short(from_sha),
                short(to_sha)
            ),
            Move::ResolveAndRerun { branch } => {
                format!("resolve conflicts on `{branch}` and re-run")
            }
            Move::GroundTruthCheckNames { workflow } => {
                format!("ground-truth emitted check names for `{workflow}`")
            }
            Move::LicencePolicyDrift { check, finding } => format!(
                "licence-policy `{check}`: {} [{}]",
                finding.describe(),
                finding.resolution().describe()
            ),
            Move::FlagNonFunctionalGate { check, evidence } => format!(
                "flag `{check}` as non-functional (unfixable from the PR side) — {evidence}"
            ),
            Move::EscalateToExpert {
                check,
                group,
                obligation,
                evidence,
            } => format!(
                "escalate `{check}` to {} [{}] — {evidence}",
                group.label(),
                obligation.label()
            ),
            Move::AssignGateOwner {
                check,
                owner,
                disposition,
                rationale,
            } => format!(
                "assign `{check}` → `{owner}` [{}] — {rationale}",
                disposition.describe()
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
    if sha.len() >= 7 {
        &sha[..7]
    } else {
        sha
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_move_is_legitimate_by_construction() {
        let moves = [
            Move::ReconcileRequiredContext {
                required: "gitleaks".into(),
                emitted: "scan / gitleaks".into(),
            },
            Move::InjectPathFilterPassThrough {
                check: "build".into(),
                workflow: "ci.yml".into(),
            },
            Move::RepinReusableWorkflow {
                reference: "org/wf/.github/workflows/x.yml".into(),
                from_sha: "deadbeefdeadbeef".into(),
                to_sha: "cafebabecafebabe".into(),
            },
            Move::ResolveAndRerun {
                branch: "feat/x".into(),
            },
            Move::GroundTruthCheckNames {
                workflow: "ci.yml".into(),
            },
            Move::LicencePolicyDrift {
                check: "licence-policy / spdx-headers".into(),
                finding: LicenceFinding::NeedsGroundTruth,
            },
            Move::FlagNonFunctionalGate {
                check: "validate-hypatia-baseline".into(),
                evidence: "main has failed this check for 5+ days independent of any PR".into(),
            },
            Move::EscalateToExpert {
                check: "lint-shell".into(),
                group: ExpertGroup::HypatiaFleet,
                obligation: EscalationKind::DispatchFix,
                evidence: "ShellCheck findings in src/scripts/*.sh — a code fix, out of lane"
                    .into(),
            },
            Move::AssignGateOwner {
                check: "governance / Well-Known (RFC 9116 + RSR)".into(),
                owner: "hyperpolymath/standards".into(),
                disposition: OwnershipDisposition::OwnedUpstream {
                    repo: "hyperpolymath/standards".into(),
                },
                rationale: "produced by the reusable governance workflow; fix belongs upstream"
                    .into(),
            },
        ];
        assert!(moves.iter().all(Move::is_legitimate));
    }

    #[test]
    fn escalation_and_owner_moves_round_trip() {
        // The two "call in the big guns / name an owner" moves must survive the
        // JSON evidence manifest unchanged.
        let escalate = Move::EscalateToExpert {
            check: "container-build".into(),
            group: ExpertGroup::HypatiaFleet,
            obligation: EscalationKind::DispatchFix,
            evidence: "podman build fails; needs a real fix".into(),
        };
        let assign = Move::AssignGateOwner {
            check: "governance / Workflow security linter".into(),
            owner: "hyperpolymath/standards".into(),
            disposition: OwnershipDisposition::MisconfiguredGate {
                detail: "duplicates the local lint-workflows gate".into(),
            },
            rationale: "consolidate upstream".into(),
        };
        for m in [escalate, assign] {
            let json = serde_json::to_string(&m).expect("serialise");
            let back: Move = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(m, back);
        }
    }

    #[test]
    fn escalation_move_names_group_and_obligation_in_kebab_case() {
        let json = serde_json::to_string(&Move::EscalateToExpert {
            check: "c".into(),
            group: ExpertGroup::HypatiaFleet,
            obligation: EscalationKind::VerifyClaim,
            evidence: "e".into(),
        })
        .unwrap();
        assert!(json.contains("\"kind\":\"escalate-to-expert\""));
        assert!(json.contains("\"group\":\"hypatia-fleet\""));
        assert!(json.contains("\"obligation\":\"verify-claim\""));
    }

    #[test]
    fn licence_resolution_respects_the_manual_only_rule() {
        // Header drift on existing files is relicensing → owner-manual (flag,
        // never auto-edit); a missing canonical text is authoring → applyable.
        assert_eq!(
            LicenceFinding::HeaderDrift {
                files: vec!["README.md".into()],
                expected: "CC-BY-SA-4.0".into()
            }
            .resolution(),
            LicenceResolution::OwnerManual
        );
        assert_eq!(
            LicenceFinding::MissingLicenceText {
                missing: vec!["CC-BY-SA-4.0.txt".into()]
            }
            .resolution(),
            LicenceResolution::AuthorAllowed
        );
        assert_eq!(
            LicenceFinding::RootDisplayMismatch {
                found: "AGPL-3.0".into()
            }
            .resolution(),
            LicenceResolution::AuthorNewRepoElseOwnerManual
        );
        assert_eq!(
            LicenceFinding::NeedsGroundTruth.resolution(),
            LicenceResolution::Unknown
        );
    }

    #[test]
    fn licence_drift_move_serialises_round_trip() {
        let m = Move::LicencePolicyDrift {
            check: "licence-policy / reuse-complete".into(),
            finding: LicenceFinding::MissingLicenceText {
                missing: vec!["CC-BY-SA-4.0.txt".into()],
            },
        };
        let json = serde_json::to_string(&m).expect("serialise");
        let back: Move = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(m, back);
    }
}
