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
use std::process::Command;

#[derive(Debug, Deserialize)]
struct RollupEntry {
    name: String,
    status: Option<String>,
    conclusion: Option<String>,
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

    Ok(build_gate(&required_contexts, &pr_view.status_check_rollup))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, status: Option<&str>, conclusion: Option<&str>) -> RollupEntry {
        RollupEntry {
            name: name.to_string(),
            status: status.map(String::from),
            conclusion: conclusion.map(String::from),
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
