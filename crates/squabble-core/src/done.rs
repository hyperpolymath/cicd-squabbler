// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `verify-satisfied` — the definition of done for a pull request, as a pure
//! function over facts already fetched.
//!
//! The question is not "can this merge?" but **"has the agent finished its part?"**
//! Those differ, and the difference is the whole design:
//!
//! - [`Verdict::agent_items`] — work the agent can and must still do: resolve a
//!   conflict in-file, answer a review thread, act on a `CHANGES_REQUESTED`,
//!   wait for a review bot to report, arm automerge, pick squash. Non-empty ⇒
//!   **not done** (the CLI exits 5).
//! - [`Verdict::held_by_human`] — what only a person can clear: a required
//!   approval, a deployment gate, pressing merge on a PR that GitHub will not let
//!   automerge (it refuses on a `CLEAN` status). Listed, but **done** — "armed and
//!   waiting for the owner" is the intended terminal state, and a gate that failed
//!   on it could never release a session.
//!
//! Required checks that are merely *pending* are not agent work when automerge is
//! armed: GitHub waits for them. Demanding green here would be jointly
//! unsatisfiable with demanding automerge, because GitHub refuses to arm
//! automerge on a PR whose checks are all green.
//!
//! A review bot is different. It is not required, so automerge will not wait for
//! it — and its output is exactly what the agent must have read. A pending review
//! bot is therefore agent work: the one place this gate waits on wall-clock.
//!
//! Not yet evaluated, and said so in [`Verdict::notes`] rather than passed
//! silently: new code-scanning alerts (the PR-minus-base set difference).

use crate::gate::{CheckRun, Gate, RequiredCheck};
use serde::{Deserialize, Serialize};

/// Review-producing Apps, by login with any `[bot]` suffix removed. Only these
/// make a pending check agent work; any other non-required pending check is
/// noise the agent need not wait for.
pub const DEFAULT_REVIEW_APPS: &[&str] = &[
    "coderabbitai",
    "copilot-pull-request-reviewer",
    "sonarqubecloud",
    "codacy-production",
];

/// Ruleset rule types whose effect this gate evaluates, or which cannot hold a
/// squash merge (a squash lands one GitHub-signed commit, so `required_signatures`
/// and `required_linear_history` are met by construction).
const ACCOUNTED_RULE_TYPES: &[&str] = &[
    "required_status_checks",
    "pull_request",
    "deletion",
    "non_fast_forward",
    "creation",
    "update",
    "required_linear_history",
    "required_signatures",
    "required_deployments",
    "code_scanning",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

/// GitHub's `mergeable`. `Unknown` is computed lazily and is common on the first
/// read after a push — it means "not yet", never "conflicting".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mergeability {
    Mergeable,
    Conflicting,
    Unknown,
}

/// One context on the head commit, required or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observed {
    pub name: String,
    /// App slug (check run) or creator login (status), `[bot]` removed.
    pub producer: String,
    pub run: CheckRun,
}

/// An unresolved review thread. An *outdated* one still blocks
/// `required_review_thread_resolution`, so it is counted and tagged, not skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub author: String,
    pub path: Option<String>,
    pub line: Option<u64>,
    pub outdated: bool,
    pub url: String,
}

/// Everything the verdict needs, fetched once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrFacts {
    pub state: PrState,
    pub is_draft: bool,
    pub mergeable: Mergeability,
    /// GitHub's `mergeStateStatus`, verbatim (`BLOCKED`, `CLEAN`, `BEHIND`, …).
    pub merge_state: String,
    /// `reviewDecision`: `APPROVED`, `REVIEW_REQUIRED`, `CHANGES_REQUESTED`, or none.
    pub review_decision: Option<String>,
    /// Armed automerge's method (`SQUASH`, `MERGE`, `REBASE`), if armed.
    pub auto_merge: Option<String>,
    /// Required contexts from rulesets ∪ classic protection. Empty is a real
    /// answer ("nothing required"), not an error.
    pub required_contexts: Vec<String>,
    /// Every context on the head, from a fully paginated read.
    pub observed: Vec<Observed>,
    pub unresolved_threads: Vec<Thread>,
    /// Reviewers whose latest opinionated review is `CHANGES_REQUESTED`.
    pub changes_requested_by: Vec<String>,
    /// `[.[].type]` of the effective rules on the base branch.
    pub rule_types: Vec<String>,
    /// The PR description, verbatim. Where a red non-required check is
    /// acknowledged (see [`acknowledged_in`]). Empty when there is none.
    pub body: String,
}

/// One unmet condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    Conflicting,
    MergeabilityUnknown,
    Draft,
    RequiredNotSatisfied { context: String, run: CheckRun },
    CheckFailed { context: String, producer: String },
    UnresolvedThread(Thread),
    ChangesRequested { reviewer: String },
    ReviewBotPending { producer: String, context: String },
    AutoMergeNotArmed { merge_state: String },
    WrongMergeMethod { method: String },
    BranchBehind,
    MergeQueue,
    AwaitingApproval { review_decision: String },
    AwaitingDeployment,
    AwaitingHumanMerge { merge_state: String },
}

impl Item {
    /// One human line: what is wrong and what closes it.
    pub fn describe(&self) -> String {
        match self {
            Self::Conflicting => "merge conflict — resolve it in the files and push".into(),
            Self::MergeabilityUnknown => {
                "mergeability not computed yet (common right after a push) — re-run".into()
            }
            Self::Draft => "PR is a draft — mark it ready for review".into(),
            Self::RequiredNotSatisfied { context, run } => {
                format!("required check `{context}` is {run:?} — fix it or the ruleset")
            }
            Self::CheckFailed { context, producer } => format!(
                "check `{context}` ({producer}) failed — not required, but a red check is \
                 still a finding; fix it here, or file an issue and add a PR-body line \
                 naming `{context}` with its link (#N)"
            ),
            Self::UnresolvedThread(t) => format!(
                "unresolved review thread by {}{}{} — act on it or reply and resolve: {}",
                t.author,
                t.path
                    .as_deref()
                    .map(|p| format!(" on {p}"))
                    .unwrap_or_default(),
                if t.outdated { " (outdated)" } else { "" },
                t.url
            ),
            Self::ChangesRequested { reviewer } => format!(
                "{reviewer} requests changes — act on them, or dismiss the review with a \
                 reason (a finding deferred to an issue is dismissed with its link)"
            ),
            Self::ReviewBotPending { producer, context } => format!(
                "review bot {producer} has not reported (`{context}` pending) — its output \
                 cannot have been read yet; wait for it"
            ),
            Self::AutoMergeNotArmed { merge_state } => format!(
                "automerge is not armed while the PR waits on requirements \
                 (mergeStateStatus={merge_state}) — arm it with squash"
            ),
            Self::WrongMergeMethod { method } => {
                format!("automerge is armed with {method} — re-arm with SQUASH")
            }
            Self::BranchBehind => "branch is behind its base — update it".into(),
            Self::MergeQueue => "base branch uses a merge queue — verify-satisfied cannot \
                 evaluate queue entry, so it refuses rather than pass vacuously"
                .into(),
            Self::AwaitingApproval { review_decision } => {
                format!("awaiting a human review (reviewDecision={review_decision})")
            }
            Self::AwaitingDeployment => "awaiting a required deployment".into(),
            Self::AwaitingHumanMerge { merge_state } => format!(
                "nothing is pending, so GitHub will not arm automerge \
                 (mergeStateStatus={merge_state}) — awaiting a human merge"
            ),
        }
    }
}

/// The body line acknowledging a red non-required check, if any.
///
/// A check run has no dismiss action, so the acknowledgement lives in the PR
/// body: a line naming the context **and** an issue or PR reference (`#N`, or an
/// `/issues/N` or `/pull/N` URL). That is the owner's 2026-09-15 ruling in
/// machine-readable form: a new finding becomes an issue, not a blocker, but it
/// must be looked at. Naming the check without a link does not count.
pub fn acknowledged_in<'a>(body: &'a str, context: &str) -> Option<&'a str> {
    body.lines()
        .map(str::trim)
        .find(|l| l.contains(context) && has_issue_ref(l))
}

fn has_issue_ref(line: &str) -> bool {
    let digit_after = |pat: &str| {
        line.match_indices(pat).any(|(i, _)| {
            line[i + pat.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        })
    };
    digit_after("#") || digit_after("/issues/") || digit_after("/pull/")
}

/// The answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub agent_items: Vec<Item>,
    pub held_by_human: Vec<Item>,
    /// Required contexts satisfied by a skip or neutral: met, but no evidence.
    pub evidence_free: Vec<String>,
    /// What was not evaluated, or looks wrong without being a fail.
    pub notes: Vec<String>,
}

impl Verdict {
    /// Done ⇔ nothing is left for the agent. Human-held items do not count.
    pub fn is_done(&self) -> bool {
        self.agent_items.is_empty()
    }
}

fn strip_bot(login: &str) -> &str {
    login.strip_suffix("[bot]").unwrap_or(login)
}

/// The required half as a [`Gate`], matched on context name (a required check
/// names a context, not a producer). Same-named runs resolve by
/// [`CheckRun::for_context`], the rule `squabble fetch` uses too.
pub fn required_gate(facts: &PrFacts) -> Gate {
    Gate::new(
        facts
            .required_contexts
            .iter()
            .map(|c| {
                let run = CheckRun::for_context(
                    facts
                        .observed
                        .iter()
                        .filter(|o| &o.name == c)
                        .map(|o| o.run),
                );
                RequiredCheck::new(c.clone(), run)
            })
            .collect(),
    )
}

/// Evaluate the definition of done.
pub fn evaluate(facts: &PrFacts, review_apps: &[&str]) -> Verdict {
    let mut agent = Vec::new();
    let mut human = Vec::new();
    let mut notes = Vec::new();

    let gate = required_gate(facts);
    let evidence_free = gate
        .evidence_free()
        .map(|c| c.required_context.clone())
        .collect();

    match facts.state {
        PrState::Closed => {
            notes.push("PR is closed without merging — nothing further to land".into());
            return Verdict {
                agent_items: agent,
                held_by_human: human,
                evidence_free,
                notes,
            };
        }
        PrState::Merged => {
            // Threads and requested changes left behind on a merged PR were never
            // acted on; they are still the agent's to answer. Checks and
            // automerge no longer mean anything.
            agent.extend(
                facts
                    .unresolved_threads
                    .iter()
                    .cloned()
                    .map(Item::UnresolvedThread),
            );
            return Verdict {
                agent_items: agent,
                held_by_human: human,
                evidence_free,
                notes,
            };
        }
        PrState::Open => {}
    }

    // 1. conflicts, resolved in-file
    match facts.mergeable {
        Mergeability::Conflicting => agent.push(Item::Conflicting),
        Mergeability::Unknown => agent.push(Item::MergeabilityUnknown),
        Mergeability::Mergeable => {}
    }
    if facts.is_draft {
        agent.push(Item::Draft);
    }
    if facts.merge_state == "BEHIND" {
        agent.push(Item::BranchBehind);
    }

    // 2. no required check failed or missing; pending is GitHub's to wait on
    let mut required_pending = false;
    for c in &gate.checks {
        match c.run {
            CheckRun::Passed | CheckRun::Skipped => {}
            CheckRun::Pending => required_pending = true,
            CheckRun::Missing | CheckRun::Failed => agent.push(Item::RequiredNotSatisfied {
                context: c.required_context.clone(),
                run: c.run,
            }),
        }
    }
    if facts.required_contexts.is_empty() {
        notes.push(
            "no required status checks on the base branch — automerge would merge on \
             arming, before any review bot reports"
                .into(),
        );
    }

    // 2b. a red check the ruleset does not require is still a finding: "all the
    // checkers have run" means their output was read, not that the ruleset is
    // satisfied. Required contexts are already named above. Acknowledged in the
    // body with an issue link, it is dealt with (the 09-15 ruling) and named in
    // the notes rather than dropped.
    for o in &facts.observed {
        if o.run != CheckRun::Failed || facts.required_contexts.contains(&o.name) {
            continue;
        }
        match acknowledged_in(&facts.body, &o.name) {
            Some(line) => notes.push(format!(
                "red check `{}` acknowledged in the PR body: {line}",
                o.name
            )),
            None => agent.push(Item::CheckFailed {
                context: o.name.clone(),
                producer: strip_bot(&o.producer).to_string(),
            }),
        }
    }

    // 3. review output read and acted on
    agent.extend(
        facts
            .unresolved_threads
            .iter()
            .cloned()
            .map(Item::UnresolvedThread),
    );
    agent.extend(
        facts
            .changes_requested_by
            .iter()
            .map(|r| Item::ChangesRequested {
                reviewer: strip_bot(r).to_string(),
            }),
    );
    for o in &facts.observed {
        let producer = strip_bot(&o.producer);
        if o.run == CheckRun::Pending && review_apps.contains(&producer) {
            agent.push(Item::ReviewBotPending {
                producer: producer.to_string(),
                context: o.name.clone(),
            });
        }
    }

    // 4. new code-scanning alerts — not yet implemented, and not passed silently
    notes.push(
        "code-scanning alerts introduced by this PR are NOT yet evaluated \
         (PR-minus-base set difference is follow-up work)"
            .into(),
    );

    // 7. the rule types, not just the required checks
    let queue = facts.rule_types.iter().any(|t| t == "merge_queue");
    if queue {
        agent.push(Item::MergeQueue);
    }
    if facts.rule_types.iter().any(|t| t == "required_deployments") {
        human.push(Item::AwaitingDeployment);
    }
    let unaccounted: Vec<&str> = facts
        .rule_types
        .iter()
        .map(String::as_str)
        .filter(|t| *t != "merge_queue" && !ACCOUNTED_RULE_TYPES.contains(t))
        .collect();
    if !unaccounted.is_empty() {
        notes.push(format!(
            "rule types not evaluated here: {}",
            unaccounted.join(", ")
        ));
    }
    // A change request that only review bots made is already agent work above;
    // calling it human-held too would tell the agent to wait on itself.
    let human_requested_changes = facts
        .changes_requested_by
        .iter()
        .any(|r| !review_apps.contains(&strip_bot(r)));
    let awaiting_human = match facts.review_decision.as_deref() {
        Some("REVIEW_REQUIRED") => true,
        Some("CHANGES_REQUESTED") => {
            human_requested_changes || facts.changes_requested_by.is_empty()
        }
        _ => false,
    };
    if awaiting_human {
        human.push(Item::AwaitingApproval {
            review_decision: facts.review_decision.clone().unwrap_or_default(),
        });
    }

    // 5 + 6. automerge armed where GitHub allows it, and with squash
    if !queue {
        match facts.auto_merge.as_deref() {
            Some("SQUASH") => {}
            Some(m) => agent.push(Item::WrongMergeMethod {
                method: m.to_string(),
            }),
            None if facts.merge_state == "BLOCKED" || required_pending => {
                agent.push(Item::AutoMergeNotArmed {
                    merge_state: facts.merge_state.clone(),
                })
            }
            None => human.push(Item::AwaitingHumanMerge {
                merge_state: facts.merge_state.clone(),
            }),
        }
    }

    // Safety net: everything evaluated is met, yet GitHub still says BLOCKED.
    // Not a fail (BLOCKED can be a false block on a squash repo), but named.
    if agent.is_empty()
        && facts.auto_merge.is_some()
        && !required_pending
        && human.is_empty()
        && facts.merge_state == "BLOCKED"
    {
        notes.push(format!(
            "every evaluated condition is met and automerge is armed, yet GitHub reports \
             BLOCKED — something unevaluated holds it (rule types: {})",
            facts.rule_types.join(", ")
        ));
    }

    Verdict {
        agent_items: agent,
        held_by_human: human,
        evidence_free,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(name: &str, producer: &str, run: CheckRun) -> Observed {
        Observed {
            name: name.into(),
            producer: producer.into(),
            run,
        }
    }

    /// An open, mergeable PR with one required check passed and squash armed.
    fn done_pr() -> PrFacts {
        PrFacts {
            state: PrState::Open,
            is_draft: false,
            mergeable: Mergeability::Mergeable,
            merge_state: "BLOCKED".into(),
            review_decision: None,
            auto_merge: Some("SQUASH".into()),
            required_contexts: vec!["build".into()],
            observed: vec![
                obs("build", "github-actions", CheckRun::Pending),
                obs("CodeRabbit", "coderabbitai[bot]", CheckRun::Passed),
            ],
            unresolved_threads: vec![],
            changes_requested_by: vec![],
            rule_types: vec!["required_status_checks".into(), "deletion".into()],
            body: String::new(),
        }
    }

    fn kinds(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|i| {
                serde_json::to_value(i).unwrap()["kind"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn armed_with_a_required_check_pending_is_done() {
        let v = evaluate(&done_pr(), DEFAULT_REVIEW_APPS);
        assert!(v.is_done(), "{:?}", v.agent_items);
    }

    #[test]
    fn a_pending_review_bot_is_agent_work() {
        let mut f = done_pr();
        f.observed[1].run = CheckRun::Pending;
        let v = evaluate(&f, DEFAULT_REVIEW_APPS);
        assert_eq!(kinds(&v.agent_items), ["review_bot_pending"]);
    }

    #[test]
    fn a_pending_non_review_check_is_not_agent_work() {
        let mut f = done_pr();
        f.observed
            .push(obs("lint", "github-actions", CheckRun::Pending));
        assert!(evaluate(&f, DEFAULT_REVIEW_APPS).is_done());
    }

    #[test]
    fn an_unresolved_thread_blocks_even_when_outdated() {
        let mut f = done_pr();
        f.unresolved_threads.push(Thread {
            author: "coderabbitai".into(),
            path: Some("src/x.rs".into()),
            line: None,
            outdated: true,
            url: "https://example/t".into(),
        });
        assert_eq!(
            kinds(&evaluate(&f, DEFAULT_REVIEW_APPS).agent_items),
            ["unresolved_thread"]
        );
    }

    #[test]
    fn a_failed_or_missing_required_check_is_agent_work_but_skipped_is_not() {
        let mut f = done_pr();
        f.required_contexts = vec!["build".into(), "scan".into(), "gone".into()];
        f.observed[0].run = CheckRun::Failed;
        f.observed
            .push(obs("scan", "github-actions", CheckRun::Skipped));
        let v = evaluate(&f, DEFAULT_REVIEW_APPS);
        assert_eq!(
            kinds(&v.agent_items),
            ["required_not_satisfied", "required_not_satisfied"]
        );
        assert_eq!(v.evidence_free, ["scan"]);
    }

    /// Measured on cicd-squabbler#114: `rust-ci / Cargo check + clippy + fmt`
    /// was red and not required, and the verdict read DONE. A red checker's
    /// output has not been dealt with just because the ruleset ignores it.
    #[test]
    fn a_failed_check_that_is_not_required_is_still_agent_work() {
        let mut f = done_pr();
        f.observed
            .push(obs("rust-ci / clippy", "github-actions", CheckRun::Failed));
        f.observed
            .push(obs("optional-scan", "github-actions", CheckRun::Skipped));
        let v = evaluate(&f, DEFAULT_REVIEW_APPS);
        assert_eq!(
            v.agent_items,
            [Item::CheckFailed {
                context: "rust-ci / clippy".into(),
                producer: "github-actions".into(),
            }]
        );
    }

    /// The 09-15 ruling: a finding becomes an issue, not a blocker — but only
    /// once someone has looked. Naming the check without a link is not looking.
    #[test]
    fn a_red_check_named_with_an_issue_link_in_the_body_is_dealt_with() {
        let red = || {
            let mut f = done_pr();
            f.observed
                .push(obs("rust-ci / clippy", "github-actions", CheckRun::Failed));
            f
        };
        for body in [
            "- `rust-ci / clippy` — inherited from main, cured by #116",
            "rust-ci / clippy: https://github.com/o/r/issues/7",
            "rust-ci / clippy fails upstream, see https://github.com/o/r/pull/116",
        ] {
            let mut f = red();
            f.body = format!("Summary\n\n{body}\n");
            let v = evaluate(&f, DEFAULT_REVIEW_APPS);
            assert!(v.is_done(), "{body:?}: {:?}", v.agent_items);
            assert!(
                v.notes.iter().any(|n| n.contains("rust-ci / clippy")),
                "an acknowledged red must stay visible: {:?}",
                v.notes
            );
        }
        for body in [
            "",
            "rust-ci / clippy is inherited from main",
            "rust-ci / clippy is # not a link",
            "see #116\nrust-ci / clippy is red",
            "`rust-ci / fmt` — see #116",
        ] {
            let mut f = red();
            f.body = body.into();
            assert_eq!(
                kinds(&evaluate(&f, DEFAULT_REVIEW_APPS).agent_items),
                ["check_failed"],
                "{body:?}"
            );
        }
    }

    /// Same-named runs from two workflows: the verdict must not depend on the
    /// order the rollup happens to list them in.
    #[test]
    fn a_passing_twin_cannot_mask_a_failing_required_check() {
        for failed_first in [true, false] {
            let mut f = done_pr();
            f.observed[0].run = CheckRun::Passed;
            let red = obs("build", "github-actions", CheckRun::Failed);
            if failed_first {
                f.observed.insert(0, red);
            } else {
                f.observed.push(red);
            }
            assert_eq!(
                kinds(&evaluate(&f, DEFAULT_REVIEW_APPS).agent_items),
                ["required_not_satisfied"],
                "failed_first={failed_first}"
            );
        }
    }

    #[test]
    fn blocked_without_automerge_is_agent_work() {
        let mut f = done_pr();
        f.auto_merge = None;
        assert_eq!(
            kinds(&evaluate(&f, DEFAULT_REVIEW_APPS).agent_items),
            ["auto_merge_not_armed"]
        );
    }

    /// GitHub refuses to arm automerge on a clean PR, so demanding it there
    /// would make the gate unsatisfiable. The owner merges.
    #[test]
    fn clean_without_automerge_is_held_by_human_not_agent_work() {
        let mut f = done_pr();
        f.auto_merge = None;
        f.merge_state = "CLEAN".into();
        f.observed[0].run = CheckRun::Passed;
        let v = evaluate(&f, DEFAULT_REVIEW_APPS);
        assert!(v.is_done(), "{:?}", v.agent_items);
        assert_eq!(kinds(&v.held_by_human), ["awaiting_human_merge"]);
    }

    #[test]
    fn a_non_squash_automerge_is_agent_work() {
        let mut f = done_pr();
        f.auto_merge = Some("REBASE".into());
        assert_eq!(
            kinds(&evaluate(&f, DEFAULT_REVIEW_APPS).agent_items),
            ["wrong_merge_method"]
        );
    }

    #[test]
    fn unknown_mergeability_fails_closed_but_is_not_called_a_conflict() {
        let mut f = done_pr();
        f.mergeable = Mergeability::Unknown;
        assert_eq!(
            kinds(&evaluate(&f, DEFAULT_REVIEW_APPS).agent_items),
            ["mergeability_unknown"]
        );
    }

    #[test]
    fn changes_requested_is_agent_work_and_a_required_approval_is_human() {
        let mut f = done_pr();
        f.changes_requested_by = vec!["coderabbitai[bot]".into()];
        f.review_decision = Some("REVIEW_REQUIRED".into());
        let v = evaluate(&f, DEFAULT_REVIEW_APPS);
        assert_eq!(kinds(&v.agent_items), ["changes_requested"]);
        assert_eq!(kinds(&v.held_by_human), ["awaiting_approval"]);
    }

    /// Measured on hypatia#883: `reviewDecision=CHANGES_REQUESTED` there comes
    /// from CodeRabbit alone, so it is agent work, not a wait on a human.
    #[test]
    fn a_change_request_only_a_bot_made_is_not_human_held() {
        let mut f = done_pr();
        f.changes_requested_by = vec!["coderabbitai".into()];
        f.review_decision = Some("CHANGES_REQUESTED".into());
        let v = evaluate(&f, DEFAULT_REVIEW_APPS);
        assert_eq!(kinds(&v.agent_items), ["changes_requested"]);
        assert!(!kinds(&v.held_by_human).contains(&"awaiting_approval".to_string()));

        f.changes_requested_by.push("a-maintainer".into());
        let v = evaluate(&f, DEFAULT_REVIEW_APPS);
        assert!(kinds(&v.held_by_human).contains(&"awaiting_approval".to_string()));
    }

    /// With a merge queue, `autoMergeRequest` is the wrong field: a pass would
    /// be vacuous, so the gate refuses.
    #[test]
    fn a_merge_queue_is_refused_not_passed() {
        let mut f = done_pr();
        f.rule_types.push("merge_queue".into());
        assert_eq!(
            kinds(&evaluate(&f, DEFAULT_REVIEW_APPS).agent_items),
            ["merge_queue"]
        );
    }

    #[test]
    fn an_unknown_rule_type_is_named_in_the_notes() {
        let mut f = done_pr();
        f.rule_types.push("file_path_restriction".into());
        let v = evaluate(&f, DEFAULT_REVIEW_APPS);
        assert!(v.is_done());
        assert!(v.notes.iter().any(|n| n.contains("file_path_restriction")));
    }

    #[test]
    fn code_scanning_is_never_passed_silently() {
        let v = evaluate(&done_pr(), DEFAULT_REVIEW_APPS);
        assert!(v.notes.iter().any(|n| n.contains("NOT yet evaluated")));
    }

    #[test]
    fn a_merged_pr_still_owes_its_unresolved_threads() {
        let mut f = done_pr();
        f.state = PrState::Merged;
        f.auto_merge = None;
        f.unresolved_threads.push(Thread {
            author: "owner".into(),
            path: None,
            line: None,
            outdated: false,
            url: "u".into(),
        });
        assert_eq!(
            kinds(&evaluate(&f, DEFAULT_REVIEW_APPS).agent_items),
            ["unresolved_thread"]
        );
    }
}
