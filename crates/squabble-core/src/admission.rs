// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Pure admission policy for CI work and automation-created pull requests.
//!
//! This module deliberately owns no clock, database, GitHub client, or queue.
//! A host supplies a policy, a request, an authoritative budget snapshot, and
//! the current time. The evaluator then returns one deterministic decision.
//! Missing, incomplete, stale, malformed, or arithmetically unsafe evidence
//! can never produce [`AdmissionDecision::Admit`].
//!
//! Admission is **not** a CI result. It only reserves permission to start work;
//! the existing gate invariant still requires the real checks to run and pass.

use serde::{Deserialize, Serialize};

/// Hard ceilings and explicit exceptions for one admission domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionPolicy {
    /// Maximum age of the budget evidence accepted by the evaluator.
    pub max_snapshot_age_seconds: u64,
    pub max_inflight_estate: u32,
    pub max_inflight_repository: u32,
    pub max_open_automation_prs_estate: u32,
    pub max_open_automation_prs_repository: u32,
    pub max_reserved_minutes_per_day: u64,
    pub max_reserved_minutes_per_change: u64,
    pub max_automatic_workflows_per_change: u32,
    pub max_runner_jobs_per_change: u32,
    /// New workflow files are quarantined unless the policy explicitly opts in.
    pub allow_new_workflow_files: bool,
    /// Broader automatic triggers are quarantined unless explicitly allowed.
    pub allow_trigger_expansion: bool,
}

/// What the caller wants the admission broker to permit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationClass {
    CreatePullRequest,
    RunCi,
    Release,
    Deployment,
}

impl OperationClass {
    fn requires_explicit_approval(self) -> bool {
        matches!(self, Self::Release | Self::Deployment)
    }
}

/// The authenticated principal class reported by the host.
///
/// This is evidence for receipts and later policy refinement, not a bypass:
/// every class, including [`ProducerClass::HumanOwner`], is subject to the
/// same capacity and cost ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProducerClass {
    HumanOwner,
    Automation,
    GitHubApp,
}

/// Whether a privileged operation has a separately grounded human approval.
///
/// The host is responsible for authenticating this fact. Merely being invoked
/// by an owner-class principal does not imply approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalState {
    None,
    HumanApproved,
}

/// How confidently the host could bound the workflow graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TopologyKnowledge {
    Known,
    /// The graph depends on runtime expressions such as a generated matrix.
    Dynamic,
    /// The graph could not be parsed or fully inspected.
    Unknown,
}

/// Conservative upper-bound inputs for one change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedCost {
    pub topology: TopologyKnowledge,
    /// Independently-triggered automatic workflow registrations.
    pub automatic_workflows: u32,
    /// Runner-bearing jobs after statically expanding bounded matrices.
    pub runner_jobs: u32,
    /// Upper-bound timeout applied to each runner job.
    pub max_job_timeout_minutes: u64,
    /// Fixed runner work not represented by `runner_jobs`.
    pub fixed_runner_minutes: u64,
    pub new_workflow_files: u32,
    pub trigger_expansions: u32,
}

impl ProjectedCost {
    /// Conservative runner-minute reservation, checked for integer overflow.
    pub fn reserved_minutes(&self) -> Option<u64> {
        u64::from(self.runner_jobs)
            .checked_mul(self.max_job_timeout_minutes)?
            .checked_add(self.fixed_runner_minutes)
    }
}

/// One request evaluated against an authoritative snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionRequest {
    pub repository_id: u64,
    /// Full object ID whose exact content will be admitted.
    pub head_sha: String,
    pub operation: OperationClass,
    pub producer: ProducerClass,
    pub approval: ApprovalState,
    pub projected: ProjectedCost,
}

/// Capacity evidence supplied by the stateful broker/ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetSnapshot {
    pub captured_at_unix_seconds: u64,
    /// False when any required counter could not be obtained.
    pub data_complete: bool,
    /// The head observed by the broker when it built this snapshot.
    pub observed_head_sha: String,
    pub estate_inflight: u32,
    pub repository_inflight: u32,
    pub estate_open_automation_prs: u32,
    pub repository_open_automation_prs: u32,
    pub reserved_minutes_today: u64,
}

/// Stable machine-readable reason for a non-admission decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdmissionReason {
    MissingPolicy,
    MissingBudgetSnapshot,
    IncompleteBudgetSnapshot,
    BudgetSnapshotFromFuture,
    StaleBudgetSnapshot,
    InvalidHeadSha,
    HeadShaChanged,
    CostOverflow,
    DynamicTopology,
    UnknownTopology,
    WorkflowFilesAdded,
    TriggerExpanded,
    PrivilegedOperationNeedsApproval,
    AutomaticWorkflowCapExceeded,
    RunnerJobCapExceeded,
    ChangeBudgetExceeded,
    EstateInflightCapReached,
    RepositoryInflightCapReached,
    EstateOpenPrCapReached,
    RepositoryOpenPrCapReached,
    DailyBudgetExceeded,
}

/// The deterministic result of evaluating an admission request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "kebab-case")]
pub enum AdmissionDecision {
    /// Static evidence is sound and capacity has been reserved conceptually.
    /// A stateful host must atomically commit the reservation before dispatch.
    Admit { reserved_minutes: u64 },
    /// Sound work cannot start until transient capacity becomes available.
    Queue { reason: AdmissionReason },
    /// A human must review an explicit policy/topology risk.
    Quarantine { reason: AdmissionReason },
    /// Evidence is absent, invalid, stale, or unsafe to calculate.
    Deny { reason: AdmissionReason },
}

fn deny(reason: AdmissionReason) -> AdmissionDecision {
    AdmissionDecision::Deny { reason }
}

fn queue(reason: AdmissionReason) -> AdmissionDecision {
    AdmissionDecision::Queue { reason }
}

fn quarantine(reason: AdmissionReason) -> AdmissionDecision {
    AdmissionDecision::Quarantine { reason }
}

/// Evaluate one request without performing any side effect.
///
/// Checks are intentionally ordered from evidence integrity, through static
/// policy, to transient capacity. A malformed request is denied even when the
/// estate is also full; it must not become eligible merely because a queue
/// later drains.
pub fn evaluate_admission(
    policy: Option<&AdmissionPolicy>,
    request: &AdmissionRequest,
    snapshot: Option<&BudgetSnapshot>,
    now_unix_seconds: u64,
) -> AdmissionDecision {
    let Some(policy) = policy else {
        return deny(AdmissionReason::MissingPolicy);
    };
    let Some(snapshot) = snapshot else {
        return deny(AdmissionReason::MissingBudgetSnapshot);
    };
    if !snapshot.data_complete {
        return deny(AdmissionReason::IncompleteBudgetSnapshot);
    }
    if snapshot.captured_at_unix_seconds > now_unix_seconds {
        return deny(AdmissionReason::BudgetSnapshotFromFuture);
    }
    if now_unix_seconds - snapshot.captured_at_unix_seconds > policy.max_snapshot_age_seconds {
        return deny(AdmissionReason::StaleBudgetSnapshot);
    }
    if !valid_sha(&request.head_sha) || !valid_sha(&snapshot.observed_head_sha) {
        return deny(AdmissionReason::InvalidHeadSha);
    }
    if request.head_sha != snapshot.observed_head_sha {
        return deny(AdmissionReason::HeadShaChanged);
    }

    let Some(reserved_minutes) = request.projected.reserved_minutes() else {
        return deny(AdmissionReason::CostOverflow);
    };
    let Some(day_after_reservation) = snapshot
        .reserved_minutes_today
        .checked_add(reserved_minutes)
    else {
        return deny(AdmissionReason::CostOverflow);
    };

    match request.projected.topology {
        TopologyKnowledge::Known => {}
        TopologyKnowledge::Dynamic => {
            return quarantine(AdmissionReason::DynamicTopology);
        }
        TopologyKnowledge::Unknown => {
            return quarantine(AdmissionReason::UnknownTopology);
        }
    }
    if request.projected.new_workflow_files > 0 && !policy.allow_new_workflow_files {
        return quarantine(AdmissionReason::WorkflowFilesAdded);
    }
    if request.projected.trigger_expansions > 0 && !policy.allow_trigger_expansion {
        return quarantine(AdmissionReason::TriggerExpanded);
    }
    if request.operation.requires_explicit_approval()
        && request.approval != ApprovalState::HumanApproved
    {
        return quarantine(AdmissionReason::PrivilegedOperationNeedsApproval);
    }
    if request.projected.automatic_workflows > policy.max_automatic_workflows_per_change {
        return quarantine(AdmissionReason::AutomaticWorkflowCapExceeded);
    }
    if request.projected.runner_jobs > policy.max_runner_jobs_per_change {
        return quarantine(AdmissionReason::RunnerJobCapExceeded);
    }
    if reserved_minutes > policy.max_reserved_minutes_per_change {
        return queue(AdmissionReason::ChangeBudgetExceeded);
    }
    if snapshot.estate_inflight >= policy.max_inflight_estate {
        return queue(AdmissionReason::EstateInflightCapReached);
    }
    if snapshot.repository_inflight >= policy.max_inflight_repository {
        return queue(AdmissionReason::RepositoryInflightCapReached);
    }
    if request.operation == OperationClass::CreatePullRequest {
        if snapshot.estate_open_automation_prs >= policy.max_open_automation_prs_estate {
            return queue(AdmissionReason::EstateOpenPrCapReached);
        }
        if snapshot.repository_open_automation_prs >= policy.max_open_automation_prs_repository {
            return queue(AdmissionReason::RepositoryOpenPrCapReached);
        }
    }
    if day_after_reservation > policy.max_reserved_minutes_per_day {
        return queue(AdmissionReason::DailyBudgetExceeded);
    }

    AdmissionDecision::Admit { reserved_minutes }
}

/// GitHub currently supplies full SHA-1 object IDs for event head commits.
/// Reject the all-zero sentinel as well as abbreviated or non-hex strings.
fn valid_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) && !sha.bytes().all(|b| b == b'0')
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn policy() -> AdmissionPolicy {
        AdmissionPolicy {
            max_snapshot_age_seconds: 60,
            max_inflight_estate: 4,
            max_inflight_repository: 1,
            max_open_automation_prs_estate: 5,
            max_open_automation_prs_repository: 1,
            max_reserved_minutes_per_day: 500,
            max_reserved_minutes_per_change: 100,
            max_automatic_workflows_per_change: 1,
            max_runner_jobs_per_change: 6,
            allow_new_workflow_files: false,
            allow_trigger_expansion: false,
        }
    }

    fn request() -> AdmissionRequest {
        AdmissionRequest {
            repository_id: 42,
            head_sha: SHA.into(),
            operation: OperationClass::RunCi,
            producer: ProducerClass::Automation,
            approval: ApprovalState::None,
            projected: ProjectedCost {
                topology: TopologyKnowledge::Known,
                automatic_workflows: 1,
                runner_jobs: 2,
                max_job_timeout_minutes: 10,
                fixed_runner_minutes: 1,
                new_workflow_files: 0,
                trigger_expansions: 0,
            },
        }
    }

    fn snapshot() -> BudgetSnapshot {
        BudgetSnapshot {
            captured_at_unix_seconds: NOW - 1,
            data_complete: true,
            observed_head_sha: SHA.into(),
            estate_inflight: 0,
            repository_inflight: 0,
            estate_open_automation_prs: 0,
            repository_open_automation_prs: 0,
            reserved_minutes_today: 0,
        }
    }

    fn decision(
        policy: Option<&AdmissionPolicy>,
        request: &AdmissionRequest,
        snapshot: Option<&BudgetSnapshot>,
    ) -> AdmissionDecision {
        evaluate_admission(policy, request, snapshot, NOW)
    }

    #[test]
    fn missing_policy_denies() {
        assert_eq!(
            decision(None, &request(), Some(&snapshot())),
            deny(AdmissionReason::MissingPolicy)
        );
    }

    #[test]
    fn missing_budget_snapshot_denies() {
        assert_eq!(
            decision(Some(&policy()), &request(), None),
            deny(AdmissionReason::MissingBudgetSnapshot)
        );
    }

    #[test]
    fn incomplete_budget_snapshot_denies() {
        let mut s = snapshot();
        s.data_complete = false;
        assert_eq!(
            decision(Some(&policy()), &request(), Some(&s)),
            deny(AdmissionReason::IncompleteBudgetSnapshot)
        );
    }

    #[test]
    fn stale_and_future_snapshots_deny() {
        let mut stale = snapshot();
        stale.captured_at_unix_seconds = NOW - 61;
        assert_eq!(
            decision(Some(&policy()), &request(), Some(&stale)),
            deny(AdmissionReason::StaleBudgetSnapshot)
        );

        let mut future = snapshot();
        future.captured_at_unix_seconds = NOW + 1;
        assert_eq!(
            decision(Some(&policy()), &request(), Some(&future)),
            deny(AdmissionReason::BudgetSnapshotFromFuture)
        );
    }

    #[test]
    fn invalid_or_changed_sha_denies() {
        let mut malformed = request();
        malformed.head_sha = "abc123".into();
        assert_eq!(
            decision(Some(&policy()), &malformed, Some(&snapshot())),
            deny(AdmissionReason::InvalidHeadSha)
        );

        let mut changed = snapshot();
        changed.observed_head_sha = "fedcba9876543210fedcba9876543210fedcba98".into();
        assert_eq!(
            decision(Some(&policy()), &request(), Some(&changed)),
            deny(AdmissionReason::HeadShaChanged)
        );
    }

    #[test]
    fn projected_cost_overflow_denies() {
        let mut r = request();
        r.projected.runner_jobs = 2;
        r.projected.max_job_timeout_minutes = u64::MAX;
        assert_eq!(
            decision(Some(&policy()), &r, Some(&snapshot())),
            deny(AdmissionReason::CostOverflow)
        );

        let mut s = snapshot();
        s.reserved_minutes_today = u64::MAX;
        assert_eq!(
            decision(Some(&policy()), &request(), Some(&s)),
            deny(AdmissionReason::CostOverflow)
        );
    }

    #[test]
    fn workflow_addition_and_trigger_expansion_quarantine() {
        let mut added = request();
        added.projected.new_workflow_files = 1;
        assert_eq!(
            decision(Some(&policy()), &added, Some(&snapshot())),
            quarantine(AdmissionReason::WorkflowFilesAdded)
        );

        let mut expanded = request();
        expanded.projected.trigger_expansions = 1;
        assert_eq!(
            decision(Some(&policy()), &expanded, Some(&snapshot())),
            quarantine(AdmissionReason::TriggerExpanded)
        );
    }

    #[test]
    fn dynamic_and_unknown_topology_quarantine() {
        let mut dynamic = request();
        dynamic.projected.topology = TopologyKnowledge::Dynamic;
        assert_eq!(
            decision(Some(&policy()), &dynamic, Some(&snapshot())),
            quarantine(AdmissionReason::DynamicTopology)
        );

        let mut unknown = request();
        unknown.projected.topology = TopologyKnowledge::Unknown;
        assert_eq!(
            decision(Some(&policy()), &unknown, Some(&snapshot())),
            quarantine(AdmissionReason::UnknownTopology)
        );
    }

    #[test]
    fn privileged_operations_need_separate_human_approval() {
        for operation in [OperationClass::Release, OperationClass::Deployment] {
            let mut r = request();
            r.operation = operation;
            r.producer = ProducerClass::HumanOwner;
            assert_eq!(
                decision(Some(&policy()), &r, Some(&snapshot())),
                quarantine(AdmissionReason::PrivilegedOperationNeedsApproval),
                "owner identity alone must not approve {operation:?}"
            );

            r.approval = ApprovalState::HumanApproved;
            assert!(matches!(
                decision(Some(&policy()), &r, Some(&snapshot())),
                AdmissionDecision::Admit { .. }
            ));
        }
    }

    #[test]
    fn static_topology_caps_quarantine() {
        let mut workflows = request();
        workflows.projected.automatic_workflows = 2;
        assert_eq!(
            decision(Some(&policy()), &workflows, Some(&snapshot())),
            quarantine(AdmissionReason::AutomaticWorkflowCapExceeded)
        );

        let mut jobs = request();
        jobs.projected.runner_jobs = 7;
        assert_eq!(
            decision(Some(&policy()), &jobs, Some(&snapshot())),
            quarantine(AdmissionReason::RunnerJobCapExceeded)
        );
    }

    #[test]
    fn per_change_and_daily_budget_caps_queue() {
        let mut p = policy();
        p.max_reserved_minutes_per_change = 20;
        assert_eq!(
            decision(Some(&p), &request(), Some(&snapshot())),
            queue(AdmissionReason::ChangeBudgetExceeded)
        );

        let mut s = snapshot();
        s.reserved_minutes_today = 480;
        assert_eq!(
            decision(Some(&policy()), &request(), Some(&s)),
            queue(AdmissionReason::DailyBudgetExceeded)
        );
    }

    #[test]
    fn inflight_caps_queue_at_the_boundary() {
        let mut estate = snapshot();
        estate.estate_inflight = policy().max_inflight_estate;
        assert_eq!(
            decision(Some(&policy()), &request(), Some(&estate)),
            queue(AdmissionReason::EstateInflightCapReached)
        );

        let mut repo = snapshot();
        repo.repository_inflight = policy().max_inflight_repository;
        assert_eq!(
            decision(Some(&policy()), &request(), Some(&repo)),
            queue(AdmissionReason::RepositoryInflightCapReached)
        );
    }

    #[test]
    fn owner_identity_does_not_bypass_capacity() {
        let mut r = request();
        r.producer = ProducerClass::HumanOwner;
        let mut s = snapshot();
        s.estate_inflight = policy().max_inflight_estate;
        assert_eq!(
            decision(Some(&policy()), &r, Some(&s)),
            queue(AdmissionReason::EstateInflightCapReached)
        );
    }

    #[test]
    fn open_pr_caps_apply_to_pr_creation_only() {
        let mut r = request();
        r.operation = OperationClass::CreatePullRequest;

        let mut estate = snapshot();
        estate.estate_open_automation_prs = policy().max_open_automation_prs_estate;
        assert_eq!(
            decision(Some(&policy()), &r, Some(&estate)),
            queue(AdmissionReason::EstateOpenPrCapReached)
        );

        let mut repo = snapshot();
        repo.repository_open_automation_prs = policy().max_open_automation_prs_repository;
        assert_eq!(
            decision(Some(&policy()), &r, Some(&repo)),
            queue(AdmissionReason::RepositoryOpenPrCapReached)
        );

        let mut ci = request();
        ci.producer = ProducerClass::HumanOwner;
        assert!(matches!(
            decision(Some(&policy()), &ci, Some(&repo)),
            AdmissionDecision::Admit { .. }
        ));
    }

    #[test]
    fn exact_cost_boundary_admits_and_reports_reservation() {
        let mut p = policy();
        p.max_reserved_minutes_per_change = 21;
        p.max_reserved_minutes_per_day = 500;
        let mut s = snapshot();
        s.reserved_minutes_today = 479;
        assert_eq!(
            decision(Some(&p), &request(), Some(&s)),
            AdmissionDecision::Admit {
                reserved_minutes: 21
            }
        );
    }

    #[test]
    fn explicit_policy_can_allow_reviewed_workflow_changes() {
        let mut p = policy();
        p.allow_new_workflow_files = true;
        p.allow_trigger_expansion = true;
        let mut r = request();
        r.projected.new_workflow_files = 1;
        r.projected.trigger_expansions = 1;
        assert!(matches!(
            decision(Some(&p), &r, Some(&snapshot())),
            AdmissionDecision::Admit { .. }
        ));
    }

    #[test]
    fn decision_round_trips_as_machine_readable_evidence() {
        let d = AdmissionDecision::Quarantine {
            reason: AdmissionReason::DynamicTopology,
        };
        let json = serde_json::to_string(&d).expect("serialise");
        let back: AdmissionDecision = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(d, back);
    }
}
