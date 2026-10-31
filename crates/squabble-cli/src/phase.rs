// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble phase` — classify **where** one Actions job stopped.
//!
//! A red check says *that* a run failed. It never says whether the policy ran,
//! and the estate's investigation separates five outcomes that all wear the same
//! word:
//!
//! 1. the workflow never started (zero jobs) — attributed by `squabble zero-job`,
//!    not by this command;
//! 2. the job died in runner setup / provisioning / lock resolution;
//! 3. a declared tool, credential or configuration prerequisite was missing;
//! 4. the policy executed and concluded failure;
//! 5. the policy executed and succeeded.
//!
//! This command reads a jobs-API payload (one job object, as
//! `GET /repos/{owner}/{repo}/actions/jobs/{job_id}` returns it) plus the
//! repository's `execution_taxonomy.a2ml` directive, and prints the class, the
//! step it is about, and the handoff. `--json` prints the class alone.
//!
//! ```text
//! gh api repos/o/r/actions/jobs/12345 > job.json
//! squabble phase job.json --repo .
//! ```
//!
//! # What this command refuses to say
//!
//! It is a diagnosis, not a verdict. There is deliberately **no exit code that
//! means green**: `0` says a classification was produced (including
//! `policy-step-failed`), `2` is a tool failure. Nothing here edits a workflow,
//! edits a setting, re-runs a job, or drops a required context, and a
//! `verified` class still needs `polarity` (vacuity) and the gate model before
//! anyone may call a repository green.

use squabble_core::execution::{classify, ExecutionClass};
use squabble_core::gate::CheckRun;
use squabble_core::polarity::{StepConclusion, StepOutcome};
use squabble_fight::execution::load_signatures;
use std::path::Path;
use std::process::ExitCode;

pub(crate) const USAGE: &str =
    "usage: squabble phase <job.json> [--repo <path>] [--json]";

struct Args {
    job: String,
    repo: String,
    json: bool,
}

/// Parse one jobs-API payload path, an optional `--repo` checkout (default `.`,
/// where the directive is read from), and `--json`.
fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut a = Args {
        job: String::new(),
        repo: ".".to_string(),
        json: false,
    };
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => a.json = true,
            "--repo" => {
                a.repo = it
                    .next()
                    .ok_or_else(|| format!("--repo needs a path\n{USAGE}"))?
                    .clone()
            }
            s if s.starts_with("--") => return Err(format!("unknown flag `{s}`\n{USAGE}")),
            s => {
                if !a.job.is_empty() {
                    return Err(format!("only one <job.json> may be given\n{USAGE}"));
                }
                a.job = s.to_string();
            }
        }
    }
    if a.job.is_empty() {
        return Err(USAGE.to_string());
    }
    Ok(a)
}

/// Read the job conclusion and step list out of a jobs-API payload.
///
/// A `null` payload or an empty object means *no job was observed* — the
/// zero-job class, which is `squabble zero-job`'s work and is reported here as
/// such rather than guessed at. An absent `steps` array is a legitimate payload
/// and parses to an empty list (which the classifier treats as absence of
/// evidence, never as evidence of anything).
fn parse_job(text: &str) -> Result<(Option<CheckRun>, Vec<StepOutcome>), String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;

    let no_job = match &v {
        serde_json::Value::Null => true,
        serde_json::Value::Object(o) => o.is_empty(),
        _ => false,
    };
    let job = if no_job {
        None
    } else {
        Some(CheckRun::from_github(
            v.get("status").and_then(|s| s.as_str()),
            v.get("conclusion").and_then(|s| s.as_str()),
        ))
    };

    let steps = v
        .get("steps")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .map(|s| {
                    StepOutcome::new(
                        s.get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        StepConclusion::parse(s.get("conclusion").and_then(|c| c.as_str())),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    Ok((job, steps))
}

pub(crate) fn run(rest: &[String]) -> ExitCode {
    let args = match parse_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("squabble phase: {e}");
            return ExitCode::from(2);
        }
    };

    let text = match std::fs::read_to_string(&args.job) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("squabble phase: cannot read `{}`: {e}", args.job);
            return ExitCode::from(2);
        }
    };
    let (job, steps) = match parse_job(&text) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("squabble phase: `{}`: {e}", args.job);
            return ExitCode::from(2);
        }
    };

    let signatures = load_signatures(Path::new(&args.repo));
    let class: ExecutionClass = classify(job, &steps, &signatures);

    if args.json {
        match serde_json::to_string_pretty(&class) {
            Ok(json) => {
                println!("{json}");
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("squabble phase: could not serialise the class: {e}");
                return ExitCode::from(2);
            }
        }
    }

    println!("repo:        {}", args.repo);
    println!(
        "job:         {}",
        match job {
            None => "none observed (no job — the zero-job class)".to_string(),
            Some(c) => format!("{c:?}"),
        }
    );
    println!("steps:       {}", steps.len());
    println!("class:       {}", class.label());
    if let Some(step) = class.step() {
        println!("step:        {step}");
    }
    println!("handoff:     {}", class.handoff().label());
    println!("owner:       {}", class.handoff().owner());
    println!("explains:    {}", class.explain());
    // Stated on every run, so no reader mistakes a classification for a verdict.
    println!(
        "\nNOT a gate verdict: this classifies where the job stopped. It asserts no gate state, \
         proposes no move, and nothing here is a green."
    );
    if !signatures.is_usable() {
        println!(
            "note: no usable execution_taxonomy signatures under `{}` — preflight and \
             prerequisite classes can only be attributed from GitHub's implicit `Set up job`.",
            args.repo
        );
    }

    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn positional_and_flags_parse() {
        let a = parse_args(&args(&["job.json", "--repo", "..", "--json"])).unwrap();
        assert_eq!(a.job, "job.json");
        assert_eq!(a.repo, "..");
        assert!(a.json);
        // Defaults.
        let b = parse_args(&args(&["job.json"])).unwrap();
        assert_eq!(b.repo, ".");
        assert!(!b.json);
    }

    #[test]
    fn bad_usage_is_refused() {
        assert!(parse_args(&args(&[])).is_err());
        assert!(parse_args(&args(&["--repo"])).is_err());
        assert!(parse_args(&args(&["a.json", "b.json"])).is_err());
        assert!(parse_args(&args(&["--wat", "a.json"])).is_err());
    }

    #[test]
    fn a_failed_jobs_payload_parses_its_steps() {
        let json = r#"{
            "id": 42,
            "status": "completed",
            "conclusion": "failure",
            "steps": [
                {"name": "Set up job", "conclusion": "success"},
                {"name": "Checkout repository", "conclusion": "success"},
                {"name": "Cargo test", "conclusion": "failure"},
                {"name": "Post job", "conclusion": null}
            ]
        }"#;
        let (job, steps) = parse_job(json).unwrap();
        assert_eq!(job, Some(CheckRun::Failed));
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[2].conclusion, StepConclusion::Failure);
        // A null conclusion is `Other`, never a skip.
        assert_eq!(steps[3].conclusion, StepConclusion::Other);
    }

    #[test]
    fn a_null_or_empty_payload_is_the_no_job_class() {
        assert_eq!(parse_job("null").unwrap().0, None);
        assert_eq!(parse_job("{}").unwrap().0, None);
        // Classified as class 1, which defers attribution to zero-job.
        let (job, steps) = parse_job("null").unwrap();
        assert_eq!(
            classify(job, &steps, &Default::default()),
            ExecutionClass::NotStarted
        );
    }

    #[test]
    fn a_payload_without_steps_is_absence_of_evidence_not_a_class() {
        let (job, steps) = parse_job(r#"{"status": "completed", "conclusion": "failure"}"#).unwrap();
        assert_eq!(job, Some(CheckRun::Failed));
        assert!(steps.is_empty());
        let c = classify(job, &steps, &Default::default());
        assert_eq!(c.label(), "unclassified");
    }

    #[test]
    fn a_non_json_payload_is_a_tool_failure() {
        assert!(parse_job("<html>nope</html>").is_err());
    }
}
