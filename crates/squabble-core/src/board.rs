// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `board` — the estate "needs me" board, pure.
//!
//! Every open pull request across the owners lands in exactly one [`Bucket`]:
//!
//! * [`Bucket::NeedsYou`] — only a human can move it (approve, merge a `CLEAN`
//!   PR GitHub refuses to arm, add a gate to a repo that has none).
//! * [`Bucket::AgentWork`] — an agent can move it (conflict, red check, review
//!   to answer, branch behind, automerge not yet armed).
//! * [`Bucket::Landing`] — automerge is armed and nothing is in its way; it
//!   lands by itself and the board shows it only as a count.
//! * [`Bucket::Stale`] — untouched for longer than the stale threshold *and*
//!   red or conflicting: the owner decides whether to close it.
//!
//! The classifier reads facts; it never fetches and never decides to merge.
//! "No required gate" is a [`Bucket::NeedsYou`] reason, never a licence to
//! arm: arming on a repo with no required contexts merges at once (AGENTS.md
//! §5c), so such a PR waits for the repo's ruleset layer instead.

use crate::chains::RepoId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Merge-relevant facts about one repository's default branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoGate {
    pub repo: RepoId,
    /// Required status-check contexts in force on the default branch, summed
    /// over every ruleset (and classic protection) that applies to it.
    pub required_contexts: usize,
    /// Highest `required_approving_review_count` among the rules that apply.
    pub required_approvals: u32,
}

/// GitHub's `MergeableState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mergeable {
    Mergeable,
    Conflicting,
    Unknown,
}

/// Merge-relevant facts about one open pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrFacts {
    pub repo: RepoId,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub head_ref: String,
    pub is_draft: bool,
    pub mergeable: Mergeable,
    /// GitHub's `MergeStateStatus` spelled as the API spells it (`CLEAN`,
    /// `BLOCKED`, `BEHIND`, `DIRTY`, `UNSTABLE`, `HAS_HOOKS`, `DRAFT`,
    /// `UNKNOWN`).
    pub merge_state: String,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, or `None`.
    pub review_decision: Option<String>,
    pub auto_merge_armed: bool,
    /// Head commit `statusCheckRollup.state` (`SUCCESS`, `FAILURE`, `ERROR`,
    /// `PENDING`, `EXPECTED`), or `None` when the head has no checks at all.
    pub rollup: Option<String>,
    /// `updatedAt`, RFC 3339.
    pub updated_at: String,
}

/// Which list a PR belongs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Bucket {
    NeedsYou,
    AgentWork,
    Landing,
    Stale,
}

impl Bucket {
    /// Section heading on the rendered board.
    pub fn heading(self) -> &'static str {
        match self {
            Bucket::NeedsYou => "Needs you",
            Bucket::AgentWork => "Agent work",
            Bucket::Landing => "Landing by itself",
            Bucket::Stale => "Stale — close?",
        }
    }
}

/// Why a PR is in its bucket. One reason per PR: the first that applies, in
/// the order [`classify`] tests them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Reason {
    // NeedsYou
    NoRequiredGate,
    ApprovalRequired,
    CleanMergeByHand,
    // AgentWork
    Draft,
    Conflict,
    ChangesRequested,
    ChecksRed,
    Behind,
    ArmAutomerge,
    MergeabilityUnknown,
    // Landing
    ArmedWaiting,
    // Stale
    StaleRedOrConflicting,
}

impl Reason {
    /// The bucket a reason belongs to.
    pub fn bucket(self) -> Bucket {
        use Reason::*;
        match self {
            NoRequiredGate | ApprovalRequired | CleanMergeByHand => Bucket::NeedsYou,
            Draft | Conflict | ChangesRequested | ChecksRed | Behind | ArmAutomerge
            | MergeabilityUnknown => Bucket::AgentWork,
            ArmedWaiting => Bucket::Landing,
            StaleRedOrConflicting => Bucket::Stale,
        }
    }

    /// One-line explanation shown on the board.
    pub fn label(self) -> &'static str {
        use Reason::*;
        match self {
            NoRequiredGate => "no required check on the default branch — add a ruleset layer (arming here would merge at once)",
            ApprovalRequired => "a required approving review is missing",
            CleanMergeByHand => "CLEAN: every requirement already passed, GitHub refuses to arm — merge it",
            Draft => "draft",
            Conflict => "merge conflict",
            ChangesRequested => "changes requested by a reviewer",
            ChecksRed => "a check on the head is red",
            Behind => "branch is behind its base",
            ArmAutomerge => "ready for automerge to be armed",
            MergeabilityUnknown => "GitHub has not computed mergeability yet",
            ArmedWaiting => "automerge armed, waiting on checks",
            StaleRedOrConflicting => "untouched past the stale threshold and red or conflicting",
        }
    }
}

/// Days since 1970-01-01 for the `YYYY-MM-DD` prefix of an RFC 3339 stamp,
/// or `None` when the prefix is not a date.
pub fn epoch_day(stamp: &str) -> Option<i64> {
    let y: i64 = stamp.get(0..4)?.parse().ok()?;
    let m: i64 = stamp.get(5..7)?.parse().ok()?;
    let d: i64 = stamp.get(8..10)?.parse().ok()?;
    if stamp.get(4..5) != Some("-") || stamp.get(7..8) != Some("-") {
        return None;
    }
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// RFC 3339 UTC stamp (`YYYY-MM-DDTHH:MM:SSZ`) for seconds since the epoch —
/// the inverse of [`epoch_day`] at day granularity, so no date crate is needed.
pub fn rfc3339_from_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Place one PR. `today` is [`epoch_day`] of the run; `stale_days` is the
/// threshold past which a red or conflicting PR is offered for closing.
///
/// A missing gate outranks everything except staleness: whatever else is
/// wrong with such a PR, the repo has to grow a gate before automation can
/// land anything there.
pub fn classify(pr: &PrFacts, gate: &RepoGate, today: i64, stale_days: i64) -> Reason {
    let red = matches!(pr.rollup.as_deref(), Some("FAILURE" | "ERROR"));
    let conflicting = pr.mergeable == Mergeable::Conflicting;
    let age = epoch_day(&pr.updated_at).map(|d| today - d);
    if (red || conflicting) && age.is_some_and(|a| a > stale_days) {
        return Reason::StaleRedOrConflicting;
    }
    if gate.required_contexts == 0 {
        return Reason::NoRequiredGate;
    }
    if pr.is_draft {
        return Reason::Draft;
    }
    if conflicting {
        return Reason::Conflict;
    }
    match pr.review_decision.as_deref() {
        Some("CHANGES_REQUESTED") => return Reason::ChangesRequested,
        Some("REVIEW_REQUIRED") if gate.required_approvals > 0 => return Reason::ApprovalRequired,
        _ => {}
    }
    if red {
        return Reason::ChecksRed;
    }
    if pr.merge_state == "BEHIND" {
        return Reason::Behind;
    }
    if pr.auto_merge_armed {
        return Reason::ArmedWaiting;
    }
    match (pr.mergeable, pr.merge_state.as_str()) {
        (_, "CLEAN") => Reason::CleanMergeByHand,
        (Mergeable::Unknown, _) | (_, "UNKNOWN") => Reason::MergeabilityUnknown,
        _ => Reason::ArmAutomerge,
    }
}

/// One run of the board: what was read, what could not be, and every PR placed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Board {
    /// RFC 3339 time of the run.
    pub generated_at: String,
    pub owners: Vec<String>,
    /// Repositories enumerated (non-archived), per owner.
    pub repos_enumerated: BTreeMap<String, usize>,
    /// Open PRs the enumeration reported, per owner — the denominator.
    pub open_prs_reported: BTreeMap<String, usize>,
    /// Repositories that could not be read, with the reason. Never dropped.
    pub unavailable: Vec<(RepoId, String)>,
    /// Repositories whose open PRs exceeded one page; the rest were not read.
    pub truncated: Vec<RepoId>,
    pub placed: Vec<(PrFacts, Reason)>,
}

impl Board {
    /// PRs actually placed, per owner — compared against
    /// [`Board::open_prs_reported`] so a shortfall is visible, never silent.
    pub fn placed_per_owner(&self) -> BTreeMap<String, usize> {
        let mut m = BTreeMap::new();
        for (pr, _) in &self.placed {
            let owner = pr.repo.as_str().split('/').next().unwrap_or("").to_string();
            *m.entry(owner).or_insert(0) += 1;
        }
        m
    }
}

/// GitHub rejects an issue body over 65 536 characters.
pub const ISSUE_BODY_LIMIT: usize = 65_536;

/// Render the board as GitHub-flavoured Markdown no longer than `max_bytes`.
///
/// `Needs you` is listed in full first; `Agent work` and `Stale` are grouped
/// by reason inside collapsed sections; `Landing` is a count. When the text
/// would exceed `max_bytes`, the longest lists are cut and each cut says how
/// many rows it hid — the totals in the header are always complete.
pub fn render_markdown(board: &Board, max_bytes: usize) -> String {
    let mut per_page = 400usize;
    loop {
        let text = render_with_cap(board, per_page);
        if text.len() <= max_bytes || per_page == 0 {
            return text;
        }
        per_page /= 2;
    }
}

/// Render the whole board with each detail list cut at `cap` lines.
fn render_with_cap(board: &Board, cap: usize) -> String {
    let mut by_reason: BTreeMap<Reason, Vec<&PrFacts>> = BTreeMap::new();
    for (pr, r) in &board.placed {
        by_reason.entry(*r).or_default().push(pr);
    }
    let count = |b: Bucket| -> usize {
        by_reason
            .iter()
            .filter(|(r, _)| r.bucket() == b)
            .map(|(_, v)| v.len())
            .sum()
    };

    let mut s = String::new();
    s.push_str("# Estate: needs me\n\n");
    s.push_str(&format!(
        "_Generated {} by `squabble board` for {}. This body is rewritten on every run; it never comments, so it never notifies._\n\n",
        board.generated_at,
        board.owners.join(", ")
    ));
    s.push_str("| | count |\n|---|---:|\n");
    for b in [
        Bucket::NeedsYou,
        Bucket::AgentWork,
        Bucket::Landing,
        Bucket::Stale,
    ] {
        s.push_str(&format!("| **{}** | {} |\n", b.heading(), count(b)));
    }
    s.push('\n');

    let placed = board.placed_per_owner();
    s.push_str("**Coverage** — ");
    let cov: Vec<String> = board
        .owners
        .iter()
        .map(|o| {
            format!(
                "{o}: {} repos, {} open PRs reported, {} placed",
                board.repos_enumerated.get(o).copied().unwrap_or(0),
                board.open_prs_reported.get(o).copied().unwrap_or(0),
                placed.get(o).copied().unwrap_or(0)
            )
        })
        .collect();
    s.push_str(&cov.join(" · "));
    s.push('\n');
    if !board.unavailable.is_empty() || !board.truncated.is_empty() {
        s.push_str(&format!(
            "\n> ⚠ **Incomplete:** {} repo(s) unreadable, {} repo(s) with more open PRs than one page. Their PRs are missing from the lists below.\n",
            board.unavailable.len(),
            board.truncated.len()
        ));
        for (r, why) in &board.unavailable {
            s.push_str(&format!("> - `{}` — {}\n", r.as_str(), why));
        }
        for r in &board.truncated {
            s.push_str(&format!("> - `{}` — truncated at one page\n", r.as_str()));
        }
    }

    // Needs you: no-gate PRs collapse to one line per repo (the fix is per
    // repo); the rest are listed one per PR.
    s.push_str(&format!("\n## {}\n", Bucket::NeedsYou.heading()));
    let mut any = false;
    for (r, prs) in by_reason
        .iter()
        .filter(|(r, _)| r.bucket() == Bucket::NeedsYou)
    {
        any = true;
        s.push_str(&format!("\n### {} ({})\n", r.label(), prs.len()));
        if *r == Reason::NoRequiredGate {
            let mut repos: BTreeMap<&str, usize> = BTreeMap::new();
            for p in prs {
                *repos.entry(p.repo.as_str()).or_insert(0) += 1;
            }
            push_capped(&mut s, repos.iter(), cap, |(repo, n)| {
                format!("- `{repo}` — {n} open PR(s)")
            });
        } else {
            push_capped(&mut s, prs.iter(), cap, |p| pr_line(p));
        }
    }
    if !any {
        s.push_str("\nNothing. 🎉\n");
    }

    for b in [Bucket::AgentWork, Bucket::Stale] {
        s.push_str(&format!("\n## {} ({})\n", b.heading(), count(b)));
        for (r, prs) in by_reason.iter().filter(|(r, _)| r.bucket() == b) {
            s.push_str(&format!(
                "\n<details><summary>{} — {}</summary>\n\n",
                r.label(),
                prs.len()
            ));
            push_capped(&mut s, prs.iter(), cap, |p| pr_line(p));
            s.push_str("\n</details>\n");
        }
    }

    s.push_str(&format!(
        "\n## {} ({})\n\nArmed and unobstructed; these need nothing from anyone.\n",
        Bucket::Landing.heading(),
        count(Bucket::Landing)
    ));
    s
}

/// One Markdown list line for a pull request.
fn pr_line(p: &PrFacts) -> String {
    let title: String = p.title.replace('|', "\\|").chars().take(90).collect();
    format!(
        "- [{}#{}]({}) {} — @{}",
        p.repo.as_str(),
        p.number,
        p.url,
        title,
        p.author
    )
}

/// Append up to `cap` lines, then an "…and N more" line for the rest.
fn push_capped<T>(
    s: &mut String,
    items: impl ExactSizeIterator<Item = T>,
    cap: usize,
    line: impl Fn(T) -> String,
) {
    let total = items.len();
    for item in items.take(cap) {
        s.push_str(&line(item));
        s.push('\n');
    }
    if total > cap {
        s.push_str(&format!(
            "- …and {} more (run `squabble board` locally for the full list)\n",
            total - cap
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(ctx: usize, approvals: u32) -> RepoGate {
        RepoGate {
            repo: RepoId::new("o/r"),
            required_contexts: ctx,
            required_approvals: approvals,
        }
    }

    fn pr() -> PrFacts {
        PrFacts {
            repo: RepoId::new("o/r"),
            number: 1,
            title: "t".into(),
            url: "https://github.com/o/r/pull/1".into(),
            author: "a".into(),
            head_ref: "h".into(),
            is_draft: false,
            mergeable: Mergeable::Mergeable,
            merge_state: "BLOCKED".into(),
            review_decision: None,
            auto_merge_armed: false,
            rollup: Some("PENDING".into()),
            updated_at: "2026-10-01T10:00:00Z".into(),
        }
    }

    const TODAY: i64 = 20_727; // 2026-10-01

    #[test]
    fn epoch_day_matches_known_dates() {
        assert_eq!(epoch_day("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_day("2000-03-01"), Some(11_017));
        assert_eq!(epoch_day("2026-10-01T13:00:00Z"), Some(TODAY));
        assert_eq!(epoch_day("not a date"), None);
        assert_eq!(epoch_day("2026-13-01"), None);
    }

    #[test]
    fn rfc3339_round_trips_through_epoch_day() {
        assert_eq!(rfc3339_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_from_unix(951_782_400), "2000-02-29T00:00:00Z");
        for day in [0i64, 11_016, 11_017, TODAY, 40_000] {
            let s = rfc3339_from_unix(day as u64 * 86_400 + 3_723);
            assert_eq!(epoch_day(&s), Some(day), "{s}");
            assert!(s.ends_with("T01:02:03Z"), "{s}");
        }
    }

    #[test]
    fn no_gate_is_never_armable() {
        // The §5c trap: a PR that would otherwise be "arm it" must not be,
        // because there is nothing for automerge to wait on.
        assert_eq!(
            classify(&pr(), &gate(0, 0), TODAY, 30),
            Reason::NoRequiredGate
        );
        let mut clean = pr();
        clean.merge_state = "CLEAN".into();
        assert_eq!(
            classify(&clean, &gate(0, 0), TODAY, 30),
            Reason::NoRequiredGate
        );
    }

    #[test]
    fn gated_unarmed_blocked_pr_is_armable() {
        assert_eq!(
            classify(&pr(), &gate(3, 0), TODAY, 30),
            Reason::ArmAutomerge
        );
    }

    #[test]
    fn armed_and_unobstructed_is_landing() {
        let mut p = pr();
        p.auto_merge_armed = true;
        assert_eq!(classify(&p, &gate(3, 0), TODAY, 30), Reason::ArmedWaiting);
        assert_eq!(Reason::ArmedWaiting.bucket(), Bucket::Landing);
    }

    #[test]
    fn armed_but_red_is_agent_work_not_landing() {
        let mut p = pr();
        p.auto_merge_armed = true;
        p.rollup = Some("FAILURE".into());
        assert_eq!(classify(&p, &gate(3, 0), TODAY, 30), Reason::ChecksRed);
    }

    #[test]
    fn clean_unarmed_needs_a_human_merge() {
        let mut p = pr();
        p.merge_state = "CLEAN".into();
        assert_eq!(
            classify(&p, &gate(3, 0), TODAY, 30),
            Reason::CleanMergeByHand
        );
        assert_eq!(Reason::CleanMergeByHand.bucket(), Bucket::NeedsYou);
    }

    #[test]
    fn approval_only_needs_you_when_the_rules_require_one() {
        let mut p = pr();
        p.review_decision = Some("REVIEW_REQUIRED".into());
        assert_eq!(
            classify(&p, &gate(3, 1), TODAY, 30),
            Reason::ApprovalRequired
        );
        assert_eq!(classify(&p, &gate(3, 0), TODAY, 30), Reason::ArmAutomerge);
    }

    #[test]
    fn conflicts_and_drafts_are_agent_work() {
        let mut p = pr();
        p.mergeable = Mergeable::Conflicting;
        assert_eq!(classify(&p, &gate(3, 0), TODAY, 30), Reason::Conflict);
        let mut d = pr();
        d.is_draft = true;
        assert_eq!(classify(&d, &gate(3, 0), TODAY, 30), Reason::Draft);
    }

    #[test]
    fn old_and_red_is_stale_but_old_and_green_is_not() {
        let mut p = pr();
        p.updated_at = "2026-08-01T00:00:00Z".into();
        p.rollup = Some("FAILURE".into());
        assert_eq!(
            classify(&p, &gate(0, 0), TODAY, 30),
            Reason::StaleRedOrConflicting
        );
        p.rollup = Some("SUCCESS".into());
        assert_eq!(classify(&p, &gate(3, 0), TODAY, 30), Reason::ArmAutomerge);
    }

    #[test]
    fn every_reason_maps_to_the_bucket_its_section_claims() {
        use Reason::*;
        for r in [NoRequiredGate, ApprovalRequired, CleanMergeByHand] {
            assert_eq!(r.bucket(), Bucket::NeedsYou);
        }
        for r in [
            Draft,
            Conflict,
            ChangesRequested,
            ChecksRed,
            Behind,
            ArmAutomerge,
            MergeabilityUnknown,
        ] {
            assert_eq!(r.bucket(), Bucket::AgentWork);
        }
    }

    fn big_board(n: usize) -> Board {
        let mut b = Board {
            generated_at: "2026-10-01T13:00:00Z".into(),
            owners: vec!["o".into()],
            ..Board::default()
        };
        b.repos_enumerated.insert("o".into(), 1);
        b.open_prs_reported.insert("o".into(), n);
        for i in 0..n {
            let mut p = pr();
            p.number = i as u64;
            p.title = "x".repeat(80);
            b.placed.push((p, Reason::Conflict));
        }
        b
    }

    #[test]
    fn render_respects_the_issue_body_limit_and_says_what_it_hid() {
        let b = big_board(5_000);
        let md = render_markdown(&b, ISSUE_BODY_LIMIT);
        assert!(md.len() <= ISSUE_BODY_LIMIT, "{} bytes", md.len());
        assert!(md.contains("more (run `squabble board` locally"));
        // The header totals are never cut.
        assert!(md.contains("| **Agent work** | 5000 |"));
        assert!(md.contains("5000 open PRs reported, 5000 placed"));
    }

    #[test]
    fn render_lists_everything_when_it_fits() {
        let b = big_board(3);
        let md = render_markdown(&b, ISSUE_BODY_LIMIT);
        assert!(!md.contains("more (run"));
        assert_eq!(md.matches("](https://github.com/o/r/pull/1)").count(), 3);
    }

    #[test]
    fn unreadable_repos_are_announced_not_dropped() {
        let mut b = big_board(1);
        b.unavailable
            .push((RepoId::new("o/gone"), "rate limited".into()));
        let md = render_markdown(&b, ISSUE_BODY_LIMIT);
        assert!(md.contains("Incomplete"));
        assert!(md.contains("`o/gone` — rate limited"));
    }
}
