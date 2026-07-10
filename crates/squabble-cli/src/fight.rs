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
}

// Shared with the top-level help in main.rs so the two cannot drift.
pub(crate) const USAGE: &str = "usage: squabble fight <owner>/<repo> <pr> [--repo-root <path>] [--gate <file>] [--json] [--summon]";

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

    // `mut` is only exercised by the boj build (summon appends evidence).
    #[cfg_attr(not(feature = "boj"), allow(unused_mut))]
    let (context, mut outcome) = squabble_fight::plan_at_root(&gate, &args.slug, &args.repo_root);

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

fn load_gate(args: &FightArgs) -> Result<Gate, String> {
    if let Some(path) = &args.gate_file {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
        return serde_json::from_str(&text)
            .map_err(|e| format!("`{path}` is not a valid gate: {e}"));
    }
    match &args.pr {
        Some(pr) => fetch::run(&args.slug, pr),
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
    fn parse_args_rejects_unknown_flags() {
        assert!(parse_args(&["o/r".to_string(), "--bogus".to_string()]).is_err());
    }
}
