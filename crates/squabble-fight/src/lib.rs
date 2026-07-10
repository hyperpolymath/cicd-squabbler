// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble-fight` — the host-side fight orchestration, shared by every host.
//!
//! `squabble-core` is the pure, estate-free engine; this crate is the *shared
//! host brain* that both `squabble-cli` and `squabble-app` call, so the CLI
//! and the HTTP backend cannot drift apart. It stays out of `squabble-core`
//! on purpose: reading a2ml descriptiles and workflow files is estate-aware
//! host work, and core's detachability is the product identity.
//!
//! For each unsatisfied required check the planner decides one of three
//! dispositions, and *nothing else* — it never bypasses:
//!
//! * **self-win** — the red is CI/gate configuration, the squabbler's own lane
//!   (reconcile a required-context name, inject a path-filter pass-through,
//!   re-pin a reusable, ground-truth workflow hygiene);
//! * **escalate** — the red is out of lane (a code/build fix, a proof, a scan)
//!   and is handed to a specialist `ExpertGroup` with evidence;
//! * **assign owner** — the red is produced upstream or is structurally
//!   misconfigured, so it is put into the debate with a named owner.
//!
//! The result is emitted as an [`Outcome`]. In v0.1 the fight *plans and
//! reports* (fail-closed, evidence-per-step); actually applying self-win moves
//! and re-running remains the open `apply` step (see `STATE.a2ml`), so a
//! still-red gate is honestly reported as [`Outcome::Red`] carrying the full
//! manifest rather than a faked green.

pub mod context;
pub mod workflows;

use context::RepoContext;
use squabble_core::gate::Gate;
use squabble_core::moves::{EscalationKind, ExpertGroup, Move, OwnershipDisposition};
use squabble_core::outcome::{Escalation, Outcome, OwnerAssignment, Report};
use squabble_core::{diagnose_with_hints, Diagnosis};
use std::collections::HashMap;
use std::path::Path;
use workflows::WorkflowFacts;

/// The pure planning core: classify every unsatisfied check and assemble the
/// [`Outcome`]. Pure over its inputs (no IO beyond the structural file probes,
/// which are read-only), so it is the unit of test coverage.
///
/// Prefer [`plan_at_root`] (or [`plan_ungrounded`]) from hosts — this is the
/// pure seam for a host that manages its own context/facts loading (e.g. to
/// cache them); it does not guarantee those inputs match `repo_root`.
pub fn plan(
    gate: &Gate,
    slug: &str,
    repo_root: &Path,
    context: &RepoContext,
    facts: &WorkflowFacts,
) -> Outcome {
    let unsatisfied: Vec<_> = gate.unsatisfied().cloned().collect();

    // 1. Per-check classification via workflow ground-truth → hints.
    let mut hints: HashMap<String, Move> = HashMap::new();
    for check in &unsatisfied {
        if let Some(m) = facts.classify(&check.required_context, slug) {
            hints.insert(check.required_context.clone(), m);
        }
    }
    let Diagnosis { state, proposed } = diagnose_with_hints(gate, &hints);

    // 2. Bucket the proposals into the report's three evidence sections.
    let mut moves_attempted = Vec::new();
    let mut escalations = Vec::new();
    let mut owner_assignments = Vec::new();
    let mut blockers = Vec::new();

    for (check, m) in unsatisfied.iter().zip(proposed.iter()) {
        let ctx = &check.required_context;
        if let Some(e) = Escalation::from_move(m) {
            blockers.push(format!("`{ctx}`: escalated to {}", e.group.label()));
            escalations.push(e);
        } else if let Some(o) = OwnerAssignment::from_move(m) {
            blockers.push(format!(
                "`{ctx}`: {} → `{}`",
                o.disposition.label(),
                o.owner
            ));
            owner_assignments.push(o);
        } else {
            blockers.push(format!("`{ctx}`: self-win — {}", m.describe()));
            moves_attempted.push(m.clone());
        }
    }

    // 3. Structural scan: surface latent issues that are not themselves a
    //    required check but belong in the debate (missing well-known files, the
    //    path-filter trap, a banned AGPL licence label).
    let (extra_escalations, extra_owner_assignments) =
        structural_scan(repo_root, slug, context, &unsatisfied, facts);
    escalations.extend(extra_escalations);
    owner_assignments.extend(extra_owner_assignments);

    // Honesty about the ground the plan stands on: if neither a2ml context nor
    // any workflow could be read, every classification above fell back to
    // name-only heuristics. Say so explicitly — a confident-looking manifest
    // computed against an absent filesystem would be silent evidence loss.
    if !context.found && facts.workflows.is_empty() {
        blockers.push(format!(
            "ground-truth absent under `{}`: no .machine_readable/ descriptiles or \
             .github/workflows found — classification is name-only conservative",
            repo_root.display()
        ));
    }

    let summary = format!(
        "gate {state:?}: {} unsatisfied — {} self-win, {} escalated, {} owner-assigned",
        unsatisfied.len(),
        moves_attempted.len(),
        escalations.len(),
        owner_assignments.len()
    );

    // The gate is genuinely not green and nothing was applied/re-run in v0.1, so
    // report it honestly as Red carrying the full plan — never a faked green.
    Outcome::Red {
        report: Report {
            summary,
            unsatisfied,
            moves_attempted,
            escalations,
            owner_assignments,
            expert_verdicts: Vec::new(),
            blockers,
        },
    }
}

/// Convenience for hosts: load the repo's ecosystem context and workflow facts
/// from `repo_root` (both loaders are fail-safe on absent/unreadable trees),
/// plan, and hand back the context alongside the outcome so the host can
/// narrate it. This is the single entry point the CLI and the App share.
pub fn plan_at_root(gate: &Gate, slug: &str, repo_root: &Path) -> (RepoContext, Outcome) {
    let context = RepoContext::load(repo_root);
    let facts = WorkflowFacts::load(repo_root);
    let outcome = plan(gate, slug, repo_root, &context, &facts);
    (context, outcome)
}

/// Plan with **no filesystem at all** — for hosts serving callers that have no
/// local checkout (e.g. the App handling a cartridge request without
/// `repo_root`). Deliberately does NOT probe the process's own working
/// directory: reading whatever tree the daemon happens to be launched from
/// would silently mis-attribute evidence to the wrong repo. The resulting
/// report carries an explicit "ground-truth absent" blocker.
pub fn plan_ungrounded(gate: &Gate, slug: &str) -> Outcome {
    plan(
        gate,
        slug,
        Path::new("(no repo_root supplied)"),
        &RepoContext::default(),
        &WorkflowFacts::default(),
    )
}

/// Read-only probes for latent structural issues worth putting into the
/// debate. A pure producer: returns what it found rather than appending to
/// caller-owned vectors.
fn structural_scan(
    repo_root: &Path,
    slug: &str,
    context: &RepoContext,
    unsatisfied: &[squabble_core::gate::RequiredCheck],
    facts: &WorkflowFacts,
) -> (Vec<Escalation>, Vec<OwnerAssignment>) {
    let mut escalations = Vec::new();
    let mut owner_assignments = Vec::new();
    let names: Vec<&str> = unsatisfied
        .iter()
        .map(|c| c.required_context.as_str())
        .collect();
    let has_wellknown_red = names
        .iter()
        .any(|n| n.to_ascii_lowercase().contains("well-known"));

    // (a) Missing RSR well-known files — the likely underlying cause of a
    //     Well-Known red. Adding these is content authoring, out of the
    //     CI-config lane, so it is escalated (fulfilled by the fleet / champion).
    if has_wellknown_red {
        let missing: Vec<&str> = ["ai.txt", "humans.txt"]
            .into_iter()
            .filter(|f| !repo_root.join(".well-known").join(f).exists())
            .collect();
        if !missing.is_empty() {
            escalations.push(Escalation {
                check: "governance / Well-Known (RFC 9116 + RSR)".to_string(),
                group: ExpertGroup::HypatiaFleet,
                obligation: EscalationKind::DispatchFix,
                evidence: format!(
                    "missing .well-known/{} — likely underlying cause; content authoring, out of lane",
                    missing.join(", .well-known/")
                ),
            });
        }
    }

    // (b) Path-filter traps — a path-filtered workflow that is (or could be) a
    //     required check strands off-path PRs as Expected → blocked. Surface
    //     each for the debate; owner is the coordinating repo if known.
    let owner = context
        .coordination_owner
        .clone()
        .unwrap_or_else(|| slug.to_string());
    for w in &facts.workflows {
        if w.path_filtered && is_gate_like(&w.file, w.name.as_deref()) {
            let check = w.name.clone().unwrap_or_else(|| w.file.clone());
            if already_owned(&owner_assignments, &check) {
                continue;
            }
            owner_assignments.push(OwnerAssignment {
                check,
                owner: owner.clone(),
                disposition: OwnershipDisposition::MisconfiguredGate {
                    detail: format!(
                        "`{}` is path-filtered (on.*.paths); if required, off-path PRs strand it as Expected → blocked",
                        w.file
                    ),
                },
                rationale: "confirm whether this is a required check; if so, drop the path filter and gate inside an always-run job".to_string(),
            });
        }
    }

    // (c) Banned AGPL licence label in the container recipe — a licence matter,
    //     so it is owner-only (doctrine #6): flagged, never auto-edited.
    for recipe in ["Containerfile", "Dockerfile"] {
        if let Ok(text) = std::fs::read_to_string(repo_root.join(recipe)) {
            if text.contains("AGPL") {
                owner_assignments.push(OwnerAssignment {
                    check: "container-build".to_string(),
                    owner: slug.to_string(),
                    disposition: OwnershipDisposition::MisconfiguredGate {
                        detail: format!(
                            "`{recipe}` declares an AGPL licence label; estate policy bans AGPL (use MPL-2.0)"
                        ),
                    },
                    rationale: "licence edits are owner-only (doctrine #6) — flagged, never auto-edited".to_string(),
                });
            }
        }
    }

    (escalations, owner_assignments)
}

/// Whether a workflow file looks like an estate compliance gate (so a path
/// filter on it is worth surfacing). Conservative allowlist by keyword.
fn is_gate_like(file: &str, name: Option<&str>) -> bool {
    let hay = format!("{} {}", file, name.unwrap_or("")).to_ascii_lowercase();
    [
        "well-known",
        "wellknown",
        "security",
        "governance",
        "rsr",
        "scorecard",
        "compliance",
    ]
    .iter()
    .any(|k| hay.contains(k))
}

fn already_owned(assignments: &[OwnerAssignment], check: &str) -> bool {
    assignments.iter().any(|a| a.check == check)
}

#[cfg(test)]
mod tests {
    use super::*;
    use squabble_core::gate::{CheckRun, RequiredCheck};

    // The five reds of ipv6-only#43, as a gate.
    fn pr43_gate() -> Gate {
        Gate::new(vec![
            RequiredCheck::new("lint-shell", CheckRun::Failed),
            RequiredCheck::new("container-build", CheckRun::Failed),
            RequiredCheck::new("lint-workflows", CheckRun::Failed),
            RequiredCheck::new("governance / Workflow security linter", CheckRun::Failed),
            RequiredCheck::new("governance / Well-Known (RFC 9116 + RSR)", CheckRun::Failed),
            RequiredCheck::new("check", CheckRun::Passed),
        ])
    }

    fn wf(
        file: &str,
        name: &str,
        job_ids: &[&str],
        reusable: &[&str],
        path_filtered: bool,
        kind: workflows::WorkflowKind,
    ) -> workflows::WorkflowInfo {
        workflows::WorkflowInfo {
            file: file.to_string(),
            name: Some(name.to_string()),
            job_ids: job_ids.iter().map(|s| s.to_string()).collect(),
            job_names: vec![],
            reusable_repos: reusable.iter().map(|s| s.to_string()).collect(),
            path_filtered,
            kind,
        }
    }

    fn pr43_facts() -> WorkflowFacts {
        use workflows::WorkflowKind;
        WorkflowFacts {
            workflows: vec![
                wf(
                    "ci.yml",
                    "CI",
                    &["lint-shell", "container-build"],
                    &[],
                    false,
                    WorkflowKind::CodeBuild,
                ),
                wf(
                    "workflow-linter.yml",
                    "Workflow Security Linter",
                    &["lint-workflows"],
                    &[],
                    true,
                    WorkflowKind::Hygiene,
                ),
                wf(
                    "governance.yml",
                    "Governance",
                    &["governance"],
                    &["hyperpolymath/standards"],
                    false,
                    WorkflowKind::Other,
                ),
            ],
        }
    }

    #[test]
    fn classifies_all_five_reds_of_pr43() {
        let gate = pr43_gate();
        let ctx = RepoContext::default();
        let facts = pr43_facts();
        let out = plan(
            &gate,
            "hyperpolymath/ipv6-only",
            Path::new("/nonexistent"),
            &ctx,
            &facts,
        );
        let Outcome::Red { report } = out else {
            panic!("expected Red plan");
        };
        // lint-shell + container-build → escalations.
        assert!(report.escalations.iter().any(|e| e.check == "lint-shell"));
        assert!(report
            .escalations
            .iter()
            .any(|e| e.check == "container-build"));
        // both governance reusables → owned upstream by standards.
        assert!(report
            .owner_assignments
            .iter()
            .any(|o| o.check.contains("Well-Known") && o.owner == "hyperpolymath/standards"));
        assert!(report
            .owner_assignments
            .iter()
            .any(|o| o.check.contains("Workflow security linter")
                && o.owner == "hyperpolymath/standards"));
        // lint-workflows stays in the squabbler's lane (self-win).
        assert!(report
            .moves_attempted
            .iter()
            .any(|m| matches!(m, Move::GroundTruthCheckNames { .. })));
        // the passing `check` is not in the work list.
        assert!(!report
            .unsatisfied
            .iter()
            .any(|c| c.required_context == "check"));
    }

    #[test]
    fn plan_at_root_is_fail_safe_on_missing_root() {
        // A nonexistent repo root must not error: loaders yield empty context
        // and facts, the planner falls back to conservative core defaults, AND
        // the report says out loud that the ground-truth was absent.
        let gate = pr43_gate();
        let (ctx, out) = plan_at_root(&gate, "hyperpolymath/ipv6-only", Path::new("/nonexistent"));
        assert!(!ctx.found);
        let Outcome::Red { report } = out else {
            panic!("expected Red plan");
        };
        // Every unsatisfied check is accounted for, plus the honesty blocker.
        assert_eq!(report.blockers.len(), report.unsatisfied.len() + 1);
        assert!(report
            .blockers
            .iter()
            .any(|b| b.contains("ground-truth absent")));
    }

    #[test]
    fn plan_ungrounded_never_reads_the_cwd() {
        // The ungrounded entry point must not graft the daemon's own working
        // directory onto a fight for an unrelated repo: no structural findings
        // from the local tree, and the absent-ground-truth blocker present.
        let gate = pr43_gate();
        let Outcome::Red { report } = plan_ungrounded(&gate, "hyperpolymath/ipv6-only") else {
            panic!("expected Red plan");
        };
        assert!(report
            .blockers
            .iter()
            .any(|b| b.contains("ground-truth absent")));
        // The AGPL/path-filter probes found nothing because nothing was read.
        assert!(!report
            .owner_assignments
            .iter()
            .any(|o| matches!(&o.disposition, OwnershipDisposition::MisconfiguredGate { detail } if detail.contains("AGPL"))));
    }
}
