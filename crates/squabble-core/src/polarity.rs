// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! The **green-polarity** classifier — the gap `fight` cannot see.
//!
//! `squabble fight` classifies only *red* checks. A gate that could not run
//! reports **green**, so the engine never inspects it. That is the whole
//! fake-green class: a scanner goes missing, a stub writes `[]`, the check goes
//! green, and the gate silently stops being a gate.
//!
//! This module classifies a check that concluded `success` as one of:
//!
//! * [`PolarityVerdict::Genuine`] — it really ran,
//! * [`PolarityVerdict::NotApplicable`] — it is correctly inapplicable *by
//!   declaration* (never an escalation; inapplicable is not broken),
//! * [`PolarityVerdict::Vacuous`] — it went green without doing its job.
//!
//! # Evidence tier
//!
//! Every variant here is witnessable from **one** source: the Actions jobs API
//! step conclusions (`.steps[].{name,conclusion}`). Causes that would need the
//! workflow YAML or a script's source are deliberately absent — a variant we
//! cannot witness from the declared source would be a taxonomy, not a
//! classifier.
//!
//! # Standalone
//!
//! `cicd-squabbler`'s ANCHOR declares `hypatia-dependent` as an **IS-NOT**, and
//! the `gate_triage` directive sets `fallback-must-be-standalone = true`. So the
//! signature is *host-supplied* ([`VacuitySignature`], read from
//! `gate_triage.a2ml`) and no scanner name appears anywhere in this file.
//!
//! # Why the SPARK theorem is untouched
//!
//! `spark/src/gate_machine.ads` models only `Check_Run`, `Gate_State` and
//! `Evaluate`, whose postcondition is `Green IFF non-empty AND all Passed`. It
//! does not mirror this crate's check annotations. Nothing here calls
//! [`crate::gate::Gate::evaluate`] or changes a [`crate::gate::CheckRun`], so
//! the proved invariant holds bit-for-bit — the same precedent that admitted
//! `MissingCause`.

use serde::{Deserialize, Serialize};

use crate::moves::{EscalationKind, ExpertGroup, Move};

/// A single step's conclusion as reported by the Actions jobs API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepConclusion {
    Success,
    Skipped,
    Failure,
    Cancelled,
    /// Anything the API reports that we do not model (including `null`).
    Other,
}

impl StepConclusion {
    /// Parse the jobs-API string. Unknown and absent values are `Other` rather
    /// than an error: an unmodelled conclusion must never be mistaken for a
    /// skip, because a skip is half of the vacuity signature.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("success") => StepConclusion::Success,
            Some("skipped") => StepConclusion::Skipped,
            Some("failure") => StepConclusion::Failure,
            Some("cancelled") => StepConclusion::Cancelled,
            _ => StepConclusion::Other,
        }
    }
}

/// One step of a job that concluded `success`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepOutcome {
    pub name: String,
    pub conclusion: StepConclusion,
}

impl StepOutcome {
    pub fn new(name: impl Into<String>, conclusion: StepConclusion) -> Self {
        StepOutcome {
            name: name.into(),
            conclusion,
        }
    }
}

/// The host-supplied vacuity fingerprint, read from the `gate_triage.a2ml`
/// keys `signature-skipped-steps` and `signature-success-steps`.
///
/// **Scanner-agnostic by construction**: no step name is compiled in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VacuitySignature {
    /// Step names that must have concluded `skipped` — "the real work did not run".
    pub skipped_steps: Vec<String>,
    /// Step names that must have concluded `success` — "something wrote a stub".
    pub success_steps: Vec<String>,
}

impl VacuitySignature {
    pub fn new(skipped: &[&str], success: &[&str]) -> Self {
        VacuitySignature {
            skipped_steps: skipped.iter().map(|s| (*s).to_string()).collect(),
            success_steps: success.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// A signature is *usable* only when **both** halves are populated.
    ///
    /// This is the false-positive guard. A skipped scan step with no
    /// corresponding stub-writing step is a legitimately optional step, not
    /// vacuity — and an empty list would match vacuously, turning every green
    /// into a finding.
    pub fn is_usable(&self) -> bool {
        !self.skipped_steps.is_empty() && !self.success_steps.is_empty()
    }
}

/// What a gate declares about *where it applies* — the directive's
/// `@gitforge_OperatorType` / `@channel` axis.
///
/// Empty lists mean **undeclared**, which is not the same as unmatched.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applicability {
    pub runs_for_operator_types: Vec<String>,
    pub runs_on_channels: Vec<String>,
}

impl Applicability {
    /// True when the gate carries no applicability predicate at all. Today no
    /// gate does, so this is the common case — and treating it as
    /// `NotApplicable` would make this classifier its own fake green.
    pub fn is_undeclared(&self) -> bool {
        self.runs_for_operator_types.is_empty() && self.runs_on_channels.is_empty()
    }
}

/// What the repo declares about itself, read from `0.1-AI-MANIFEST.a2ml`.
/// `None` on either field means the manifest is silent on that key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoDeclaration {
    pub operator_type: Option<String>,
    pub channel: Option<String>,
}

/// The four evidence fields issue #58 requires of every verdict.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// How many runs of this check were inspected.
    pub run_count: u32,
    /// Fraction of those runs that were stubbed, in `0.0..=1.0`.
    pub stub_rate: f64,
    /// Does the tool this gate invokes exist upstream at all? `None` when the
    /// host could not determine it — never silently `true`.
    pub upstream_exists: Option<bool>,
    /// Is the technology this gate scans for still present in the tree? `None`
    /// when unmeasured: no gate today declares the paths/globs that would make
    /// this computable, and claiming `true` would be an overclaim.
    pub target_tech_present: Option<bool>,
}

/// Render a tri-state honestly. "unmeasured" is a first-class answer.
fn tri(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "true",
        Some(false) => "false",
        None => "unmeasured",
    }
}

impl Evidence {
    /// Rendered into [`Move::EscalateToExpert::evidence`] so the owner can make
    /// the case-2 call the squabbler is forbidden to make for them.
    pub fn describe(&self) -> String {
        format!(
            "run-count={} stub-rate={:.2} upstream-exists={} target-tech-present={}",
            self.run_count,
            self.stub_rate,
            tri(self.upstream_exists),
            tri(self.target_tech_present)
        )
    }
}

/// Why a green check is not really green. **Step-observable only.**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VacuityCause {
    /// The directive's signature matched: the real step was skipped *and* a
    /// stub-writing step succeeded. (Measured in the wild: 4 repos.)
    StubbedAfterSkippedScan,
    /// The job concluded success with steps recorded, every one of them skipped.
    AllStepsSkipped,
    /// The job concluded success having recorded no steps at all.
    NoStepsRecorded,
}

impl VacuityCause {
    pub const fn label(self) -> &'static str {
        match self {
            VacuityCause::StubbedAfterSkippedScan => "stubbed after a skipped scan",
            VacuityCause::AllStepsSkipped => "every step skipped",
            VacuityCause::NoStepsRecorded => "no steps recorded",
        }
    }
}

/// What the squabbler recommends the owner *do*. These are the computable
/// subset of the directive's five cases.
///
/// Case 2 (`useless-everywhere` → assess-value-then-delete) is **absent on
/// purpose**: the directive reserves that assessment to the owner, and it is
/// not decidable from a stub-rate. With `run_count == 1` the rate is exactly
/// `0.0` or `1.0`, so a single stubbed run would read as "useless everywhere".
/// The rate is reported as evidence instead.
///
/// Case 4 (`missing-local-tooling`, the trufflehog/pre-push class) is also
/// absent: it is not observable from the jobs API, so folding it in here would
/// be a claim this detector cannot witness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Recommendation {
    /// Case 1 — the technology this gate scans for is gone. Computable.
    RecommendRemoval,
    /// Case 5 — the tool is declared but does not exist upstream.
    /// "Should never happen"; the declaration is what is wrong.
    FixTheDeclaration,
    /// Case 3 — the gate is valuable but does not resolve here. Make it work.
    MakeItGreatInPractice,
}

impl Recommendation {
    pub const fn label(self) -> &'static str {
        match self {
            Recommendation::RecommendRemoval => "recommend-removal",
            Recommendation::FixTheDeclaration => "fix-the-declaration",
            Recommendation::MakeItGreatInPractice => "make-it-great-in-practice",
        }
    }

    /// Select the case from the four required evidence fields. Total function.
    pub const fn from_evidence(e: &Evidence) -> Self {
        match (e.target_tech_present, e.upstream_exists) {
            // Case 1 — measured absent. Only a measurement may recommend removal.
            (Some(false), _) => Recommendation::RecommendRemoval,
            // Case 5 — the tool is declared but is not there.
            (_, Some(false)) => Recommendation::FixTheDeclaration,
            // Case 3 — including every unmeasured combination. Unmeasured must
            // fall to the non-destructive recommendation, and stub_rate never
            // enters: see the type doc.
            _ => Recommendation::MakeItGreatInPractice,
        }
    }
}

/// The verdict on one check that concluded `success`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum PolarityVerdict {
    /// The check ran and its green means what it says.
    Genuine,
    /// Correctly inapplicable *by declaration*. Never escalated: inapplicable
    /// is not broken (`unmatched-is-vacuous = false`).
    NotApplicable { declaration: String },
    /// Green without doing its job.
    Vacuous {
        cause: VacuityCause,
        recommendation: Recommendation,
        evidence: Evidence,
    },
}

impl PolarityVerdict {
    /// The consumer. An enum nothing reads is itself a fake gate, so every
    /// `Vacuous` verdict projects to a move the report already prints.
    ///
    /// The move is [`Move::EscalateToExpert`], which by its own contract
    /// promotes no check and drops no required context — so surfacing a vacuous
    /// gate can never itself move the gate to green. `Genuine` and
    /// `NotApplicable` project to nothing.
    pub fn to_move(&self, check: &str) -> Option<Move> {
        match self {
            PolarityVerdict::Genuine | PolarityVerdict::NotApplicable { .. } => None,
            PolarityVerdict::Vacuous {
                cause,
                recommendation,
                evidence,
            } => Some(Move::EscalateToExpert {
                check: check.to_string(),
                group: ExpertGroup::GateTriage,
                obligation: EscalationKind::AssessConfidence,
                evidence: format!(
                    "green but vacuous ({}) — {} [{}]",
                    cause.label(),
                    evidence.describe(),
                    recommendation.label()
                ),
            }),
        }
    }
}

/// Does `steps` contain `name` with exactly `want`?
fn step_concluded(steps: &[StepOutcome], name: &str, want: StepConclusion) -> bool {
    steps
        .iter()
        .any(|s| s.name.trim() == name.trim() && s.conclusion == want)
}

/// Axis 0 — applicability, checked **first** and three-way.
///
/// * gate undeclared → `None` (fall through to the signature),
/// * gate declared and repo matches → `None` (fall through),
/// * gate declared and repo **contradicts** it → `Some(NotApplicable)`.
///
/// A repo silent on the key cannot contradict anything, so it falls through.
/// This is fail-open into a path that can only *escalate*, never green.
fn applicability_verdict(
    applicability: &Applicability,
    declared: &RepoDeclaration,
) -> Option<PolarityVerdict> {
    if applicability.is_undeclared() {
        return None;
    }

    if let Some(op) = declared.operator_type.as_deref() {
        if !applicability.runs_for_operator_types.is_empty()
            && !applicability
                .runs_for_operator_types
                .iter()
                .any(|t| t == op)
        {
            return Some(PolarityVerdict::NotApplicable {
                declaration: format!(
                    "@gitforge_OperatorType={op} not in [{}]",
                    applicability.runs_for_operator_types.join(", ")
                ),
            });
        }
    }

    if let Some(ch) = declared.channel.as_deref() {
        if !applicability.runs_on_channels.is_empty()
            && !applicability.runs_on_channels.iter().any(|c| c == ch)
        {
            return Some(PolarityVerdict::NotApplicable {
                declaration: format!(
                    "@channel={ch} not in [{}]",
                    applicability.runs_on_channels.join(", ")
                ),
            });
        }
    }

    None
}

/// Classify one check that concluded `success`.
///
/// Never call this on a red check — `fight` already handles those, and this
/// module has no opinion about them.
pub fn classify(
    steps: &[StepOutcome],
    signature: &VacuitySignature,
    applicability: &Applicability,
    declared: &RepoDeclaration,
    evidence: Evidence,
) -> PolarityVerdict {
    // Axis 0 first: inapplicable is not broken.
    if let Some(v) = applicability_verdict(applicability, declared) {
        return v;
    }

    let cause = if steps.is_empty() {
        Some(VacuityCause::NoStepsRecorded)
    } else if signature.is_usable()
        // CONJUNCTION. A partial match is a legitimately optional step.
        && signature
            .skipped_steps
            .iter()
            .all(|n| step_concluded(steps, n, StepConclusion::Skipped))
        && signature
            .success_steps
            .iter()
            .all(|n| step_concluded(steps, n, StepConclusion::Success))
    {
        Some(VacuityCause::StubbedAfterSkippedScan)
    } else if steps.iter().all(|s| s.conclusion == StepConclusion::Skipped) {
        Some(VacuityCause::AllStepsSkipped)
    } else {
        None
    };

    match cause {
        None => PolarityVerdict::Genuine,
        Some(cause) => PolarityVerdict::Vacuous {
            cause,
            recommendation: Recommendation::from_evidence(&evidence),
            evidence,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{CheckRun, Gate, GateState, RequiredCheck};

    /// The measured real-world signature, supplied the way the host supplies
    /// it — as data, never compiled in.
    fn sig() -> VacuitySignature {
        VacuitySignature::new(&["Run Hypatia scan"], &["Create stub findings"])
    }

    fn ev(tech: bool, upstream: bool, rate: f64) -> Evidence {
        Evidence {
            run_count: 4,
            stub_rate: rate,
            upstream_exists: Some(upstream),
            target_tech_present: Some(tech),
        }
    }

    fn classify_steps(steps: &[StepOutcome]) -> PolarityVerdict {
        classify(
            steps,
            &sig(),
            &Applicability::default(),
            &RepoDeclaration::default(),
            ev(true, true, 1.0),
        )
    }

    // ---- the signature is a CONJUNCTION -------------------------------------

    #[test]
    fn both_halves_matching_is_vacuous() {
        let v = classify_steps(&[
            StepOutcome::new("Checkout", StepConclusion::Success),
            StepOutcome::new("Run Hypatia scan", StepConclusion::Skipped),
            StepOutcome::new("Create stub findings", StepConclusion::Success),
        ]);
        assert!(
            matches!(
                v,
                PolarityVerdict::Vacuous {
                    cause: VacuityCause::StubbedAfterSkippedScan,
                    ..
                }
            ),
            "got {v:?}"
        );
    }

    #[test]
    fn skipped_step_without_a_stub_step_is_genuine() {
        // The false-positive guard. A skipped step with nothing writing a stub
        // is a legitimately optional step, not a vacuous gate.
        let v = classify_steps(&[
            StepOutcome::new("Checkout", StepConclusion::Success),
            StepOutcome::new("Run Hypatia scan", StepConclusion::Skipped),
        ]);
        assert_eq!(v, PolarityVerdict::Genuine, "got {v:?}");
    }

    #[test]
    fn stub_step_without_a_skipped_scan_is_genuine() {
        // The scan really ran; a stub step succeeding alongside it proves
        // nothing.
        let v = classify_steps(&[
            StepOutcome::new("Run Hypatia scan", StepConclusion::Success),
            StepOutcome::new("Create stub findings", StepConclusion::Success),
        ]);
        assert_eq!(v, PolarityVerdict::Genuine, "got {v:?}");
    }

    #[test]
    fn an_empty_signature_never_matches() {
        // An empty list is vacuously "all matched", which would turn every
        // green into a finding. `is_usable` is what stops that.
        let v = classify(
            &[StepOutcome::new("Build", StepConclusion::Success)],
            &VacuitySignature::default(),
            &Applicability::default(),
            &RepoDeclaration::default(),
            ev(true, true, 0.0),
        );
        assert_eq!(v, PolarityVerdict::Genuine, "got {v:?}");
    }

    #[test]
    fn a_half_populated_signature_is_not_usable() {
        assert!(!VacuitySignature::new(&["a"], &[]).is_usable());
        assert!(!VacuitySignature::new(&[], &["b"]).is_usable());
        assert!(VacuitySignature::new(&["a"], &["b"]).is_usable());
    }

    // ---- the other two step-observable causes -------------------------------

    #[test]
    fn no_steps_recorded_is_vacuous() {
        let v = classify_steps(&[]);
        assert!(
            matches!(
                v,
                PolarityVerdict::Vacuous {
                    cause: VacuityCause::NoStepsRecorded,
                    ..
                }
            ),
            "got {v:?}"
        );
    }

    #[test]
    fn every_step_skipped_is_vacuous() {
        let v = classify_steps(&[
            StepOutcome::new("Checkout", StepConclusion::Skipped),
            StepOutcome::new("Build", StepConclusion::Skipped),
        ]);
        assert!(
            matches!(
                v,
                PolarityVerdict::Vacuous {
                    cause: VacuityCause::AllStepsSkipped,
                    ..
                }
            ),
            "got {v:?}"
        );
    }

    #[test]
    fn a_check_whose_steps_ran_is_genuine() {
        let v = classify_steps(&[
            StepOutcome::new("Checkout", StepConclusion::Success),
            StepOutcome::new("Build", StepConclusion::Success),
        ]);
        assert_eq!(v, PolarityVerdict::Genuine, "got {v:?}");
    }

    #[test]
    fn an_unmodelled_conclusion_is_not_a_skip() {
        // A skip is half the signature, so anything we do not model must never
        // be mistaken for one.
        assert_eq!(StepConclusion::parse(None), StepConclusion::Other);
        assert_eq!(StepConclusion::parse(Some("neutral")), StepConclusion::Other);
        assert_eq!(
            StepConclusion::parse(Some("skipped")),
            StepConclusion::Skipped
        );
    }

    // ---- Axis 0 is THREE-WAY ------------------------------------------------

    #[test]
    fn declared_and_unmatched_is_not_applicable() {
        let v = classify(
            &[],  // would otherwise be NoStepsRecorded — applicability wins
            &sig(),
            &Applicability {
                runs_for_operator_types: vec!["platform_maintainer".into()],
                runs_on_channels: vec![],
            },
            &RepoDeclaration {
                operator_type: Some("user".into()),
                channel: None,
            },
            ev(true, true, 1.0),
        );
        assert!(
            matches!(v, PolarityVerdict::NotApplicable { .. }),
            "inapplicable is not broken; got {v:?}"
        );
    }

    #[test]
    fn an_unmatched_channel_is_also_not_applicable() {
        let v = classify(
            &[],
            &sig(),
            &Applicability {
                runs_for_operator_types: vec![],
                runs_on_channels: vec!["nightly".into(), "alpha".into()],
            },
            &RepoDeclaration {
                operator_type: None,
                channel: Some("release".into()),
            },
            ev(true, true, 1.0),
        );
        assert!(
            matches!(v, PolarityVerdict::NotApplicable { .. }),
            "got {v:?}"
        );
    }

    #[test]
    fn undeclared_applicability_falls_through_to_the_signature() {
        // THE critical case: today no gate carries an applicability predicate.
        // Treating undeclared as NotApplicable would make every green
        // not-applicable — this classifier would become its own fake green.
        let v = classify(
            &[
                StepOutcome::new("Run Hypatia scan", StepConclusion::Skipped),
                StepOutcome::new("Create stub findings", StepConclusion::Success),
            ],
            &sig(),
            &Applicability::default(),
            &RepoDeclaration {
                operator_type: Some("user".into()),
                channel: Some("release".into()),
            },
            ev(true, true, 1.0),
        );
        assert!(
            matches!(v, PolarityVerdict::Vacuous { .. }),
            "undeclared must not short-circuit; got {v:?}"
        );
    }

    #[test]
    fn declared_and_matched_falls_through_to_the_signature() {
        let v = classify(
            &[
                StepOutcome::new("Run Hypatia scan", StepConclusion::Skipped),
                StepOutcome::new("Create stub findings", StepConclusion::Success),
            ],
            &sig(),
            &Applicability {
                runs_for_operator_types: vec!["developer".into()],
                runs_on_channels: vec!["alpha".into()],
            },
            &RepoDeclaration {
                operator_type: Some("developer".into()),
                channel: Some("alpha".into()),
            },
            ev(true, true, 1.0),
        );
        assert!(matches!(v, PolarityVerdict::Vacuous { .. }), "got {v:?}");
    }

    #[test]
    fn a_repo_silent_on_the_key_cannot_contradict_a_declaration() {
        // Silence is not a mismatch. Falling through is fail-open into a path
        // that can only escalate, never green.
        let v = classify(
            &[StepOutcome::new("Build", StepConclusion::Success)],
            &sig(),
            &Applicability {
                runs_on_channels: vec!["nightly".into()],
                runs_for_operator_types: vec![],
            },
            &RepoDeclaration::default(),
            ev(true, true, 0.0),
        );
        assert_eq!(v, PolarityVerdict::Genuine, "got {v:?}");
    }

    // ---- recommendation selection ------------------------------------------

    #[test]
    fn absent_technology_recommends_removal() {
        assert_eq!(
            Recommendation::from_evidence(&ev(false, true, 1.0)),
            Recommendation::RecommendRemoval
        );
    }

    #[test]
    fn a_tool_that_does_not_exist_upstream_is_a_declaration_bug() {
        assert_eq!(
            Recommendation::from_evidence(&ev(true, false, 1.0)),
            Recommendation::FixTheDeclaration
        );
    }

    #[test]
    fn a_full_stub_rate_never_recommends_deletion() {
        // The case-2 guard. With run_count == 1 the rate is exactly 0.0 or 1.0,
        // so a single stubbed run would otherwise read as "useless everywhere"
        // — a judgement the directive reserves to the owner.
        for rate in [0.0, 0.25, 0.5, 1.0] {
            assert_eq!(
                Recommendation::from_evidence(&ev(true, true, rate)),
                Recommendation::MakeItGreatInPractice,
                "stub_rate {rate} must not change the recommendation"
            );
        }
    }

    #[test]
    fn an_unmeasured_field_never_recommends_removal() {
        // The host cannot measure target-tech-present from the jobs API today,
        // and "unmeasured" must fall to the non-destructive recommendation
        // rather than being rendered as a confident `true`/`false`.
        let unmeasured = Evidence {
            run_count: 1,
            stub_rate: 1.0,
            upstream_exists: None,
            target_tech_present: None,
        };
        assert_eq!(
            Recommendation::from_evidence(&unmeasured),
            Recommendation::MakeItGreatInPractice
        );
        assert!(
            unmeasured.describe().contains("target-tech-present=unmeasured"),
            "got `{}`",
            unmeasured.describe()
        );
    }

    #[test]
    fn the_evidence_string_carries_all_four_required_fields() {
        let d = ev(true, true, 0.75).describe();
        for field in [
            "run-count",
            "stub-rate",
            "upstream-exists",
            "target-tech-present",
        ] {
            assert!(d.contains(field), "`{field}` missing from `{d}`");
        }
    }

    // ---- the consumer (an enum nothing reads is itself a fake gate) ---------

    #[test]
    fn a_vacuous_verdict_projects_to_a_gate_triage_escalation() {
        let v = classify_steps(&[
            StepOutcome::new("Run Hypatia scan", StepConclusion::Skipped),
            StepOutcome::new("Create stub findings", StepConclusion::Success),
        ]);
        let m = v.to_move("scan / hypatia").expect("vacuous must project");
        match m {
            Move::EscalateToExpert {
                check,
                group,
                obligation,
                evidence,
            } => {
                assert_eq!(check, "scan / hypatia");
                assert_eq!(group, ExpertGroup::GateTriage);
                assert_eq!(obligation, EscalationKind::AssessConfidence);
                assert!(evidence.contains("run-count"), "got `{evidence}`");
                assert!(evidence.contains("stub-rate"), "got `{evidence}`");
            }
            other => panic!("must never self-win; got {other:?}"),
        }
    }

    #[test]
    fn genuine_and_not_applicable_project_to_nothing() {
        assert!(PolarityVerdict::Genuine.to_move("c").is_none());
        assert!(
            PolarityVerdict::NotApplicable {
                declaration: "d".into()
            }
            .to_move("c")
            .is_none()
        );
    }

    // ---- the Rust witness of the SPARK theorem ------------------------------

    #[test]
    fn classification_does_not_move_the_gate() {
        // gate_machine.ads proves `Green IFF non-empty AND all Passed`.
        // Nothing in this module touches a CheckRun, so a vacuous verdict on a
        // green gate must leave `evaluate()` exactly where it was.
        let gate = Gate::new(vec![
            RequiredCheck::new("scan / hypatia", CheckRun::Passed),
            RequiredCheck::new("build", CheckRun::Passed),
        ]);
        let before = gate.evaluate();
        assert_eq!(before, GateState::Green);

        let v = classify_steps(&[
            StepOutcome::new("Run Hypatia scan", StepConclusion::Skipped),
            StepOutcome::new("Create stub findings", StepConclusion::Success),
        ]);
        assert!(matches!(v, PolarityVerdict::Vacuous { .. }));
        let _ = v.to_move("scan / hypatia");

        assert_eq!(
            gate.evaluate(),
            before,
            "polarity classification must never move the proved gate state"
        );
    }
}
