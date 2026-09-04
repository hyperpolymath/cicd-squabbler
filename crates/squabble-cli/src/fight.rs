// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble fight` — the CLI front-end to the shared fight planner.
//!
//! The planning brain lives in `squabble-fight` (shared with `squabble-app`
//! so the CLI and the HTTP backend cannot drift apart); this module is only
//! argument parsing, gate acquisition (live via `gh`, or offline via
//! `--gate <file>`), human-facing narration, and the optional `--summon`
//! step that turns each escalation into a live boj-server expert call
//! (feature `boj`; fail-closed when the experts are unreachable).

use crate::fetch;
use squabble_core::gate::Gate;
use squabble_core::moves::Move;
use squabble_core::outcome::Escalation;
use squabble_core::polarity::{Applicability, Evidence, RepoDeclaration};
use squabble_core::outcome::Outcome;
use squabble_fight::context::RepoContext;
use std::path::PathBuf;
use std::process::ExitCode;

struct FightArgs {
    slug: String,
    pr: Option<String>,
    repo_root: PathBuf,
    gate_file: Option<String>,
    json: bool,
    summon: bool,
    apply: bool,
}

// Shared with the top-level help in main.rs so the two cannot drift.
pub(crate) const USAGE: &str = "usage: squabble fight <owner>/<repo> <pr> [--repo-root <path>] [--gate <file>] [--json] [--summon] [--apply]";

/// Entry point for `squabble fight`; `rest` is the args after the subcommand.
pub fn run(rest: &[String]) -> ExitCode {
    let args = match parse_args(rest) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };

    let (gate, greens) = match load_gate(&args) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("squabble fight: {e}");
            return ExitCode::from(2);
        }
    };

    let (context, mut outcome) = squabble_fight::plan_at_root(&gate, &args.slug, &args.repo_root);

    // `fight` classifies only reds, so a check that could not run reports green
    // and is never inspected. Surface those before anything else acts on the
    // outcome — including `--summon`, which must record its honest
    // non-dispatch for them.
    let vacuity = classify_greens(&args, &greens);
    attach_vacuity(&mut outcome, &vacuity);

    // `--apply` enacts the appliable self-win moves (v0.1: path-filter strips)
    // by writing the workflow files — and nothing more. It never commits or
    // pushes, and it never re-runs the checks, so the gate stays honestly red
    // (only CI can turn a check green); the report gains an `applied` section
    // recording exactly what was written. Default (propose) leaves the tree
    // untouched.
    if args.apply {
        if let Outcome::Red { report } = &mut outcome {
            let result =
                squabble_fight::apply::apply_moves(&args.repo_root, &report.moves_attempted);
            let applied_n = result.applied.len();
            report.applied = result.applied;
            report.blockers.extend(result.skipped);
            if applied_n > 0 {
                report.blockers.push(format!(
                    "applied {applied_n} self-win move(s) to the working tree — re-run CI to \
                     confirm the gate; nothing was committed or pushed (that is the operator's step)"
                ));
            }
        }
    }

    if args.summon {
        // Without the `boj` feature there is no client compiled in. Refusing
        // loudly honours `no-silent-skip`: pretending to summon would be
        // worse than failing.
        #[cfg(not(feature = "boj"))]
        {
            eprintln!(
                "squabble fight --summon: this build has no boj-server client — \
                 rebuild with `--features boj` to summon experts"
            );
            return ExitCode::from(2);
        }
        #[cfg(feature = "boj")]
        crate::boj::summon(&mut outcome, &args.repo_root);
    }

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

/// Classify the checks that concluded **green**, per the `gate_triage`
/// directive.
///
/// `fight` only ever classifies reds, so a check that could not run reports
/// green and is never inspected. This is the missing polarity.
fn classify_greens(args: &FightArgs, greens: &[fetch::GreenCheck]) -> Vec<Move> {
    let signature = squabble_fight::gate_triage::load_signature(&args.repo_root);
    if !signature.is_usable() {
        // Fail-safe: no directive means "detect no vacuity", never "detect it
        // everywhere". It also costs zero API calls on repos without one.
        return Vec::new();
    }
    // No gate declares an applicability predicate today, so Axis 0 falls
    // through to the signature. Stated explicitly rather than assumed.
    let applicability = Applicability::default();
    let declared = RepoDeclaration::default();

    let mut moves = Vec::new();
    for g in greens {
        let steps = match fetch::fetch_step_outcomes(&args.slug, g.job_id) {
            Ok(s) => s,
            Err(e) => {
                // `no-silent-skip`: an uninspectable green is reported, never
                // quietly assumed genuine.
                eprintln!(
                    "squabble fight: could not inspect green check `{}`: {e}",
                    g.name
                );
                continue;
            }
        };
        if steps.is_empty() {
            // The jobs API returned no steps. That is absence of evidence, not
            // evidence the job ran nothing, so `classify` refuses to escalate
            // it — but `no-silent-skip` still owes the operator a word, exactly
            // as the fetch-error branch above does.
            eprintln!(
                "squabble fight: green check `{}` recorded no steps — not inspectable, not judged",
                g.name
            );
            continue;
        }
        // One run inspected, and it is the run being judged. `upstream-exists`
        // and `target-tech-present` are not observable from the jobs API — no
        // gate declares the globs that would make them computable — so they are
        // reported `unmeasured` rather than asserted, which also keeps the
        // recommendation on the non-destructive branch.
        //
        // With `run_count == 1` the stub rate can only be 0.0 or 1.0, and it is
        // measured from the same predicate `classify` uses rather than assumed:
        // hardcoding 1.0 reported stub evidence for jobs that genuinely ran.
        let evidence = Evidence {
            run_count: 1,
            stub_rate: if signature.matches(&steps) { 1.0 } else { 0.0 },
            upstream_exists: None,
            target_tech_present: None,
        };
        let verdict = squabble_core::polarity::classify(
            &steps,
            &signature,
            &applicability,
            &declared,
            evidence,
        );
        if let Some(m) = verdict.to_move(&g.name) {
            moves.push(m);
        }
    }
    moves
}

/// Fold vacuity findings into the outcome **without changing its colour**.
fn attach_vacuity(outcome: &mut Outcome, moves: &[Move]) {
    if moves.is_empty() {
        return;
    }
    match outcome {
        Outcome::Red { report } => {
            for m in moves {
                if let Some(e) = Escalation::from_move(m) {
                    report.escalations.push(e);
                }
            }
        }
        // A gate that is green overall carries no `Report`, and giving
        // `Outcome::Green` one would change an outcome state — which issue #58
        // rules is spec-first work, not something to slip in here. stderr keeps
        // the finding visible in `--json` mode too, so it is never dropped.
        _ => {
            eprintln!(
                "squabble fight: {} green check(s) are vacuous — not represented in --json \
                 (that needs an Outcome change; see issue #58):",
                moves.len()
            );
            for m in moves {
                eprintln!("  - {}", m.describe());
            }
        }
    }
}

fn load_gate(args: &FightArgs) -> Result<(Gate, Vec<fetch::GreenCheck>), String> {
    if let Some(path) = &args.gate_file {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
        // Offline mode inspects no live runs, so there are no green checks to
        // classify — an honest empty set, not a silent skip.
        return serde_json::from_str(&text)
            .map(|g| (g, Vec::new()))
            .map_err(|e| format!("`{path}` is not a valid gate: {e}"));
    }
    match &args.pr {
        Some(pr) => fetch::run_with_greens(&args.slug, pr),
        None => Err(format!(
            "need a PR number (live) or `--gate <file>` (offline).\n{USAGE}"
        )),
    }
}

fn parse_args(rest: &[String]) -> Result<FightArgs, String> {
    let mut slug = None;
    let mut pr = None;
    let mut repo_root = PathBuf::from(".");
    let mut gate_file = None;
    let mut json = false;
    let mut summon = false;
    let mut apply = false;

    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--repo-root" => {
                let v = rest
                    .get(i + 1)
                    .ok_or_else(|| format!("--repo-root needs a value\n{USAGE}"))?;
                repo_root = PathBuf::from(v);
                i += 2;
            }
            "--gate" => {
                let v = rest
                    .get(i + 1)
                    .ok_or_else(|| format!("--gate needs a value\n{USAGE}"))?;
                gate_file = Some(v.clone());
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            "--summon" => {
                summon = true;
                i += 1;
            }
            "--apply" => {
                apply = true;
                i += 1;
            }
            s if s.starts_with("--") => return Err(format!("unknown flag `{s}`\n{USAGE}")),
            s => {
                if slug.is_none() {
                    slug = Some(s.to_string());
                } else if pr.is_none() {
                    pr = Some(s.to_string());
                } else {
                    return Err(format!("unexpected argument `{s}`\n{USAGE}"));
                }
                i += 1;
            }
        }
    }

    let slug = slug.ok_or_else(|| format!("missing <owner>/<repo>\n{USAGE}"))?;
    if !slug.contains('/') {
        return Err(format!("expected `owner/repo`, got `{slug}`\n{USAGE}"));
    }
    Ok(FightArgs {
        slug,
        pr,
        repo_root,
        gate_file,
        json,
        summon,
        apply,
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
    if !report.applied.is_empty() {
        println!("\n  applied to the working tree (no commit/push — operator's step):");
        for a in &report.applied {
            println!("    - {}: {}", a.file, a.detail);
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

    #[test]
    fn parse_args_accepts_summon_flag() {
        let args = parse_args(&[
            "hyperpolymath/ipv6-only".to_string(),
            "43".to_string(),
            "--summon".to_string(),
        ])
        .expect("parse");
        assert!(args.summon);
        assert_eq!(args.slug, "hyperpolymath/ipv6-only");
    }

    #[test]
    fn parse_args_accepts_apply_flag_default_off() {
        let off = parse_args(&["o/r".to_string(), "1".to_string()]).expect("parse");
        assert!(!off.apply, "apply must default to propose-only");
        let on = parse_args(&["o/r".to_string(), "1".to_string(), "--apply".to_string()])
            .expect("parse");
        assert!(on.apply);
    }

    #[test]
    fn parse_args_rejects_unknown_flags() {
        assert!(parse_args(&["o/r".to_string(), "--bogus".to_string()]).is_err());
    }
}
