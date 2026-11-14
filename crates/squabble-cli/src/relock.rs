// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble relock` — the SQ-LOCK-01 executor (ADR 0004).
//!
//! Repairs the `actions.lock` records whose `ref:` repeats their own SHA. It
//! never touches the network: the evidence (the tags at each SHA, the branches
//! that contain it) comes from a TSV file the caller gathered. It changes
//! nothing unless `--write` is given AND the guards say Apply (the
//! `ESTATE_AUTOFIX` kill switch, the attempt and open-PR limits). It never
//! opens a PR and never claims the rule's check passed: that check is the
//! repository's lock gate, run on the PR.

use squabble_core::lockfix::{plan_lock_fix, reusable_callers, LockFixPlan, RefEvidence};
use squabble_core::rules::{decide, rule, Decision, GuardInput};
use std::collections::BTreeMap;
use std::process::ExitCode;

pub const USAGE: &str = "usage: squabble relock --lock <file> --evidence <tsv> (--workflows <dir> | --callers <file>) [--attempts <n>] [--open-fix-pr] [--write]";

#[derive(Default)]
struct Args {
    lock: Option<String>,
    evidence: Option<String>,
    callers: Option<String>,
    workflows: Option<String>,
    attempts: u32,
    open_fix_pr: bool,
    write: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            "--lock" => a.lock = Some(value("--lock")?),
            "--evidence" => a.evidence = Some(value("--evidence")?),
            "--callers" => a.callers = Some(value("--callers")?),
            "--workflows" => a.workflows = Some(value("--workflows")?),
            "--attempts" => {
                a.attempts = value("--attempts")?
                    .parse()
                    .map_err(|_| "--attempts needs a whole number".to_string())?
            }
            "--open-fix-pr" => a.open_fix_pr = true,
            "--write" => a.write = true,
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(a)
}

/// Parse the evidence file: `key<TAB>tag|branch|default<TAB>name` per line, `#`
/// comments and blank lines ignored. A malformed line is an error: evidence
/// that was misread must not become a repair.
pub fn parse_evidence(text: &str) -> Result<BTreeMap<String, RefEvidence>, String> {
    let mut out: BTreeMap<String, RefEvidence> = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        let [key, kind, name] = f[..] else {
            return Err(format!(
                "evidence line {}: want key<TAB>kind<TAB>name",
                n + 1
            ));
        };
        let e = out.entry(key.trim().to_ascii_lowercase()).or_default();
        match kind {
            "tag" => e.tags.push(name.to_string()),
            "branch" => e.branches.push(name.to_string()),
            "default" => e.default_branch = Some(name.to_string()),
            other => return Err(format!("evidence line {}: unknown kind `{other}`", n + 1)),
        }
    }
    Ok(out)
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path}`: {e}"))
}

/// Write `text` to `path` through a sibling temp file, so a failed write
/// leaves the original lock intact.
fn write_atomic(path: &str, text: &str) -> Result<(), String> {
    let tmp = format!("{path}.squabble-tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write `{tmp}`: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot replace `{path}`: {e}")
    })
}

fn print_plan(plan: &LockFixPlan) {
    for (key, new) in &plan.edits {
        println!("repair\t{key}\tref -> {new}");
    }
    for (key, why) in &plan.refused {
        println!("refer\t{key}\t{why}");
    }
}

/// The job-level reusable callers, from the workflow directory and/or a list
/// file. One of the two is required: a missing list would leave a caller
/// record flagged as if it were a defect, and a rewrite of it breaks the lock.
fn collect_callers(a: &Args) -> Result<Vec<String>, String> {
    if a.workflows.is_none() && a.callers.is_none() {
        return Err("--workflows or --callers is required (callers are never guessed)".into());
    }
    let mut callers: Vec<String> = Vec::new();
    if let Some(dir) = &a.workflows {
        let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot read `{dir}`: {e}"))?;
        let mut paths: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        paths.sort();
        for p in paths
            .iter()
            .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
        {
            callers.extend(reusable_callers(&read(&p.to_string_lossy())?));
        }
    }
    if let Some(p) = &a.callers {
        callers.extend(read(p)?.lines().map(|l| l.trim().to_ascii_lowercase()));
    }
    callers.sort();
    callers.dedup();
    Ok(callers)
}

pub fn run(args: &[String]) -> ExitCode {
    match run_inner(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("squabble relock: {e}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run_inner(args: &[String]) -> Result<ExitCode, String> {
    let a = parse_args(args)?;
    let lock_path = a.lock.as_deref().ok_or("--lock is required")?;
    let ev_path = a.evidence.as_deref().ok_or("--evidence is required")?;
    let lock = read(lock_path)?;
    let evidence = parse_evidence(&read(ev_path)?)?;
    let callers = collect_callers(&a)?;
    let plan = plan_lock_fix(&lock, &callers, &evidence)?;
    print_plan(&plan);
    let guards = GuardInput {
        autofix: std::env::var("ESTATE_AUTOFIX").ok(),
        attempts_on_head: a.attempts,
        open_fix_pr: a.open_fix_pr,
        ..GuardInput::default()
    };
    let rule = rule("SQ-LOCK-01").ok_or("SQ-LOCK-01 is not in the catalogue")?;
    let decision = decide(rule, &guards);
    println!("decision\tSQ-LOCK-01\t{decision:?}");
    let Some(new_lock) = plan.new_lock.as_deref() else {
        println!("nothing to write");
        return Ok(exit_for(&plan));
    };
    if a.write && decision == Decision::Apply {
        write_atomic(lock_path, new_lock)?;
        println!("wrote\t{lock_path}\tcheck: not run here; the lock gate on the PR is the check");
    } else if a.write {
        println!("not written: the guards say {decision:?}");
        return Ok(ExitCode::from(4));
    } else {
        println!("dry run: pass --write to apply");
    }
    Ok(exit_for(&plan))
}

/// 0 when every flagged record was repaired, 4 when some are referrals.
fn exit_for(plan: &LockFixPlan) -> ExitCode {
    if plan.refused.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_parses_by_kind_and_refuses_junk() {
        let e =
            parse_evidence("# note\n\nA/B@x\ttag\tv1\nA/B@x\tbranch\tmain\nA/B@x\tdefault\tmain\n")
                .unwrap();
        let r = &e["a/b@x"];
        assert_eq!((r.tags.len(), r.branches.len()), (1, 1));
        assert_eq!(r.default_branch.as_deref(), Some("main"));
        for bad in ["k\ttag", "k\tweird\tx", "k tag v1", "k\ttag\tv1\textra"] {
            assert!(parse_evidence(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn arguments_need_values_and_reject_unknown_flags() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(parse_args(&s(&["--lock"])).is_err());
        assert!(parse_args(&s(&["--attempts", "x"])).is_err());
        assert!(parse_args(&s(&["--wat"])).is_err());
        let ok = parse_args(&s(&[
            "--lock",
            "l",
            "--evidence",
            "e",
            "--write",
            "--attempts",
            "2",
        ]))
        .unwrap();
        assert!(ok.write && ok.attempts == 2);
    }
}
