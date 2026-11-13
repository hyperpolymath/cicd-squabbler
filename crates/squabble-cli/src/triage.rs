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
    self, always_after_cancel, decide, match_log, referral_message, self_sha_lock_records,
    unpinned_setup_bun, Decision, GuardInput, Referral, RuleClass,
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

/// What the command line asked for.
#[derive(Default)]
struct Args {
    log: Option<String>,
    step: Option<String>,
    lock: Option<String>,
    callers: Option<String>,
    workflow: Option<String>,
    json: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--log" => a.log = Some(val("--log")?),
            "--step" => a.step = Some(val("--step")?),
            "--lock" => a.lock = Some(val("--lock")?),
            "--callers" => a.callers = Some(val("--callers")?),
            "--workflow" => a.workflow = Some(val("--workflow")?),
            "--json" => a.json = true,
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if a.log.is_none() && a.lock.is_none() && a.workflow.is_none() {
        return Err("give at least one of --log, --lock, --workflow".into());
    }
    if a.step.is_some() && a.log.is_none() {
        return Err("--step needs --log".into());
    }
    if a.callers.is_some() && a.lock.is_none() {
        return Err("--callers needs --lock".into());
    }
    Ok(a)
}

/// The rules found, in discovery order, one entry per rule id, each with an
/// optional detail line; and the last lines of the log, for a referral.
#[derive(Default)]
struct Findings {
    found: Vec<(&'static str, Option<String>)>,
    signature: Option<String>,
}

impl Findings {
    fn add(&mut self, id: &'static str, detail: Option<String>) {
        if !self.found.iter().any(|(i, _)| *i == id) {
            self.found.push((id, detail));
        }
    }

    fn scan_log(&mut self, path: &str, step: Option<&str>) -> Result<(), String> {
        let text = read(path)?;
        self.signature = Some(tail_lines(&text, 3));
        for id in match_log(&text, step) {
            self.add(id, None);
        }
        Ok(())
    }

    fn scan_lock(&mut self, path: &str, callers_path: Option<&str>) -> Result<(), String> {
        let text = read(path)?;
        let callers: Vec<String> = match callers_path {
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
            self.add(
                "SQ-LOCK-01",
                Some(format!("self-SHA records: {}", flagged.join(", "))),
            );
        }
        Ok(())
    }

    fn scan_workflow(&mut self, path: &str) -> Result<(), String> {
        let text = read(path)?;
        if unpinned_setup_bun(&text) {
            self.add(
                "SQ-PIN-03",
                Some("setup-bun step without an exact bun-version".into()),
            );
        }
        let jobs = always_after_cancel(&text);
        if !jobs.is_empty() {
            self.add(
                "SQ-GEN-03",
                Some(format!(
                    "if: always() after needs, downloading an artifact: {}",
                    jobs.join(", ")
                )),
            );
        }
        Ok(())
    }
}

/// One row per catalogue rule found, with its guard decision and, for a
/// referral, the message Hypatia would receive.
fn build_rows(findings: Findings, step: Option<&str>) -> Vec<Row> {
    let guards = GuardInput {
        autofix: std::env::var("ESTATE_AUTOFIX").ok(),
        ..GuardInput::default()
    };
    let Findings { found, signature } = findings;
    found
        .into_iter()
        .filter_map(|(id, detail)| {
            let rule = rules::rule(id)?;
            let decision = decide(rule, &guards);
            let referral = matches!(decision, Decision::Refer { .. }).then(|| {
                referral_message(
                    &Referral {
                        rule_id: rule.id.to_string(),
                        problem: rule.summary.to_string(),
                        fix: Some(rule.fix.to_string()),
                        risk: rule.refer_when.to_string(),
                        step_name: step.map(String::from),
                        signature: signature.clone(),
                        ..Referral::default()
                    },
                    false,
                )
            });
            Some(Row {
                rule: id,
                class: rule.class,
                decision,
                detail,
                referral,
            })
        })
        .collect()
}

fn print_rows(rows: &[Row], json: bool) -> Result<(), String> {
    if json {
        let s = serde_json::to_string_pretty(rows).map_err(|e| e.to_string())?;
        println!("{s}");
        return Ok(());
    }
    if rows.is_empty() {
        println!("no catalogue rule matched");
    }
    for r in rows {
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

fn run_inner(args: &[String]) -> Result<(), String> {
    let a = parse_args(args)?;
    let mut findings = Findings::default();
    if let Some(p) = &a.log {
        findings.scan_log(p, a.step.as_deref())?;
    }
    if let Some(p) = &a.lock {
        findings.scan_lock(p, a.callers.as_deref())?;
    }
    if let Some(p) = &a.workflow {
        findings.scan_workflow(p)?;
    }
    let rows = build_rows(findings, a.step.as_deref());
    print_rows(&rows, a.json)
}
