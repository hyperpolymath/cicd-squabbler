// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble fight` — the orchestration that turns a stuck gate into a plan.
//!
//! For each unsatisfied required check it decides one of three dispositions,
//! and *nothing else* — it never bypasses:
//!
//! * **self-win** — the red is CI/gate configuration, the squabbler's own lane
//!   (reconcile a required-context name, inject a path-filter pass-through,
//!   re-pin a reusable, ground-truth workflow hygiene);
//! * **escalate** — the red is out of lane (a code/build fix, a proof, a scan)
//!   and is handed to a specialist [`ExpertGroup`] with evidence;
//! * **assign owner** — the red is produced upstream or is structurally
//!   misconfigured, so it is put into the debate with a named owner.
//!
//! The result is emitted as an [`Outcome`] — the first binary to do so. In this
//! v0.1 the fight *plans and reports* (fail-closed, evidence-per-step); actually
//! applying self-win moves and re-running remains the open `apply`/App step
//! (see `STATE.a2ml`), so a still-red gate is honestly reported as
//! [`Outcome::Red`] carrying the full manifest rather than a faked green.

use crate::context::RepoContext;
use crate::fetch;
use crate::workflows::WorkflowFacts;
use squabble_core::gate::Gate;
use squabble_core::moves::{EscalationKind, ExpertGroup, Move, OwnershipDisposition};
use squabble_core::outcome::{Escalation, Outcome, OwnerAssignment, Report};
use squabble_core::{diagnose_with_hints, Diagnosis};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

struct FightArgs {
    slug: String,
    pr: Option<String>,
    repo_root: PathBuf,
    gate_file: Option<String>,
    json: bool,
}

/// Entry point for `squabble fight`; `rest` is the args after the subcommand.
pub fn run(rest: &[String]) -> ExitCode {
    let args = match parse_args(rest) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };

    let gate = match load_gate(&args) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("squabble fight: {e}");
            return ExitCode::from(2);
        }
    };

    let context = RepoContext::load(&args.repo_root);
    let facts = WorkflowFacts::load(&args.repo_root);

    let outcome = fight(&gate, &args.slug, &args.repo_root, &context, &facts);
    print_human(&args.slug, &context, &outcome);

    if args.json {
        match serde_json::to_string_pretty(&outcome) {
            Ok(j) => println!("\n{j}"),
            Err(e) => {
                eprintln!("squabble fight: could not serialise outcome: {e}");
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::SUCCESS
}

/// The pure planning core: classify every unsatisfied check and assemble the
/// [`Outcome`]. Pure over its inputs (no IO beyond the structural file probes,
/// which are read-only), so it is the unit of test coverage.
fn fight(
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
    structural_scan(
        repo_root,
        slug,
        context,
        &unsatisfied,
        facts,
        &mut escalations,
        &mut owner_assignments,
    );

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
            blockers,
        },
    }
}

/// Read-only probes for latent structural issues worth putting into the debate.
fn structural_scan(
    repo_root: &Path,
    slug: &str,
    context: &RepoContext,
    unsatisfied: &[squabble_core::gate::RequiredCheck],
    facts: &WorkflowFacts,
    escalations: &mut Vec<Escalation>,
    owner_assignments: &mut Vec<OwnerAssignment>,
) {
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
            if already_owned(owner_assignments, &check) {
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

fn load_gate(args: &FightArgs) -> Result<Gate, String> {
    if let Some(path) = &args.gate_file {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
        return serde_json::from_str(&text)
            .map_err(|e| format!("`{path}` is not a valid gate: {e}"));
    }
    match &args.pr {
        Some(pr) => fetch::run(&args.slug, pr),
        None => Err(
            "need a PR number (live) or `--gate <file>` (offline). \
             usage: squabble fight <owner>/<repo> <pr> [--repo-root <path>] [--gate <file>] [--json]"
                .to_string(),
        ),
    }
}

fn parse_args(rest: &[String]) -> Result<FightArgs, String> {
    let usage =
        "usage: squabble fight <owner>/<repo> <pr> [--repo-root <path>] [--gate <file>] [--json]";
    let mut slug = None;
    let mut pr = None;
    let mut repo_root = PathBuf::from(".");
    let mut gate_file = None;
    let mut json = false;

    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--repo-root" => {
                let v = rest
                    .get(i + 1)
                    .ok_or_else(|| format!("--repo-root needs a value\n{usage}"))?;
                repo_root = PathBuf::from(v);
                i += 2;
            }
            "--gate" => {
                let v = rest
                    .get(i + 1)
                    .ok_or_else(|| format!("--gate needs a value\n{usage}"))?;
                gate_file = Some(v.clone());
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            s if s.starts_with("--") => return Err(format!("unknown flag `{s}`\n{usage}")),
            s => {
                if slug.is_none() {
                    slug = Some(s.to_string());
                } else if pr.is_none() {
                    pr = Some(s.to_string());
                } else {
                    return Err(format!("unexpected argument `{s}`\n{usage}"));
                }
                i += 1;
            }
        }
    }

    let slug = slug.ok_or_else(|| format!("missing <owner>/<repo>\n{usage}"))?;
    if !slug.contains('/') {
        return Err(format!("expected `owner/repo`, got `{slug}`\n{usage}"));
    }
    Ok(FightArgs {
        slug,
        pr,
        repo_root,
        gate_file,
        json,
    })
}

fn print_human(slug: &str, context: &RepoContext, outcome: &Outcome) {
    let Outcome::Red { report } = outcome else {
        // v0.1 fight only ever produces Red (plan); other arms are future apply.
        println!("gate won.");
        return;
    };
    println!("squabble fight {slug}");
    println!("  {}", report.summary);
    if context.found {
        if let Some(owner) = &context.coordination_owner {
            println!("  context: coordinates with `{owner}`");
        }
        if !context.is_not.is_empty() {
            println!(
                "  identity: {} IS-NOT boundaries read from a2ml",
                context.is_not.len()
            );
        }
    } else {
        println!("  context: no .machine_readable/ descriptiles found — using workflow ground-truth only");
    }

    if !report.moves_attempted.is_empty() {
        println!("\n  self-win moves (squabbler's lane):");
        for m in &report.moves_attempted {
            println!("    - {}", m.describe());
        }
    }
    if !report.escalations.is_empty() {
        println!("\n  escalations (call in the big guns):");
        for e in &report.escalations {
            println!(
                "    - `{}` → {} [{}]: {}",
                e.check,
                e.group.label(),
                e.obligation.label(),
                e.evidence
            );
        }
    }
    if !report.owner_assignments.is_empty() {
        println!("\n  owner assignments (into the debate):");
        for o in &report.owner_assignments {
            println!(
                "    - `{}` → `{}` [{}]: {}",
                o.check,
                o.owner,
                o.disposition.label(),
                o.rationale
            );
        }
    }
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
        kind: crate::workflows::WorkflowKind,
    ) -> crate::workflows::WorkflowInfo {
        crate::workflows::WorkflowInfo {
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
        use crate::workflows::WorkflowKind;
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
        let out = fight(
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
}
