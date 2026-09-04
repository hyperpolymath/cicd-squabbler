// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Live plumbing: turn a real GitHub PR into a [`squabble_core::gate::Gate`].
//!
//! This is the "git/`gh` plumbing" the README named as v0.1's next step. It
//! shells out to the `gh` CLI (already present on every estate runner and on
//! the owner's machine) rather than adding an HTTP client dependency here —
//! `squabble-core` stays host-agnostic; this module is the host.
//!
//! Two calls, both needed to build a [`Gate`]:
//!
//! 1. the branch ruleset's `required_status_checks` contexts (the
//!    *requirement* set — what must pass, independent of what ran), and
//! 2. the PR's `statusCheckRollup` (the *realised* runs on the head commit).
//!
//! A required context with no matching rollup entry is [`CheckRun::Missing`];
//! matching-but-incomplete is [`CheckRun::Pending`]; a `SUCCESS` conclusion is
//! [`CheckRun::Passed`]; anything else that completed is [`CheckRun::Failed`].

use serde::Deserialize;
use squabble_core::gate::{CheckRun, Gate, RequiredCheck};
use squabble_core::polarity::{StepConclusion, StepOutcome};
use std::process::Command;

#[derive(Debug, Deserialize)]
struct RollupEntry {
    name: String,
    status: Option<String>,
    conclusion: Option<String>,
    /// `https://github.com/O/R/actions/runs/<run>/job/<job>` — the only place
    /// the rollup exposes a job id, which is what the jobs API needs.
    #[serde(rename = "detailsUrl")]
    details_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PrView {
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    #[serde(rename = "statusCheckRollup")]
    status_check_rollup: Vec<RollupEntry>,
}

#[derive(Debug, Deserialize)]
struct RulesetRule {
    #[serde(rename = "type")]
    rule_type: String,
    parameters: Option<RulesetParameters>,
}

#[derive(Debug, Deserialize)]
struct RulesetParameters {
    #[serde(default)]
    required_status_checks: Vec<RulesetContext>,
}

#[derive(Debug, Deserialize)]
struct RulesetContext {
    context: String,
}

/// Parse a `gh pr view --json baseRefName,statusCheckRollup` payload into the
/// realised-run half of a [`Gate`]. Pure — no IO, fully testable on fixtures.
fn parse_rollup(entry: &RollupEntry) -> CheckRun {
    match entry.conclusion.as_deref() {
        Some("SUCCESS") => CheckRun::Passed,
        Some("FAILURE")
        | Some("ERROR")
        | Some("TIMED_OUT")
        | Some("CANCELLED")
        | Some("STARTUP_FAILURE") => CheckRun::Failed,
        _ => match entry.status.as_deref() {
            Some("COMPLETED") => CheckRun::Failed, // completed with no recognised conclusion
            _ => CheckRun::Pending,
        },
    }
}

/// Build a [`Gate`] from the required-context set and the realised rollup.
/// Pure and the unit of test coverage for this module — the two `gh` calls
/// in [`run`] exist only to produce these two slices from a live PR.
fn build_gate(required_contexts: &[String], rollup: &[RollupEntry]) -> Gate {
    let checks = required_contexts
        .iter()
        .map(|required| {
            let run = rollup
                .iter()
                .find(|r| &r.name == required)
                .map(parse_rollup)
                .unwrap_or(CheckRun::Missing);
            RequiredCheck::new(required.clone(), run)
        })
        .collect();
    Gate::new(checks)
}

/// A check that concluded `success`, and the job whose steps can be inspected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GreenCheck {
    pub name: String,
    pub job_id: u64,
}

#[derive(Debug, Deserialize)]
struct JobStep {
    name: String,
    conclusion: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JobView {
    #[serde(default)]
    steps: Vec<JobStep>,
}

/// Pull the job id out of a rollup entry's `detailsUrl`.
///
/// The rollup exposes no job id field, but the details URL ends
/// `/actions/runs/<run>/job/<job>`. Pure and directly tested: a silent `None`
/// here would mean a green check is never inspected, which is precisely the
/// failure this module exists to catch.
fn job_id_from_details_url(url: &str) -> Option<u64> {
    url.split("/job/").nth(1)?.split('/').next()?.parse().ok()
}

/// The checks that concluded `success` and can actually be inspected.
///
/// A `SUCCESS` entry with an unparseable `detailsUrl` (a status context posted
/// by an app, say — it has no job) is skipped: there are no steps to read.
fn greens_from_rollup(rollup: &[RollupEntry]) -> Vec<GreenCheck> {
    rollup
        .iter()
        .filter(|r| r.conclusion.as_deref() == Some("SUCCESS"))
        .filter_map(|r| {
            let job_id = job_id_from_details_url(r.details_url.as_deref()?)?;
            Some(GreenCheck {
                name: r.name.clone(),
                job_id,
            })
        })
        .collect()
}

/// Parse a jobs-API payload into step outcomes. Pure — the unit of test
/// coverage for [`fetch_step_outcomes`].
fn parse_steps(json: &str) -> Result<Vec<StepOutcome>, String> {
    let job: JobView =
        serde_json::from_str(json).map_err(|e| format!("could not parse job response: {e}"))?;
    Ok(job
        .steps
        .into_iter()
        .map(|s| StepOutcome::new(s.name, StepConclusion::parse(s.conclusion.as_deref())))
        .collect())
}

/// Fetch one job's step conclusions — the declared evidence tier for
/// [`squabble_core::polarity`].
pub fn fetch_step_outcomes(slug: &str, job_id: u64) -> Result<Vec<StepOutcome>, String> {
    let json = run_gh(&["api", &format!("repos/{slug}/actions/jobs/{job_id}")])?;
    parse_steps(&json)
}

fn run_gh(args: &[&str]) -> Result<String, String> {
    let out = Command::new("gh")
        .args(args)
        .output()
        .map_err(|e| format!("failed to run `gh {}`: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "`gh {}` exited {}: {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Fetch a live PR's gate from GitHub via `gh` and return it as a [`Gate`].
///
/// `slug` is `owner/repo`. Requires `gh` to be authenticated for that repo —
/// the same precondition every other `gh`-based estate tool already has.
pub fn run(slug: &str, pr: &str) -> Result<Gate, String> {
    run_with_greens(slug, pr).map(|(gate, _greens)| gate)
}

/// As [`run`], but also returns the checks that concluded **success**, with the
/// job id needed to inspect their steps.
///
/// The green set is what [`squabble_core::polarity`] classifies. `fight` only
/// ever looks at reds, so a gate that could not run reports green and is never
/// inspected — that is the whole fake-green class.
pub fn run_with_greens(slug: &str, pr: &str) -> Result<(Gate, Vec<GreenCheck>), String> {
    let (owner, repo) = slug
        .split_once('/')
        .ok_or_else(|| format!("expected `owner/repo`, got `{slug}`"))?;

    let pr_json = run_gh(&[
        "pr",
        "view",
        pr,
        "--repo",
        slug,
        "--json",
        "baseRefName,statusCheckRollup",
    ])?;
    let pr_view: PrView = serde_json::from_str(&pr_json)
        .map_err(|e| format!("could not parse `gh pr view` output: {e}"))?;

    let rules_json = run_gh(&[
        "api",
        &format!(
            "repos/{owner}/{repo}/rules/branches/{}",
            pr_view.base_ref_name
        ),
    ])?;
    let rules: Vec<RulesetRule> = serde_json::from_str(&rules_json)
        .map_err(|e| format!("could not parse ruleset response: {e}"))?;

    let required_contexts: Vec<String> = rules
        .into_iter()
        .filter(|r| r.rule_type == "required_status_checks")
        .filter_map(|r| r.parameters)
        .flat_map(|p| p.required_status_checks)
        .map(|c| c.context)
        .collect();

    if required_contexts.is_empty() {
        return Err(format!(
            "no `required_status_checks` rule found on `{owner}/{repo}` branch `{}` — \
             an unprotected branch has no gate to squabble over",
            pr_view.base_ref_name
        ));
    }

    Ok((
        build_gate(&required_contexts, &pr_view.status_check_rollup),
        greens_from_rollup(&pr_view.status_check_rollup),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, status: Option<&str>, conclusion: Option<&str>) -> RollupEntry {
        RollupEntry {
            name: name.to_string(),
            status: status.map(String::from),
            conclusion: conclusion.map(String::from),
            details_url: None,
        }
    }

    #[test]
    fn passed_conclusion_maps_to_passed() {
        assert_eq!(
            parse_rollup(&entry("x", Some("COMPLETED"), Some("SUCCESS"))),
            CheckRun::Passed
        );
    }

    #[test]
    fn failure_conclusion_maps_to_failed() {
        assert_eq!(
            parse_rollup(&entry("x", Some("COMPLETED"), Some("FAILURE"))),
            CheckRun::Failed
        );
    }

    #[test]
    fn in_progress_maps_to_pending() {
        assert_eq!(
            parse_rollup(&entry("x", Some("IN_PROGRESS"), None)),
            CheckRun::Pending
        );
    }

    #[test]
    fn required_context_absent_from_rollup_is_missing() {
        let gate = build_gate(&["required / never-ran".to_string()], &[]);
        assert_eq!(gate.checks[0].run, CheckRun::Missing);
    }

    #[test]
    fn required_context_matched_to_realised_run_by_exact_name() {
        let rollup = vec![entry(
            "required / it-ran",
            Some("COMPLETED"),
            Some("SUCCESS"),
        )];
        let gate = build_gate(&["required / it-ran".to_string()], &rollup);
        assert_eq!(gate.checks[0].run, CheckRun::Passed);
    }

    #[test]
    fn build_gate_evaluates_green_when_all_required_passed() {
        let rollup = vec![entry("a", Some("COMPLETED"), Some("SUCCESS"))];
        let gate = build_gate(&["a".to_string()], &rollup);
        assert_eq!(gate.evaluate(), squabble_core::gate::GateState::Green);
    }
}

#[cfg(test)]
mod polarity_plumbing_tests {
    use super::*;

    fn green(name: &str, details: Option<&str>) -> RollupEntry {
        RollupEntry {
            name: name.to_string(),
            status: Some("COMPLETED".into()),
            conclusion: Some("SUCCESS".into()),
            details_url: details.map(String::from),
        }
    }

    #[test]
    fn a_job_id_is_read_from_a_real_details_url() {
        // Shape taken from a live `gh pr view --json statusCheckRollup`.
        let url = "https://github.com/hyperpolymath/standards/actions/runs/33817314194/job/100852208701";
        assert_eq!(job_id_from_details_url(url), Some(100852208701));
    }

    #[test]
    fn a_details_url_with_no_job_segment_yields_none() {
        // A status context posted by an app has no job, so there are no steps
        // to inspect. It must be skipped, not guessed at.
        assert_eq!(
            job_id_from_details_url("https://example.com/build/status"),
            None
        );
        assert_eq!(
            job_id_from_details_url(
                "https://github.com/o/r/actions/runs/1"
            ),
            None
        );
    }

    #[test]
    fn only_successful_checks_with_an_inspectable_job_are_green() {
        let rollup = vec![
            green("has-a-job", Some("https://g/o/r/actions/runs/1/job/42")),
            green("no-details-url", None),
            green("not-a-job", Some("https://example.com/status")),
            RollupEntry {
                name: "red".into(),
                status: Some("COMPLETED".into()),
                conclusion: Some("FAILURE".into()),
                details_url: Some("https://g/o/r/actions/runs/1/job/43".into()),
            },
        ];
        let greens = greens_from_rollup(&rollup);
        assert_eq!(
            greens,
            vec![GreenCheck {
                name: "has-a-job".into(),
                job_id: 42
            }],
            "reds are `fight`'s job; only inspectable greens belong here"
        );
    }

    #[test]
    fn step_conclusions_are_parsed_from_a_jobs_api_payload() {
        let json = r#"{
            "id": 42,
            "conclusion": "success",
            "steps": [
                {"name": "Set up job", "conclusion": "success"},
                {"name": "Run Hypatia scan", "conclusion": "skipped"},
                {"name": "Create stub findings", "conclusion": "success"},
                {"name": "Post job", "conclusion": null}
            ]
        }"#;
        let steps = parse_steps(json).expect("valid payload");
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[1].name, "Run Hypatia scan");
        assert_eq!(steps[1].conclusion, StepConclusion::Skipped);
        assert_eq!(steps[2].conclusion, StepConclusion::Success);
        // A null conclusion must not be mistaken for a skip — a skip is half
        // the vacuity signature.
        assert_eq!(steps[3].conclusion, StepConclusion::Other);
    }

    #[test]
    fn a_payload_with_no_steps_parses_to_an_empty_list() {
        // `NoStepsRecorded` is a real vacuity cause, so this must parse rather
        // than error.
        let steps = parse_steps(r#"{"id": 1, "conclusion": "success"}"#).expect("valid");
        assert!(steps.is_empty());
    }

    #[test]
    fn the_parsed_steps_classify_as_vacuous_end_to_end() {
        // The whole chain, with nothing synthetic on the signature side. This
        // payload is the LIVE shape of hyperpolymath/session-sentinel run
        // 33813809227 (jobs API, 2026-09-04): a green Hypatia check whose
        // scanner never ran. It is matched against THIS REPO'S OWN directive
        // file rather than an inline fixture.
        //
        // The fixture this test used to carry named the step "Create stub
        // findings" on BOTH sides, so it agreed with itself and proved nothing
        // — the directive's abbreviation matched no real job, and the test
        // could not see that. Loading the real file is what makes drift on
        // either side fail here.
        let json = r#"{"steps":[
            {"name":"Run Hypatia scan","conclusion":"skipped"},
            {"name":"Create stub findings (when Hypatia unavailable)","conclusion":"success"}
        ]}"#;
        let steps = parse_steps(json).expect("valid payload");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root");
        let sig = squabble_fight::gate_triage::load_signature(root);
        let verdict = squabble_core::polarity::classify(
            &steps,
            &sig,
            &squabble_core::polarity::Applicability::default(),
            &squabble_core::polarity::RepoDeclaration::default(),
            squabble_core::polarity::Evidence {
                run_count: 1,
                stub_rate: 1.0,
                upstream_exists: None,
                target_tech_present: None,
            },
        );
        assert!(
            matches!(
                verdict,
                squabble_core::polarity::PolarityVerdict::Vacuous { .. }
            ),
            "got {verdict:?}"
        );
        assert!(verdict.to_move("scan / hypatia").is_some());
    }

    #[test]
    fn a_scanner_that_really_ran_is_not_called_vacuous() {
        // The negative control. Live shape of hyperpolymath/echidnabot run
        // 33712324720: the same gate, same job, but the scan actually ran and
        // the stub was skipped. If this ever returns Vacuous the classifier is
        // condemning working gates.
        let json = r#"{"steps":[
            {"name":"Run Hypatia scan","conclusion":"success"},
            {"name":"Create stub findings (when Hypatia unavailable)","conclusion":"skipped"}
        ]}"#;
        let steps = parse_steps(json).expect("valid payload");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root");
        let sig = squabble_fight::gate_triage::load_signature(root);
        let verdict = squabble_core::polarity::classify(
            &steps,
            &sig,
            &squabble_core::polarity::Applicability::default(),
            &squabble_core::polarity::RepoDeclaration::default(),
            squabble_core::polarity::Evidence {
                run_count: 1,
                stub_rate: 0.0,
                upstream_exists: None,
                target_tech_present: None,
            },
        );
        assert!(
            !matches!(
                verdict,
                squabble_core::polarity::PolarityVerdict::Vacuous { .. }
            ),
            "a gate that ran must not be reported vacuous; got {verdict:?}"
        );
    }
}
