// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble verify-satisfied <owner/repo> <pr>` — is this PR actually done?
//!
//! Reads the PR (GraphQL, `squabble-forge::pr_done`) and its base branch's
//! gate (REST, `fetch::base_gate`), then asks the pure evaluator in
//! `squabble-core::done`. The verdict goes to stdout as JSON; a human-readable
//! list goes to stderr. Exit `5` means agent work remains — the one answer a
//! hook needs, so hook and human read the same implementation.

use crate::fetch;
use serde::Serialize;
use squabble_core::done::{evaluate, PrState, Verdict, DEFAULT_REVIEW_APPS};
use squabble_forge::pr_done::fetch_pr_done;
use squabble_forge::GhTransport;
use std::process::ExitCode;

pub const USAGE: &str = "usage: squabble verify-satisfied <owner>/<repo> <pr-number>";

/// Exit code for "the PR is not done: agent items remain".
pub const NOT_DONE_EXIT: u8 = 5;

#[derive(Serialize)]
struct Report<'a> {
    repo: &'a str,
    pr: u64,
    head_oid: String,
    base_ref: String,
    done: bool,
    #[serde(flatten)]
    verdict: Verdict,
}

pub fn run(args: &[String]) -> ExitCode {
    let (slug, pr) = match args {
        [slug, pr] => match (slug.split_once('/'), pr.parse::<u64>()) {
            (Some(_), Ok(n)) => (slug.as_str(), n),
            _ => {
                eprintln!("squabble verify-satisfied: {USAGE}");
                return ExitCode::from(2);
            }
        },
        _ => {
            eprintln!("squabble verify-satisfied: {USAGE}");
            return ExitCode::from(2);
        }
    };
    let (owner, name) = slug.split_once('/').expect("checked above");

    let read = match fetch_pr_done(&GhTransport, owner, name, pr) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("squabble verify-satisfied: {e}");
            return ExitCode::from(2);
        }
    };
    let mut facts = read.facts;
    // The base gate only bears on a PR that can still merge: for a merged or
    // closed PR it feeds nothing but the evidence-free listing, never an agent
    // item (pinned by `the_base_gate_never_decides_a_closed_or_merged_verdict`).
    // Skipping the REST call there keeps the commonest done-claim ("landed X")
    // off the rate limit this PAT shares with every other session.
    if facts.state == PrState::Open {
        match fetch::base_gate(slug, &read.base_ref) {
            Ok((contexts, rule_types)) => {
                facts.required_contexts = contexts;
                facts.rule_types = rule_types;
            }
            Err(e) => {
                eprintln!("squabble verify-satisfied: {e}");
                return ExitCode::from(fetch::FetchError::FAILED_EXIT);
            }
        }
    }

    let verdict = evaluate(&facts, DEFAULT_REVIEW_APPS);
    let done = verdict.is_done();
    eprint!("{}", render(slug, pr, &verdict));
    let report = Report {
        repo: slug,
        pr,
        head_oid: read.head_oid,
        base_ref: read.base_ref,
        done,
        verdict,
    };
    match serde_json::to_string_pretty(&report) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("squabble verify-satisfied: could not serialise verdict: {e}");
            return ExitCode::from(2);
        }
    }
    if done {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(NOT_DONE_EXIT)
    }
}

fn render(slug: &str, pr: u64, v: &Verdict) -> String {
    let mut s = format!(
        "{slug}#{pr}: {}\n",
        if v.is_done() {
            "DONE (nothing left for the agent)"
        } else {
            "NOT DONE"
        }
    );
    let mut section = |title: &str, lines: Vec<String>| {
        if !lines.is_empty() {
            s.push_str(&format!("  {title}:\n"));
            for l in lines {
                s.push_str(&format!("    - {l}\n"));
            }
        }
    };
    section(
        "agent must act",
        v.agent_items.iter().map(|i| i.describe()).collect(),
    );
    section(
        "held by a human",
        v.held_by_human.iter().map(|i| i.describe()).collect(),
    );
    section(
        "satisfied without evidence (skipped/neutral)",
        v.evidence_free.clone(),
    );
    section("notes", v.notes.clone());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hook keys on this number; it must not collide with any other code the
    /// binary already uses (2 failure, 3 no gate, 4 chains blocking).
    #[test]
    fn not_done_has_its_own_exit_code() {
        for other in [0u8, 2, 3, 4] {
            assert_ne!(NOT_DONE_EXIT, other);
        }
    }

    #[test]
    fn malformed_arguments_are_a_usage_failure_not_a_verdict() {
        for bad in [
            vec![],
            vec!["norepo".to_string(), "1".to_string()],
            vec!["o/r".to_string(), "x".to_string()],
        ] {
            assert_eq!(run(&bad), ExitCode::from(2));
        }
    }
}
