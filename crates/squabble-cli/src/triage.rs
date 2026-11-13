// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble triage` — read-only first-line classification (ADR 0004).
//!
//! Matches a job log, an `actions.lock` and a workflow against the rule
//! catalogue and prints each rule, its class and the guard decision. It never
//! writes and never calls the network, Hypatia included, so a referral is
//! printed as if Hypatia were unreachable.

use serde::Serialize;
use squabble_core::rules::{
    self, decide, match_log, referral_message, self_sha_lock_records, unpinned_setup_bun, Decision,
    GuardInput, Referral, RuleClass,
};
use std::process::ExitCode;

pub const USAGE: &str = "usage: squabble triage [--log <file> [--step <name>]] [--lock <file> [--callers <file>]] [--workflow <file>] [--json]";

#[derive(Serialize)]
struct Row {
    rule: &'static str,
    class: RuleClass,
    decision: Decision,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    referral: Option<String>,
}

fn tail_lines(text: &str, n: usize) -> String {
    let v: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    v[v.len().saturating_sub(n)..].join("\n")
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path}`: {e}"))
}

pub fn run(args: &[String]) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("squabble triage: {e}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run_inner(args: &[String]) -> Result<(), String> {
    let (mut log, mut step, mut lock, mut callers, mut workflow) = (None, None, None, None, None);
    let mut json = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--log" => log = Some(val("--log")?),
            "--step" => step = Some(val("--step")?),
            "--lock" => lock = Some(val("--lock")?),
            "--callers" => callers = Some(val("--callers")?),
            "--workflow" => workflow = Some(val("--workflow")?),
            "--json" => json = true,
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if log.is_none() && lock.is_none() && workflow.is_none() {
        return Err("give at least one of --log, --lock, --workflow".into());
    }
    if step.is_some() && log.is_none() {
        return Err("--step needs --log".into());
    }
    if callers.is_some() && lock.is_none() {
        return Err("--callers needs --lock".into());
    }

    // (rule id, detail) in discovery order, one row per rule id.
    let mut signature: Option<String> = None;
    let mut found: Vec<(&'static str, Option<String>)> = Vec::new();
    let mut add = |id: &'static str, detail: Option<String>| {
        if !found.iter().any(|(i, _)| *i == id) {
            found.push((id, detail));
        }
    };
    if let Some(p) = &log {
        let text = read(p)?;
        signature = Some(tail_lines(&text, 3));
        for id in match_log(&text, step.as_deref()) {
            add(id, None);
        }
    }
    if let Some(p) = &lock {
        let text = read(p)?;
        let callers: Vec<String> = match &callers {
            Some(c) => read(c)?
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect(),
            None => Vec::new(),
        };
        let flagged = self_sha_lock_records(&text, &callers);
        if !flagged.is_empty() {
            add(
                "SQ-LOCK-01",
                Some(format!("self-SHA records: {}", flagged.join(", "))),
            );
        }
    }
    if let Some(p) = &workflow {
        if unpinned_setup_bun(&read(p)?) {
            add(
                "SQ-PIN-03",
                Some("setup-bun step without an exact bun-version".into()),
            );
        }
    }

    let guards = GuardInput {
        autofix: std::env::var("ESTATE_AUTOFIX").ok(),
        ..GuardInput::default()
    };
    let rows: Vec<Row> = found
        .into_iter()
        .filter_map(|(id, detail)| {
            let rule = rules::rule(id)?;
            let decision = decide(rule, &guards);
            let referral = match (&decision, rule.class) {
                (Decision::Refer { .. }, _) => Some(referral_message(
                    &Referral {
                        rule_id: rule.id.to_string(),
                        problem: rule.summary.to_string(),
                        fix: Some(rule.fix.to_string()),
                        risk: rule.refer_when.to_string(),
                        step_name: step.clone(),
                        signature: signature.clone(),
                        ..Referral::default()
                    },
                    false,
                )),
                _ => None,
            };
            Some(Row {
                rule: id,
                class: rule.class,
                decision,
                detail,
                referral,
            })
        })
        .collect();

    if json {
        let s = serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())?;
        println!("{s}");
        return Ok(());
    }
    if rows.is_empty() {
        println!("no catalogue rule matched");
    }
    for r in &rows {
        println!("{}\t{:?}\t{:?}", r.rule, r.class, r.decision);
        if let Some(d) = &r.detail {
            println!("  {d}");
        }
        if let Some(m) = &r.referral {
            for l in m.lines() {
                println!("  {l}");
            }
        }
    }
    Ok(())
}
