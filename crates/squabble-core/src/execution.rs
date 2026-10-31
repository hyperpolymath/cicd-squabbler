// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! The execution-phase classifier — *where* a run stopped, not merely that it
//! stopped.
//!
//! `fight` learns one bit about a red check: that it is red. It then attributes
//! the red from the *shape of the workflow* (`squabble-fight::workflows`): an
//! in-repo hygiene workflow is a self-win, a security workflow is a security
//! escalation, a reusable is an upstream owner. That attribution is only sound
//! when the policy actually ran. A job that died in runner setup, or that never
//! got its toolchain, is not a finding about the code — and handing it to a
//! fixer invents work nobody can do.
//!
//! The estate's own investigation separates five outcomes that share the single
//! word "red":
//!
//! 1. **the workflow never started** — admission or source resolution refused
//!    it, zero jobs ran. Owned by [`crate::startup`] and `squabble zero-job`;
//!    this module does not re-derive it, it only *names* it
//!    ([`ExecutionClass::NotStarted`]) so a report cannot silently drop it.
//! 2. **preflight** — the job started and died in runner setup, provisioning or
//!    lock resolution, before any policy step executed.
//! 3. **prerequisite missing** — a declared step that establishes a tool,
//!    credential or piece of configuration failed, so the policy step could not
//!    run.
//! 4. **the policy executed and failed** — findings, or a policy script that
//!    returned non-zero.
//! 5. **the policy executed and succeeded** — the only evidence-bearing success.
//!
//! Classes 2–5 are witnessable from the one source `fight` already reads for
//! greens: the Actions jobs API step conclusions
//! (`.steps[].{name,conclusion}`, the same source
//! [`crate::polarity`] uses).
//!
//! # Evidence tier — stated, not implied
//!
//! Step *names and conclusions* are the whole input. So:
//!
//! * [`ExecutionClass::PreflightFailed`] is only reachable for a step the
//!   **host declares** to be preflight (or GitHub's implicit `Set up job`). An
//!   undeclared setup step is not guessed at.
//! * [`ExecutionClass::PolicyStepFailed`] says *where* the run stopped, never
//!   *why*. A dependency-resolution error inside a step like `Cargo check
//!   --locked` is indistinguishable here from a genuine compile failure,
//!   because both are "a policy step concluded failure". This module therefore
//!   does **not** assert class 4; it reports that the policy was reached, and
//!   the finding itself must be read from the policy's own report. The
//!   estate's lockfile class (`B-LOCKFILE`) is detected by reading **job
//!   annotations**, which is a stronger source this module deliberately does
//!   not have.
//!
//! # Fail-closed
//!
//! No step evidence, an unconcluded job, a failure outside any step, or a green
//! with no successful step all yield [`ExecutionClass::Unclassified`] with a
//! reason. An unreadable fact never becomes a class.
//!
//! # Nothing here is a green
//!
//! [`ExecutionClass::Verified`] is a statement that a step ran and succeeded —
//! the same fact [`crate::polarity`] interrogates for vacuity, and the two must
//! be read together. This module constructs no [`CheckRun`], calls no
//! [`crate::gate::Gate::evaluate`], drops no required context and proposes no
//! [`crate::moves::Move`], so `squabble ≠ bypass` holds bit-for-bit: a
//! classification is a diagnosis, and no diagnosis moves a gate.

use crate::gate::CheckRun;
use crate::polarity::{StepConclusion, StepOutcome};
use serde::{Deserialize, Serialize};

/// The implicit step GitHub inserts at the head of every job. Its failure is
/// the runner-side class (image pull, runner allocation, billing refusal) and
/// it is universal, so it is compiled in rather than declared per repo. This is
/// a GitHub primitive, not a scanner name: the standalone rule
/// (`hypatia-dependent` is an IS-NOT) is not bent by it.
pub const SETUP_JOB_STEP: &str = "Set up job";

/// GitHub's trailing cleanup steps. A failure here happens *after* the policy
/// and is not a policy finding, so it must not be read as one.
pub const POST_JOB_STEPS: &[&str] = &["Post job", "Complete job"];

/// Host-supplied step-name signatures, read from the `execution_taxonomy`
/// directive. **No step name of any estate tool is compiled in** — the same
/// discipline as [`crate::polarity::VacuitySignature`], for the same reason: a
/// classifier that carries other people's vocabulary is a taxonomy, not a
/// classifier, and it drifts silently when a workflow is renamed.
///
/// Matching is an EXACT compare after trimming, deliberately. This repository
/// has already paid once for abbreviation drift (see `gate_triage.a2ml`: an
/// abbreviated step name matched no real job on earth, and the vacuity
/// classifier silently became its own fake green).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseSignatures {
    /// Steps that run **before** the policy: runner/service setup, checkout,
    /// toolchain installation, dependency and lock resolution, cache restore.
    #[serde(default)]
    pub setup_steps: Vec<String>,
    /// Steps that establish a *declared* prerequisite — a tool, credential or
    /// configuration fact — whose failure means the policy step cannot run.
    #[serde(default)]
    pub prerequisite_steps: Vec<String>,
}

impl PhaseSignatures {
    /// A signature set is usable only when it declares something. An empty set
    /// matches nothing, so a missing directive means "attribute no preflight
    /// class", never "everything is preflight".
    pub fn is_usable(&self) -> bool {
        !self.setup_steps.is_empty() || !self.prerequisite_steps.is_empty()
    }

    /// Is this step a declared setup step (or GitHub's implicit one)?
    pub fn is_setup(&self, name: &str) -> bool {
        let n = name.trim();
        n == SETUP_JOB_STEP || self.setup_steps.iter().any(|s| s.trim() == n)
    }

    /// Is this step a declared prerequisite step?
    pub fn is_prerequisite(&self, name: &str) -> bool {
        let n = name.trim();
        self.prerequisite_steps.iter().any(|s| s.trim() == n)
    }

    /// GitHub's own trailing cleanup steps — never a policy step.
    pub fn is_post(&self, name: &str) -> bool {
        let n = name.trim();
        POST_JOB_STEPS.iter().any(|s| *s == n)
    }

    /// A step that is not preflight, not a prerequisite and not cleanup: the
    /// only kind whose conclusion is evidence about the policy itself.
    pub fn is_policy_step(&self, name: &str) -> bool {
        !self.is_setup(name) && !self.is_prerequisite(name) && !self.is_post(name)
    }
}

/// Why a run could not be placed in one of the five classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnclassifiedReason {
    /// The job exists but records no steps at all — absence of evidence, not
    /// evidence of anything (the same stance [`crate::polarity`] takes for a
    /// green with no steps).
    NoStepEvidence,
    /// The job has not concluded (queued, in progress).
    NotConcluded,
    /// The conclusion says the job failed, but no step recorded a failure:
    /// cancelled, timed out, or a runner-side failure between steps.
    NoFailingStep,
    /// The only failed step is GitHub's trailing cleanup, which runs after the
    /// policy — it says nothing about the policy.
    FailureOutsidePolicyStep,
    /// The job concluded successfully but no policy step recorded success, so
    /// there is no evidence the policy executed. (`polarity` calls the
    /// all-steps-skipped form vacuous; this module refuses to call it verified.)
    NoSuccessfulStep,
}

impl UnclassifiedReason {
    pub const fn label(self) -> &'static str {
        match self {
            Self::NoStepEvidence => "no step evidence",
            Self::NotConcluded => "not concluded",
            Self::NoFailingStep => "no failing step",
            Self::FailureOutsidePolicyStep => "failure outside a policy step",
            Self::NoSuccessfulStep => "no successful policy step",
        }
    }
}

/// Who must act on a class, within the squabbler's lane rules. These name a
/// **decision owner**, never a fixer the squabbler may dispatch on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Handoff {
    /// Nothing ran: hand to admission/source-resolution attribution
    /// (`crate::startup`, `squabble zero-job`, `squabble chains`) and to the
    /// owner of the refusing setting.
    Admission,
    /// Runner, provisioning or lock resolution. The owner of the *workflow's
    /// provisioning*, not a code fixer: nothing has been said about the code.
    Provisioning,
    /// A declared tool/credential/configuration prerequisite. The owner of the
    /// repository's settings or secrets.
    Prerequisites,
    /// The policy was reached. Read its own report (annotation, SARIF, log) —
    /// the finding is evidence to be collected, not an inference to be made.
    PolicyReport,
    /// No claim. The report must say so rather than pick a lane.
    NoClaim,
}

impl Handoff {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Admission => "admission",
            Self::Provisioning => "provisioning",
            Self::Prerequisites => "prerequisites",
            Self::PolicyReport => "policy-report",
            Self::NoClaim => "no-claim",
        }
    }

    /// Who owns the next move, in plain words. Deliberately a human/owner
    /// phrase: the squabbler proposes, an owner acts.
    pub const fn owner(self) -> &'static str {
        match self {
            Self::Admission =>
                "owner of the refusing setting (Actions policy / allow-list / ruleset), with `squabble zero-job` for the attribution",
            Self::Provisioning =>
                "owner of the workflow's provisioning (runner label, toolchain, dependency and lock resolution) — no claim is made about the code",
            Self::Prerequisites =>
                "owner of the repository's tools/credentials/configuration",
            Self::PolicyReport =>
                "the producing policy's owner; collect its report as evidence",
            Self::NoClaim =>
                "nobody yet: the evidence does not support a class, and the run still must be explained",
        }
    }
}

/// Where a job stopped, as one of the estate's distinct outcomes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "kebab-case")]
pub enum ExecutionClass {
    /// Class 1 — no job exists at all (zero-job `startup_failure`, admission or
    /// source-resolution refusal). Attribution lives in [`crate::startup`].
    NotStarted,
    /// Class 2 — a declared preflight step failed. The policy never executed.
    PreflightFailed { step: String },
    /// Class 3 — a declared prerequisite step failed, so the policy step could
    /// not run. Distinct from class 2: the runner was fine; a tool, credential
    /// or configuration fact was not there.
    PrerequisiteMissing { step: String },
    /// Class 4-shaped — a policy step concluded failure. The policy *was*
    /// reached; whether that is a genuine finding must be read from its own
    /// report (see the module's evidence-tier note).
    PolicyStepFailed { step: String },
    /// Class 5 — the job passed and a policy step recorded success.
    Verified { step: String },
    /// No claim, with a reason.
    Unclassified { reason: UnclassifiedReason },
}

impl ExecutionClass {
    /// A stable kebab-case label for reports and JSON.
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotStarted => "not-started",
            Self::PreflightFailed { .. } => "preflight-failed",
            Self::PrerequisiteMissing { .. } => "prerequisite-missing",
            Self::PolicyStepFailed { .. } => "policy-step-failed",
            Self::Verified { .. } => "verified",
            Self::Unclassified { .. } => "unclassified",
        }
    }

    /// The step this class is about, when it is about one.
    pub fn step(&self) -> Option<&str> {
        match self {
            Self::NotStarted | Self::Unclassified { .. } => None,
            Self::PreflightFailed { step }
            | Self::PrerequisiteMissing { step }
            | Self::PolicyStepFailed { step }
            | Self::Verified { step } => Some(step),
        }
    }

    /// Where to hand this, and to whom.
    pub fn handoff(&self) -> Handoff {
        match self {
            Self::NotStarted => Handoff::Admission,
            Self::PreflightFailed { .. } => Handoff::Provisioning,
            Self::PrerequisiteMissing { .. } => Handoff::Prerequisites,
            Self::PolicyStepFailed { .. } => Handoff::PolicyReport,
            Self::Verified { .. } => Handoff::NoClaim,
            Self::Unclassified { .. } => Handoff::NoClaim,
        }
    }

    /// One line a human can act on. Concrete on purpose: a diagnosis nobody can
    /// act on is only marginally better than none.
    pub fn explain(&self) -> String {
        match self {
            Self::NotStarted => "no job exists, so no check report could be created — attribute it with `squabble zero-job` / `squabble chains`, never infer a cause from the conclusion alone".to_string(),
            Self::PreflightFailed { step } =>
                format!("`{step}` failed before any policy step ran: this is provisioning, not a policy finding"),
            Self::PrerequisiteMissing { step } =>
                format!("`{step}` could not establish a declared tool/credential/configuration prerequisite, so the policy step never ran"),
            Self::PolicyStepFailed { step } =>
                format!("`{step}` ran and concluded failure: the policy was reached — read its own report for the finding"),
            Self::Verified { step } =>
                format!("`{step}` ran and succeeded: an executed success, subject to the green-polarity check for vacuity"),
            Self::Unclassified { reason } =>
                format!("no class claimed ({}) — the run still must be explained", reason.label()),
        }
    }

    /// True only for a class that carries positive evidence of execution.
    /// Deliberately **not** a gate verdict: `polarity` must still be consulted
    /// before this may be read as a green, and no caller may treat it as one.
    pub fn is_evidence_bearing_success(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }
}

/// Classify one job's step evidence.
///
/// * `job` — the job's realised conclusion ([`CheckRun`]) as GitHub reports it,
///   or `None`/`Some(CheckRun::Missing)` when no job exists at all.
/// * `steps` — the jobs-API step list, in API order.
/// * `signatures` — the host's declared setup/prerequisite step names.
///
/// Order matters and is checked from the top of the job down: a preflight
/// failure cannot be masked by anything later, because nothing later ran.
pub fn classify(job: Option<CheckRun>, steps: &[StepOutcome], signatures: &PhaseSignatures) -> ExecutionClass {
    // Class 1 — nothing started. Attribution is `startup`'s job, not ours.
    let Some(job) = job.filter(|c| !matches!(c, CheckRun::Missing)) else {
        return ExecutionClass::NotStarted;
    };

    if matches!(job, CheckRun::Pending) {
        return ExecutionClass::Unclassified {
            reason: UnclassifiedReason::NotConcluded,
        };
    }

    if steps.is_empty() {
        return ExecutionClass::Unclassified {
            reason: UnclassifiedReason::NoStepEvidence,
        };
    }

    // Classes 2 and 3 — the earliest failing preflight/prerequisite step wins.
    // Setup is checked first because it runs first (and so that a name declared
    // in both lists cannot be read as a prerequisite).
    if let Some(step) = steps
        .iter()
        .find(|s| s.conclusion == StepConclusion::Failure && signatures.is_setup(&s.name))
    {
        return ExecutionClass::PreflightFailed {
            step: step.name.clone(),
        };
    }
    if let Some(step) = steps
        .iter()
        .find(|s| s.conclusion == StepConclusion::Failure && signatures.is_prerequisite(&s.name))
    {
        return ExecutionClass::PrerequisiteMissing {
            step: step.name.clone(),
        };
    }

    // The policy was reached. A failing policy step is reported as *where*, not
    // *why* — see the module's evidence-tier note.
    if let Some(step) = steps
        .iter()
        .find(|s| s.conclusion == StepConclusion::Failure && signatures.is_policy_step(&s.name))
    {
        return ExecutionClass::PolicyStepFailed {
            step: step.name.clone(),
        };
    }

    // A failure recorded only by a trailing cleanup step says nothing about the
    // policy.
    if let Some(step) = steps
        .iter()
        .find(|s| s.conclusion == StepConclusion::Failure)
    {
        return ExecutionClass::Unclassified {
            reason: if signatures.is_post(&step.name) {
                UnclassifiedReason::FailureOutsidePolicyStep
            } else {
                UnclassifiedReason::NoFailingStep
            },
        };
    }

    // Class 5 — positive evidence only: a *policy* step recorded success and the
    // job passed. A green whose policy step was skipped is not verified here;
    // `polarity` owns the vacuity question.
    if matches!(job, CheckRun::Passed) {
        if let Some(step) = steps
            .iter()
            .rev()
            .find(|s| s.conclusion == StepConclusion::Success && signatures.is_policy_step(&s.name))
        {
            return ExecutionClass::Verified {
                step: step.name.clone(),
            };
        }
        return ExecutionClass::Unclassified {
            reason: UnclassifiedReason::NoSuccessfulStep,
        };
    }

    // Job failed/skipped with no failed step recorded (cancelled, timed out).
    ExecutionClass::Unclassified {
        reason: UnclassifiedReason::NoFailingStep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig() -> PhaseSignatures {
        PhaseSignatures {
            setup_steps: vec![
                "Checkout repository".into(),
                "Install Rust toolchain".into(),
                "Cache cargo registry and build".into(),
            ],
            prerequisite_steps: vec!["Probe whether fight can inspect the full gate".into()],
        }
    }

    fn step(name: &str, c: StepConclusion) -> StepOutcome {
        StepOutcome::new(name, c)
    }

    fn setup_failed_step() -> StepOutcome {
        step(SETUP_JOB_STEP, StepConclusion::Failure)
    }

    #[test]
    fn no_job_is_not_started_and_never_a_class_about_steps() {
        // Even with step text in hand, "no job" wins: the estate's first class is
        // decided before any step could exist.
        let steps = vec![step("Cargo check", StepConclusion::Failure)];
        assert_eq!(
            classify(None, &steps, &sig()),
            ExecutionClass::NotStarted
        );
        assert_eq!(
            classify(Some(CheckRun::Missing), &steps, &sig()),
            ExecutionClass::NotStarted
        );
        assert_eq!(ExecutionClass::NotStarted.handoff(), Handoff::Admission);
    }

    #[test]
    fn a_runner_side_setup_failure_is_preflight_not_a_finding() {
        // The regression this module exists for: `Set up job` failing used to be
        // an indistinguishable `CheckRun::Failed`, which `fight` attributed to
        // the workflow's *kind* (a code fixer, or a security scan).
        let steps = vec![
            setup_failed_step(),
            step("Cargo check", StepConclusion::Skipped),
        ];
        let c = classify(Some(CheckRun::Failed), &steps, &sig());
        assert_eq!(
            c,
            ExecutionClass::PreflightFailed {
                step: SETUP_JOB_STEP.into()
            }
        );
        assert_eq!(c.handoff(), Handoff::Provisioning);
        assert!(!c.explain().contains("finding"));
    }

    #[test]
    fn a_declared_provisioning_step_is_preflight() {
        let steps = vec![
            step(SETUP_JOB_STEP, StepConclusion::Success),
            step("Install Rust toolchain", StepConclusion::Failure),
            step("Cargo check", StepConclusion::Skipped),
        ];
        let c = classify(Some(CheckRun::Failed), &steps, &sig());
        assert_eq!(
            c,
            ExecutionClass::PreflightFailed {
                step: "Install Rust toolchain".into()
            }
        );
    }

    #[test]
    fn an_undeclared_setup_looking_step_is_not_guessed_at() {
        // No signature declares it, so it is read as a policy step. The module
        // reports where, not why — and the directive is what moves the line.
        let steps = vec![
            step(SETUP_JOB_STEP, StepConclusion::Success),
            step("Some Private Setup Thing", StepConclusion::Failure),
        ];
        assert_eq!(
            classify(Some(CheckRun::Failed), &steps, &sig()),
            ExecutionClass::PolicyStepFailed {
                step: "Some Private Setup Thing".into()
            }
        );
    }

    #[test]
    fn a_failed_prerequisite_is_its_own_class_not_preflight() {
        let steps = vec![
            step(SETUP_JOB_STEP, StepConclusion::Success),
            step("Checkout repository", StepConclusion::Success),
            step(
                "Probe whether fight can inspect the full gate",
                StepConclusion::Failure,
            ),
        ];
        let c = classify(Some(CheckRun::Failed), &steps, &sig());
        assert_eq!(
            c,
            ExecutionClass::PrerequisiteMissing {
                step: "Probe whether fight can inspect the full gate".into()
            }
        );
        assert_eq!(c.handoff(), Handoff::Prerequisites);
        assert_ne!(c.handoff(), Handoff::Provisioning);
    }

    #[test]
    fn setup_outranks_prerequisite_when_both_would_match() {
        let both = PhaseSignatures {
            setup_steps: vec!["Install Rust toolchain".into()],
            prerequisite_steps: vec!["Install Rust toolchain".into()],
        };
        let steps = vec![step("Install Rust toolchain", StepConclusion::Failure)];
        assert_eq!(
            classify(Some(CheckRun::Failed), &steps, &both),
            ExecutionClass::PreflightFailed {
                step: "Install Rust toolchain".into()
            }
        );
    }

    #[test]
    fn a_policy_step_failure_names_the_step_and_hands_to_the_policy_owner() {
        // The estate's rust reusable records `Cargo check`, `Cargo fmt`,
        // `Cargo clippy` after its toolchain install (measured 2026-10-31).
        // A `--locked` dependency error and a compile error are BOTH this shape,
        // so the class must not claim to know which.
        let steps = vec![
            step(SETUP_JOB_STEP, StepConclusion::Success),
            step("Checkout repository", StepConclusion::Success),
            step("Install Rust toolchain", StepConclusion::Success),
            step("Cache cargo registry and build", StepConclusion::Success),
            step("Cargo check", StepConclusion::Failure),
            step("Cargo fmt", StepConclusion::Skipped),
        ];
        let c = classify(Some(CheckRun::Failed), &steps, &sig());
        assert_eq!(
            c,
            ExecutionClass::PolicyStepFailed {
                step: "Cargo check".into()
            }
        );
        assert_eq!(c.handoff(), Handoff::PolicyReport);
        assert!(c.explain().contains("read its own report"));
        assert!(
            !c.is_evidence_bearing_success(),
            "reaching the policy is not a success"
        );
    }

    #[test]
    fn a_verified_run_names_the_step_that_ran() {
        let steps = vec![
            step(SETUP_JOB_STEP, StepConclusion::Success),
            step("Checkout repository", StepConclusion::Success),
            step("Cargo check", StepConclusion::Success),
            step("Post job", StepConclusion::Success),
        ];
        assert_eq!(
            classify(Some(CheckRun::Passed), &steps, &sig()),
            ExecutionClass::Verified {
                step: "Cargo check".into()
            }
        );
    }

    #[test]
    fn a_green_with_no_successful_policy_step_is_not_verified() {
        // The all-skipped shape: `polarity` calls it vacuous, this module
        // refuses to call it executed. Neither may be read as a green.
        let steps = vec![
            step(SETUP_JOB_STEP, StepConclusion::Success),
            step("Run Hypatia scan", StepConclusion::Skipped),
        ];
        let c = classify(Some(CheckRun::Passed), &steps, &sig());
        assert_eq!(
            c,
            ExecutionClass::Unclassified {
                reason: UnclassifiedReason::NoSuccessfulStep
            }
        );
        assert!(!c.is_evidence_bearing_success());
    }

    #[test]
    fn a_green_from_a_preflight_step_alone_is_not_verified() {
        // Only setup ran and succeeded; nothing about the policy is known.
        let steps = vec![
            step(SETUP_JOB_STEP, StepConclusion::Success),
            step("Checkout repository", StepConclusion::Success),
        ];
        assert_eq!(
            classify(Some(CheckRun::Passed), &steps, &sig()),
            ExecutionClass::Unclassified {
                reason: UnclassifiedReason::NoSuccessfulStep
            }
        );
    }

    #[test]
    fn a_cleanup_failure_is_not_a_policy_finding() {
        let steps = vec![
            step(SETUP_JOB_STEP, StepConclusion::Success),
            step("Cargo check", StepConclusion::Success),
            step("Post job", StepConclusion::Failure),
        ];
        let c = classify(Some(CheckRun::Failed), &steps, &sig());
        assert_eq!(
            c,
            ExecutionClass::Unclassified {
                reason: UnclassifiedReason::FailureOutsidePolicyStep
            }
        );
        assert_eq!(c.handoff(), Handoff::NoClaim);
    }

    #[test]
    fn an_empty_step_list_is_the_honest_non_answer() {
        let c = classify(Some(CheckRun::Failed), &[], &sig());
        assert_eq!(
            c,
            ExecutionClass::Unclassified {
                reason: UnclassifiedReason::NoStepEvidence
            }
        );
        // Symbolically identical to a skipped job's evidence: absence, not
        // evidence of absence.
        assert_eq!(
            classify(Some(CheckRun::Skipped), &[], &sig()),
            ExecutionClass::Unclassified {
                reason: UnclassifiedReason::NoStepEvidence
            }
        );
    }

    #[test]
    fn an_unconcluded_job_is_not_classified() {
        assert_eq!(
            classify(Some(CheckRun::Pending), &[], &sig()),
            ExecutionClass::Unclassified {
                reason: UnclassifiedReason::NotConcluded
            }
        );
    }

    #[test]
    fn a_failure_with_no_failing_step_is_not_attributed() {
        // Cancelled/timeout: the conclusion says failure, no step recorded one.
        let steps = vec![
            step(SETUP_JOB_STEP, StepConclusion::Success),
            step("Cargo check", StepConclusion::Cancelled),
        ];
        assert_eq!(
            classify(Some(CheckRun::Failed), &steps, &sig()),
            ExecutionClass::Unclassified {
                reason: UnclassifiedReason::NoFailingStep
            }
        );
    }

    #[test]
    fn matching_is_exact_so_an_abbreviated_name_cannot_match() {
        // The `gate_triage.a2ml` failure mode, re-asserted here: an abbreviated
        // signature silently matches nothing. It must therefore leave the step
        // in the policy lane, not mis-file it as preflight.
        let abbreviated = PhaseSignatures {
            setup_steps: vec!["Install Rust".into()],
            prerequisite_steps: vec![],
        };
        let steps = vec![step("Install Rust toolchain", StepConclusion::Failure)];
        assert_eq!(
            classify(Some(CheckRun::Failed), &steps, &abbreviated),
            ExecutionClass::PolicyStepFailed {
                step: "Install Rust toolchain".into()
            }
        );
        assert!(!abbreviated.is_setup("Install Rust toolchain"));
        // Surrounding whitespace is not an abbreviation, and is tolerated.
        assert!(abbreviated.is_setup("  Install Rust  "));
    }

    #[test]
    fn an_empty_signature_set_attributes_no_preflight_class() {
        let empty = PhaseSignatures::default();
        assert!(!empty.is_usable());
        let steps = vec![step("Install Rust toolchain", StepConclusion::Failure)];
        // Only GitHub's implicit step is universal; everything else stays policy.
        assert!(matches!(
            classify(Some(CheckRun::Failed), &steps, &empty),
            ExecutionClass::PolicyStepFailed { .. }
        ));
        assert!(matches!(
            classify(Some(CheckRun::Failed), &[setup_failed_step()], &empty),
            ExecutionClass::PreflightFailed { .. }
        ));
    }

    #[test]
    fn classes_round_trip_as_json_and_carry_no_gate_state() {
        let cases = [
            ExecutionClass::NotStarted,
            ExecutionClass::PreflightFailed {
                step: "Set up job".into(),
            },
            ExecutionClass::PrerequisiteMissing {
                step: "Probe".into(),
            },
            ExecutionClass::PolicyStepFailed {
                step: "Cargo check".into(),
            },
            ExecutionClass::Verified {
                step: "Cargo test".into(),
            },
            ExecutionClass::Unclassified {
                reason: UnclassifiedReason::NoStepEvidence,
            },
        ];
        for c in &cases {
            let json = serde_json::to_string(c).expect("serialisable");
            let back: ExecutionClass = serde_json::from_str(&json).expect("round trips");
            assert_eq!(&back, c);
            assert!(!json.contains("green"), "{json}");
        }
        assert!(serde_json::to_string(&ExecutionClass::PreflightFailed {
            step: "Set up job".into()
        })
        .unwrap()
        .contains("\"class\":\"preflight-failed\""));
    }

    #[test]
    fn only_a_verified_class_is_evidence_bearing_and_it_still_needs_polarity() {
        let failing = classify(
            Some(CheckRun::Failed),
            &[step("Cargo check", StepConclusion::Failure)],
            &sig(),
        );
        assert!(!failing.is_evidence_bearing_success());
        let verified = classify(
            Some(CheckRun::Passed),
            &[step("Cargo check", StepConclusion::Success)],
            &sig(),
        );
        assert!(verified.is_evidence_bearing_success());
        // It is still not a gate verdict: the handoff claims nothing, so no
        // caller can read it as "this repository is green".
        assert_eq!(verified.handoff(), Handoff::NoClaim);
    }

    #[test]
    fn every_class_names_a_handoff_and_a_reason_a_human_can_act_on() {
        for c in [
            ExecutionClass::NotStarted,
            ExecutionClass::PreflightFailed {
                step: "Set up job".into(),
            },
            ExecutionClass::PrerequisiteMissing { step: "P".into() },
            ExecutionClass::PolicyStepFailed {
                step: "Cargo test".into(),
            },
            ExecutionClass::Verified {
                step: "Cargo test".into(),
            },
            ExecutionClass::Unclassified {
                reason: UnclassifiedReason::NoFailingStep,
            },
        ] {
            assert!(!c.explain().is_empty());
            assert!(!c.handoff().owner().is_empty());
            assert!(!c.label().is_empty());
        }
    }
}
