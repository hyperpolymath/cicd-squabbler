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
    // GitHub's rollup is a union: commit statuses use context/state, while
    // check runs use name/conclusion. CodeRabbit commonly supplies a status.
    #[serde(alias = "context")]
    name: String,
    status: Option<String>,
    #[serde(alias = "state")]
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
    let path = url
        .strip_prefix("https://github.com/")?
        .split(['?', '#'])
        .next()?;
    let mut segments = path.split('/');
    let (owner, repo, actions, runs, run_id, job, job_id) = (
        segments.next()?,
        segments.next()?,
        segments.next()?,
        segments.next()?,
        segments.next()?,
        segments.next()?,
        segments.next()?,
    );

    if owner.is_empty()
        || repo.is_empty()
        || actions != "actions"
        || runs != "runs"
        || run_id.parse::<u64>().is_err()
        || job != "job"
        || !matches!(segments.next(), None | Some(""))
        || segments.next().is_some()
    {
        return None;
    }

    job_id.parse().ok()
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

// ---------------------------------------------------------------------------
// Classic branch protection (issue #100)
//
// `rules/branches/{branch}` returns RULESET rules only. A branch protected
// the classic way — `branches/{branch}/protection`, with
// `required_status_checks.contexts` — is genuinely gated, and without this
// fallback squabble reported "no gate" for it. The two mechanisms are
// cumulative (GitHub enforces BOTH when both exist), so the required-context
// set is the union, deduplicated, ruleset first.
// ---------------------------------------------------------------------------

/// The outcome of probing the classic branch-protection endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProtectionProbe {
    /// 2xx: a protection payload (its `required_status_checks` may still be
    /// absent — protection can cover force-pushes/deletions only).
    Protected(String),
    /// 404: the branch is genuinely not protected the classic way.
    NotProtected,
    /// 403: protection exists-or-not is INVISIBLE to this token. Never to be
    /// read as "no gate" — that would reintroduce, at the API layer, exactly
    /// the vacuous-gate conflation the `NoGate` variant removed.
    Hidden,
}

#[derive(Debug, Deserialize)]
struct ClassicProtection {
    required_status_checks: Option<ClassicRequiredChecks>,
}

#[derive(Debug, Deserialize)]
struct ClassicRequiredChecks {
    /// The long-standing shape: a bare list of context names.
    #[serde(default)]
    contexts: Vec<String>,
    /// The newer shape GitHub added alongside `contexts`: objects carrying
    /// `context` (and `app_id`). Both are read and unioned; either may be
    /// present without the other.
    #[serde(default)]
    checks: Vec<ClassicCheck>,
}

#[derive(Debug, Deserialize)]
struct ClassicCheck {
    context: String,
}

/// Classify a FAILED classic-protection probe call. A 404 means genuinely
/// *not protected*; a 403 means *not visible to this token* — the distinction
/// the defect turns on (issue #100's ⚠ note), so it is pure and directly
/// tested on real `gh api` stderr shapes. Any other failure is an ordinary
/// fetch error (`None` here), never a protection finding.
fn probe_from_stderr(stderr: &str) -> Option<ProtectionProbe> {
    if stderr.contains("HTTP 404") || stderr.contains("Branch not protected") {
        return Some(ProtectionProbe::NotProtected);
    }
    if stderr.contains("HTTP 403") || stderr.contains("Resource not accessible") {
        return Some(ProtectionProbe::Hidden);
    }
    None
}

/// Probe the classic branch-protection endpoint for `slug@branch`.
fn probe_classic_protection(slug: &str, branch: &str) -> Result<ProtectionProbe, String> {
    let out = Command::new("gh")
        .args(["api", &format!("repos/{slug}/branches/{branch}/protection")])
        .output()
        .map_err(|e| format!("failed to run `gh api …/protection`: {e}"))?;
    if out.status.success() {
        return Ok(ProtectionProbe::Protected(
            String::from_utf8_lossy(&out.stdout).into_owned(),
        ));
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    probe_from_stderr(&stderr).ok_or_else(|| {
        format!(
            "`gh api repos/{slug}/branches/{branch}/protection` exited {}: {}",
            out.status,
            stderr.trim()
        )
    })
}

/// The context names a classic-protection payload requires. Both wire shapes
/// (`contexts` and `checks[].context`) are unioned.
fn parse_classic_contexts(json: &str) -> Result<Vec<String>, String> {
    let p: ClassicProtection = serde_json::from_str(json)
        .map_err(|e| format!("could not parse branch-protection response: {e}"))?;
    let mut out: Vec<String> = Vec::new();
    if let Some(rsc) = p.required_status_checks {
        for c in rsc.contexts {
            if !out.contains(&c) {
                out.push(c);
            }
        }
        for c in rsc.checks.into_iter().map(|c| c.context) {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    Ok(out)
}

/// The union of required contexts from the two protection APIs, in
/// enforcement order: ruleset contexts first, then classic-only ones.
/// `Hidden` is a hard error even when the ruleset yielded contexts — a gate
/// that cannot be fully seen must not be reasoned about partially (the
/// undercount would read as satisfied when a hidden classic requirement is
/// unmet: a vacuous green).
fn required_contexts_from_apis(
    rules_json: &str,
    protection: &ProtectionProbe,
) -> Result<Vec<String>, FetchError> {
    let rules: Vec<RulesetRule> = serde_json::from_str(rules_json)
        .map_err(|e| format!("could not parse ruleset response: {e}"))?;
    let mut contexts: Vec<String> = Vec::new();
    for c in rules
        .into_iter()
        .filter(|r| r.rule_type == "required_status_checks")
        .filter_map(|r| r.parameters)
        .flat_map(|p| p.required_status_checks)
        .map(|c| c.context)
    {
        if !contexts.contains(&c) {
            contexts.push(c);
        }
    }
    match protection {
        ProtectionProbe::Protected(json) => {
            for c in parse_classic_contexts(json)? {
                if !contexts.contains(&c) {
                    contexts.push(c);
                }
            }
        }
        ProtectionProbe::NotProtected => {}
        ProtectionProbe::Hidden => {
            return Err(FetchError::Failed(
                "classic branch protection on this branch is not visible to this token \
                 (HTTP 403); refusing to conclude anything about a gate that cannot be \
                 fully read — grant the token Administration read, or use one that has it"
                    .to_string(),
            ));
        }
    }
    Ok(contexts)
}

/// Turn a computed required-context set into the gate half of a fetch: an
/// empty union means neither protection mechanism requires anything — the
/// qualified `NoGate` finding (exit 3), no longer a ruleset-only statement.
fn contexts_or_no_gate(
    contexts: Vec<String>,
    slug: &str,
    branch: &str,
) -> Result<Vec<String>, FetchError> {
    if contexts.is_empty() {
        return Err(FetchError::NoGate {
            slug: slug.to_string(),
            branch: branch.to_string(),
        });
    }
    Ok(contexts)
}

/// The required contexts whose realised run concluded `STARTUP_FAILURE` —
/// the trigger for the Actions-policy why-probe (issue #15). A startup
/// refusal means the repository's own Actions posture, not the workflow's
/// content, is the first thing to check; matching is by exact context name,
/// the same key [`build_gate`] uses.
fn startup_failures_from_rollup(required_contexts: &[String], rollup: &[RollupEntry]) -> Vec<String> {
    required_contexts
        .iter()
        .filter(|req| {
            rollup
                .iter()
                .any(|r| &r.name == *req && r.conclusion.as_deref() == Some("STARTUP_FAILURE"))
        })
        .cloned()
        .collect()
}
// ---------------------------------------------------------------------------
// Actions-policy why-probe (issue #15)
//
// A required-context STARTUP_FAILURE is *created by repository settings*, not
// by the workflow: `allowed_actions=selected` with a `patterns_allowed` that
// does not cover an external `uses:` (mode 1), or a tag-pinned `uses:` under
// `sha_pinning_required=true` (mode 2). The probe reads both permission
// endpoints and hands the posture to the fight as classification input.
// ---------------------------------------------------------------------------

/// The repository's live Actions permissions posture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionsPolicy {
    /// `all` | `local_only` | `selected` (empty when unknown).
    pub allowed_actions: String,
    /// Whether full-length-SHA pinning is enforced for every `uses:`.
    pub sha_pinning_required: bool,
    /// Whether GitHub-owned actions pass under a `selected` posture.
    pub github_owned_allowed: bool,
    /// `patterns_allowed` (meaningful only under `selected`).
    pub patterns_allowed: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PermissionsResponse {
    allowed_actions: Option<String>,
    #[serde(default)]
    sha_pinning_required: bool,
}

#[derive(Debug, Deserialize)]
struct SelectedActionsResponse {
    #[serde(default)]
    github_owned_allowed: bool,
    #[serde(default)]
    patterns_allowed: Vec<String>,
}

/// Combine the two permission endpoints' payloads into an [`ActionsPolicy`].
/// Pure — the unit of test coverage for [`fetch_actions_policy`].
/// `selected_json` is required iff `allowed_actions == "selected"`.
fn parse_policy(perms_json: &str, selected_json: Option<&str>) -> Result<ActionsPolicy, String> {
    let perms: PermissionsResponse = serde_json::from_str(perms_json)
        .map_err(|e| format!("could not parse actions/permissions response: {e}"))?;
    let allowed_actions = perms.allowed_actions.unwrap_or_default();
    let (github_owned_allowed, patterns_allowed) = if allowed_actions == "selected" {
        let sel_json = selected_json
            .ok_or_else(|| "allowed_actions=selected but no selected-actions payload".to_string())?;
        let sel: SelectedActionsResponse = serde_json::from_str(sel_json)
            .map_err(|e| format!("could not parse selected-actions response: {e}"))?;
        (sel.github_owned_allowed, sel.patterns_allowed)
    } else {
        // Under `all` GitHub-owned refs pass (so does everything else); under
        // `local_only` nothing external does. Either way there is no pattern
        // list to reconcile — mode 1 is scoped to `selected` (issue #15).
        (allowed_actions == "all", Vec::new())
    };
    Ok(ActionsPolicy {
        allowed_actions,
        sha_pinning_required: perms.sha_pinning_required,
        github_owned_allowed,
        patterns_allowed,
    })
}

/// Probe the repository's Actions permissions (`actions/permissions`, plus
/// `actions/permissions/selected-actions` when the posture is `selected`).
///
/// `.github/settings.yml` CANNOT set these (probot/settings manages branch
/// protection/labels/merges only) — the REST endpoints are the only read
/// surface, exactly as issue #15's DO-NOT pointer states.
pub fn fetch_actions_policy(slug: &str) -> Result<ActionsPolicy, String> {
    let perms_json = run_gh(&["api", &format!("repos/{slug}/actions/permissions")])?;
    // Only fetch the pattern list when the posture actually uses it — one
    // Administration-scoped call fewer for the common `all` posture.
    let needs_selected = serde_json::from_str::<PermissionsResponse>(&perms_json)
        .map_err(|e| format!("could not parse actions/permissions response: {e}"))?
        .allowed_actions
        .as_deref()
        == Some("selected");
    let selected_json = if needs_selected {
        Some(run_gh(&[
            "api",
            &format!("repos/{slug}/actions/permissions/selected-actions"),
        ])?)
    } else {
        None
    };
    parse_policy(&perms_json, selected_json.as_deref())
}

/// The result of the Actions-policy why-probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyProbe {
    /// No required context resolved to `STARTUP_FAILURE` — the probe was
    /// deliberately NOT attempted (it would cost an Administration-scoped
    /// call for zero information). Distinct from `Failed`, so a consumer can
    /// tell "not needed" from "tried and could not".
    NotTriggered,
    /// The probe ran; carries the live posture.
    Fetched(ActionsPolicy),
    /// The probe was required and attempted but failed (no-silent-skip — the
    /// host narrates this, then classifies without the posture).
    Failed(String),
}

/// Everything a live fetch assembles for the fight.
#[derive(Debug)]
pub struct FetchBundle {
    /// The gate: required contexts bound to their realised runs.
    pub gate: Gate,
    /// Green checks worth a polarity inspection (issue #58).
    pub greens: Vec<GreenCheck>,
    /// Required contexts whose realised run concluded `STARTUP_FAILURE`
    /// (issue #15) — the inputs that upgrade a mis-attribution into an
    /// Actions-policy move.
    pub startup_failed: Vec<String>,
    /// The why-probe for those startups.
    pub policy_probe: PolicyProbe,
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

/// Why a fetch produced no gate.
///
/// `NoGate` is a **finding**, not a malfunction: the base branch carries no
/// `required_status_checks` ruleset rule, so there is genuinely nothing to
/// triage. Everything else — `gh` failing, a malformed slug, JSON that will
/// not parse — is `Failed`.
///
/// They are separate variants because they were previously the same one.
/// `fetch` returned a bare `String` for both, the CLI mapped every error to
/// exit 2, and so a caller had to choose between treating a real breakage as
/// a clean skip or treating a true non-finding as a broken build. Both are
/// wrong. A consumer cannot ask a question the producer never answers, so the
/// answer is given here rather than guessed downstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// No `required_status_checks` rule applies to the PR's base branch.
    NoGate { slug: String, branch: String },
    /// Any other failure. Carries the message it always carried.
    Failed(String),
}

impl FetchError {
    /// Exit code for "there is no gate here" — a reportable non-finding.
    pub const NO_GATE_EXIT: u8 = 3;
    /// Exit code for a genuine malfunction. Unchanged, so existing callers
    /// that only know about 2 keep failing on exactly what they failed on.
    pub const FAILED_EXIT: u8 = 2;

    /// The process exit code this error should produce.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::NoGate { .. } => Self::NO_GATE_EXIT,
            Self::Failed(_) => Self::FAILED_EXIT,
        }
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Unqualified on purpose (issue #100, criterion 6): the fetch now
            // reads BOTH protection mechanisms — the branch rulesets AND the
            // classic `branches/{b}/protection` endpoint — so an empty union
            // really does mean no required status checks apply. The earlier
            // caveated phrasing ("no ruleset rule applies … classic protection
            // is not visible to this query") was accurate only while the
            // classic API was never consulted; keeping it now would understate
            // what was actually checked.
            Self::NoGate { slug, branch } => write!(
                f,
                "no required status checks apply to `{slug}` branch `{branch}` — nothing \
                 to squabble over. (Checked both protection mechanisms: branch rulesets \
                 and classic branch protection.)"
            ),
            Self::Failed(msg) => f.write_str(msg),
        }
    }
}

/// Lets `?` keep working on the many helpers that still yield `String`.
impl From<String> for FetchError {
    fn from(msg: String) -> Self {
        Self::Failed(msg)
    }
}

/// Fetch a live PR's gate from GitHub via `gh` and return it as a [`Gate`].
///
/// `slug` is `owner/repo`. Requires `gh` to be authenticated for that repo —
/// the same precondition every other `gh`-based estate tool already has.
pub fn run(slug: &str, pr: &str) -> Result<Gate, FetchError> {
    run_bundle(slug, pr).map(|b| b.gate)
}

/// As [`run`], but also returns the checks that concluded **success**, with the
/// job id needed to inspect their steps.
///
/// The green set is what [`squabble_core::polarity`] classifies. `fight` only
/// ever looks at reds, so a gate that could not run reports green and is never
/// inspected — that is the whole fake-green class.
pub fn run_with_greens(slug: &str, pr: &str) -> Result<(Gate, Vec<GreenCheck>), FetchError> {
    run_bundle(slug, pr).map(|b| (b.gate, b.greens))
}

/// The full live fetch: gate, inspectable greens, and the Actions-policy
/// why-probe inputs (issue #15) for any required context that refused to
/// start. `fight` consumes this; the narrower entry points project from it.
///
/// Protection-mechanism coverage (issue #100): the required-context set is
/// the UNION of the branch ruleset's `required_status_checks` contexts and
/// classic branch protection's `required_status_checks` — GitHub enforces
/// both when both exist. A 404 on the classic endpoint is *not protected*; a
/// 403 is *not visible to this token*, which is an error, never a "no gate".
pub fn run_bundle(slug: &str, pr: &str) -> Result<FetchBundle, FetchError> {
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

    let protection = probe_classic_protection(slug, &pr_view.base_ref_name)?;
    let required_contexts = required_contexts_from_apis(&rules_json, &protection)?;
    let required_contexts =
        contexts_or_no_gate(required_contexts, slug, &pr_view.base_ref_name)?;

    let startup_failed =
        startup_failures_from_rollup(&required_contexts, &pr_view.status_check_rollup);
    let policy_probe = if startup_failed.is_empty() {
        PolicyProbe::NotTriggered
    } else {
        match fetch_actions_policy(slug) {
            Ok(p) => PolicyProbe::Fetched(p),
            Err(e) => PolicyProbe::Failed(e),
        }
    };

    Ok(FetchBundle {
        gate: build_gate(&required_contexts, &pr_view.status_check_rollup),
        greens: greens_from_rollup(&pr_view.status_check_rollup),
        startup_failed,
        policy_probe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- FetchError: the whole point is that these two are distinguishable ---

    /// The mutant this guards against: someone "simplifying" the codes back to
    /// a single value. If both constants become 2, every caller silently
    /// returns to being unable to tell a non-finding from a breakage — the
    /// exact defect this type exists to remove — and nothing else in the suite
    /// would notice.
    #[test]
    fn no_gate_and_failure_do_not_share_an_exit_code() {
        assert_ne!(
            FetchError::NO_GATE_EXIT,
            FetchError::FAILED_EXIT,
            "a shared exit code makes the two outcomes indistinguishable to any caller"
        );
    }

    #[test]
    fn each_variant_maps_to_its_own_exit_code() {
        let no_gate = FetchError::NoGate {
            slug: "o/r".into(),
            branch: "main".into(),
        };
        assert_eq!(no_gate.exit_code(), 3);
        assert_eq!(FetchError::Failed("gh exploded".into()).exit_code(), 2);
    }

    /// `?` converts every `String` error in this module through `From`. If that
    /// conversion ever produced `NoGate`, a genuine breakage would be reported
    /// as a clean non-finding and the build would go green on a broken tool.
    #[test]
    fn an_arbitrary_error_string_becomes_failed_never_no_gate() {
        let e: FetchError = String::from("could not parse ruleset response").into();
        assert_eq!(
            e,
            FetchError::Failed("could not parse ruleset response".into())
        );
        assert_eq!(e.exit_code(), FetchError::FAILED_EXIT);
    }

    /// Issue #100, criterion 6 — the stronger claim. Since the fetch reads BOTH
    /// protection mechanisms (rulesets AND classic branch protection), the
    /// `NoGate` message is the unqualified finding it could not be while it
    /// queried only one API. This test is the successor of #99's
    /// `the_no_gate_message_does_not_claim_the_branch_is_unprotected`, which
    /// guarded against overclaiming on partial evidence. The evidence is no
    /// longer partial, so the message must no longer hedge — and must not
    /// keep the retired caveated phrasing either.
    #[test]
    fn the_no_gate_message_is_now_the_unqualified_finding() {
        let msg = FetchError::NoGate {
            slug: "hyperpolymath/MetaManifold-WebUI".into(),
            branch: "main".into(),
        }
        .to_string();
        assert!(msg.contains("hyperpolymath/MetaManifold-WebUI"), "{msg}");
        assert!(msg.contains("main"), "{msg}");
        assert!(
            msg.contains("no required status checks apply"),
            "must now state the unqualified finding: {msg}"
        );
        assert!(
            msg.contains("both protection mechanisms"),
            "must say what was actually checked: {msg}"
        );
        assert!(
            !msg.contains("is not visible to this query"),
            "keeps the caveat from the single-API era: {msg}"
        );
    }

    // --- Issue #100, criterion 5: a fixture per protection mechanism ---
    //
    // These four run against the pure seam `required_contexts_from_apis` +
    // `contexts_or_no_gate`, which is where the fetch pipeline's behaviour
    // per mechanism is decided. The `gh` plumbing that feeds it (`run_bundle`)
    // is a thin shell: probe both endpoints, hand the payloads here.

    /// The measured shape of `repos/{o}/{r}/rules/branches/main` for a
    /// ruleset-gated branch (2 contexts), with no classic protection.
    const RULESET_TWO_CONTEXTS: &str = r#"[
        {
            "type": "required_status_checks",
            "parameters": {
                "required_status_checks": [
                    {"context": "build"},
                    {"context": "test"}
                ],
                "strict_required_status_checks_policy": true
            }
        },
        {"type": "pull_request", "parameters": {}}
    ]"#;

    /// The measured shape of `repos/{o}/{r}/branches/main/protection` for a
    /// classically-protected branch (both wire shapes present).
    const CLASSIC_TWO_CONTEXTS: &str = r#"{
        "url": "https://api.github.com/repos/o/r/branches/main/protection",
        "required_status_checks": {
            "url": "https://api.github.com/repos/o/r/branches/main/protection/required-status-checks",
            "strict": true,
            "contexts": ["ci", "lint"],
            "checks": [
                {"context": "ci", "app_id": null},
                {"context": "lint", "app_id": 15368}
            ],
            "contexts_url": "https://api.github.com/repos/o/r/branches/main/protection/required-status-checks/contexts"
        },
        "enforce_admins": {"url": "…", "enabled": false}
    }"#;

    #[test]
    fn ruleset_gated_fixture_yields_the_ruleset_contexts() {
        let contexts =
            required_contexts_from_apis(RULESET_TWO_CONTEXTS, &ProtectionProbe::NotProtected)
                .expect("a ruleset gate must parse");
        assert_eq!(contexts, vec!["build".to_string(), "test".to_string()]);
        let gated = contexts_or_no_gate(contexts, "o/r", "main");
        assert!(gated.is_ok(), "a ruleset-gated branch is gated, not NoGate");
    }

    #[test]
    fn classic_protected_fixture_yields_the_classic_contexts() {
        // Criterion 5's load-bearing case: with an EMPTY ruleset list and a
        // classic-protected branch, the fetch must produce the classic
        // contexts — asserted as "the outcome is a gate" (the result the CLI
        // exits 0 on, never NoGate/3) AND as the exact context list, because
        // asserting merely "not 3" would not close the defect.
        let gated = contexts_or_no_gate(
            required_contexts_from_apis("[]", &ProtectionProbe::Protected(
                CLASSIC_TWO_CONTEXTS.to_string(),
            ))
            .expect("classic protection must parse"),
            "o/r",
            "main",
        );
        let contexts = gated.unwrap_or_else(|e| {
            panic!("a classically-protected branch must fetch as gated (exit-0 shape), got {e:?}")
        });
        assert_eq!(contexts, vec!["ci".to_string(), "lint".to_string()]);
    }

    #[test]
    fn both_mechanisms_union_the_contexts() {
        // GitHub enforces BOTH mechanisms when both exist, so the gate must
        // require the union — ruleset contexts first, classic-only ones after,
        // duplicates dropped (`ci` appears in both here).
        let classic_overlap = r#"{
            "required_status_checks": {
                "contexts": ["ci", "classic-only"],
                "checks": [{"context": "ci"}, {"context": "classic-only"}]
            }
        }"#;
        let ruleset_with_ci = r#"[
            {"type": "required_status_checks",
             "parameters": {"required_status_checks": [{"context": "ci"}, {"context": "ruleset-only"}]}}
        ]"#;
        let contexts = required_contexts_from_apis(
            ruleset_with_ci,
            &ProtectionProbe::Protected(classic_overlap.to_string()),
        )
        .expect("union must parse");
        assert_eq!(
            contexts,
            vec![
                "ci".to_string(),
                "ruleset-only".to_string(),
                "classic-only".to_string()
            ]
        );
    }

    #[test]
    fn a_genuinely_unprotected_branch_is_the_only_no_gate() {
        let result = contexts_or_no_gate(
            required_contexts_from_apis("[]", &ProtectionProbe::NotProtected)
                .expect("empty inputs must parse"),
            "hyperpolymath/MetaManifold-WebUI",
            "main",
        );
        let err = result.expect_err("neither mechanism requires anything");
        assert!(
            matches!(err, FetchError::NoGate { .. }),
            "expected NoGate, got {err:?}"
        );
        assert_eq!(err.exit_code(), FetchError::NO_GATE_EXIT);
    }

    #[test]
    fn a_hidden_classic_protection_is_never_read_as_no_gate() {
        // The issue's ⚠: a 403 on the protection endpoint must NOT become a
        // "no gate" — even when the ruleset query DID return contexts. A
        // partially-visible gate cannot be reasoned about at all.
        for rules in ["[]", RULESET_TWO_CONTEXTS] {
            let err = required_contexts_from_apis(rules, &ProtectionProbe::Hidden)
                .expect_err("hidden protection must hard-fail");
            assert!(
                matches!(err, FetchError::Failed(_)),
                "expected Failed (exit 2), not NoGate: {err:?}"
            );
            assert_eq!(err.exit_code(), FetchError::FAILED_EXIT);
            assert!(
                err.to_string().contains("not visible to this token"),
                "must name the visibility failure: {err}"
            );
        }
    }

    #[test]
    fn gh_stderr_shapes_classify_404_and_403() {
        // Real `gh api` stderr shapes (measured): a 404 prints "Branch not
        // protected", a 403 prints "Resource not accessible by …".
        assert_eq!(
            probe_from_stderr("gh: Branch not protected (HTTP 404)"),
            Some(ProtectionProbe::NotProtected)
        );
        assert_eq!(
            probe_from_stderr("gh: Resource not accessible by integration (HTTP 403)"),
            Some(ProtectionProbe::Hidden)
        );
        assert_eq!(
            probe_from_stderr("gh: Not Found (HTTP 404)"),
            Some(ProtectionProbe::NotProtected)
        );
        assert_eq!(probe_from_stderr("gh: validation failed"), None);
    }

    #[test]
    fn classic_checks_shape_is_read_without_contexts() {
        // Some payloads carry only the newer `checks` objects.
        let json = r#"{"required_status_checks": {"strict": false, "checks": [{"context": "gate"}]}}"#;
        assert_eq!(
            parse_classic_contexts(json).expect("parse"),
            vec!["gate".to_string()]
        );
        // And protection with no status-check requirement at all yields none.
        assert!(parse_classic_contexts(r#"{"enforce_admins": {"enabled": true}}"#)
            .expect("parse")
            .is_empty());
    }

    #[test]
    fn mixed_check_runs_and_commit_statuses_parse_without_losing_failures() {
        let json = r#"{"baseRefName":"main","statusCheckRollup":[
            {"__typename":"CheckRun","name":"CI","status":"COMPLETED","conclusion":"SUCCESS"},
            {"__typename":"StatusContext","context":"CodeRabbit","state":"SUCCESS"},
            {"__typename":"StatusContext","context":"External review","state":"FAILURE"},
            {"__typename":"StatusContext","context":"Pending review","state":"PENDING"}
        ]}"#;
        let parsed: PrView = serde_json::from_str(json).expect("both GitHub rollup variants");
        assert_eq!(parsed.status_check_rollup.len(), 4);
        assert_eq!(
            parse_rollup(&parsed.status_check_rollup[0]),
            CheckRun::Passed
        );
        assert_eq!(
            parse_rollup(&parsed.status_check_rollup[1]),
            CheckRun::Passed
        );
        assert_eq!(
            parse_rollup(&parsed.status_check_rollup[2]),
            CheckRun::Failed
        );
        assert_eq!(
            parse_rollup(&parsed.status_check_rollup[3]),
            CheckRun::Pending
        );
        assert!(greens_from_rollup(&parsed.status_check_rollup).is_empty());
    }

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
        let url =
            "https://github.com/hyperpolymath/standards/actions/runs/33817314194/job/100852208701";
        assert_eq!(job_id_from_details_url(url), Some(100852208701));
    }

    #[test]
    fn a_query_string_or_fragment_does_not_hide_the_job_id() {
        // GitHub appends `?check_suite_focus=true` to details URLs as a matter
        // of course. Cutting the id on `/` alone leaves the suffix attached,
        // `parse::<u64>` fails, and the green is skipped without a word — the
        // exact silent undercount this module exists to prevent.
        let base =
            "https://github.com/hyperpolymath/standards/actions/runs/33817314194/job/100852208701";
        for suffix in ["?check_suite_focus=true", "#step:4:1", "?a=1#step:2:9"] {
            let url = format!("{base}{suffix}");
            assert_eq!(
                job_id_from_details_url(&url),
                Some(100852208701),
                "suffix {suffix} must not hide the job id"
            );
        }
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
            job_id_from_details_url("https://github.com/o/r/actions/runs/1"),
            None
        );
    }

    #[test]
    fn an_external_job_url_is_not_treated_as_a_github_actions_job() {
        assert_eq!(job_id_from_details_url("https://ci.example/job/42"), None);
    }

    #[test]
    fn only_successful_checks_with_an_inspectable_job_are_green() {
        let rollup = vec![
            green(
                "has-a-job",
                Some("https://github.com/o/r/actions/runs/1/job/42"),
            ),
            green("no-details-url", None),
            green("not-a-job", Some("https://example.com/status")),
            RollupEntry {
                name: "red".into(),
                status: Some("COMPLETED".into()),
                conclusion: Some("FAILURE".into()),
                details_url: Some("https://github.com/o/r/actions/runs/1/job/43".into()),
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
        // An absent `steps` array is a legitimate payload — the caller reports
        // it as an uninspectable green — so this must parse rather than error.
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
        let sig = squabble_fight::gate_triage::load_signatures(root);
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
        let sig = squabble_fight::gate_triage::load_signatures(root);
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

#[cfg(test)]
mod policy_plumbing_tests {
    use super::*;

    fn entry_with_conclusion(name: &str, conclusion: Option<&str>) -> RollupEntry {
        RollupEntry {
            name: name.to_string(),
            status: Some("COMPLETED".into()),
            conclusion: conclusion.map(String::from),
            details_url: None,
        }
    }

    #[test]
    fn only_required_contexts_with_startup_failure_trigger_the_probe() {
        let required = vec!["gate / a".to_string(), "gate / b".to_string()];
        let rollup = vec![
            entry_with_conclusion("gate / a", Some("STARTUP_FAILURE")),
            entry_with_conclusion("gate / b", Some("SUCCESS")),
            // An UN-required startup failure must not trigger a probe either.
            entry_with_conclusion("other / c", Some("STARTUP_FAILURE")),
        ];
        assert_eq!(
            startup_failures_from_rollup(&required, &rollup),
            vec!["gate / a".to_string()]
        );
        // Nothing startup-failed → NotTriggered is the correct probe outcome.
        let quiet = vec![entry_with_conclusion("gate / a", Some("SUCCESS"))];
        assert!(startup_failures_from_rollup(&required, &quiet).is_empty());
    }

    #[test]
    fn startup_failure_still_classifies_failed_in_the_gate() {
        // The gate model is untouched: STARTUP_FAILURE is a Failed check (the
        // SPARK machine knows no fourth state). The why-probe is ADDITIONAL
        // information carried beside the gate, never a new gate colour.
        assert_eq!(
            parse_rollup(&entry_with_conclusion("x", Some("STARTUP_FAILURE"))),
            CheckRun::Failed
        );
    }

    #[test]
    fn the_estate_default_posture_parses() {
        // allowed_actions=all + sha_pinning_required=true — the decision of
        // record; no selected-actions call is made under it.
        let policy = parse_policy(
            r#"{"enabled": true, "allowed_actions": "all", "sha_pinning_required": true}"#,
            None,
        )
        .expect("parse");
        assert_eq!(policy.allowed_actions, "all");
        assert!(policy.sha_pinning_required);
        assert!(policy.github_owned_allowed);
        assert!(policy.patterns_allowed.is_empty());
    }

    #[test]
    fn an_empty_selected_allowlist_parses_with_its_patterns() {
        let policy = parse_policy(
            r#"{"enabled": true, "allowed_actions": "selected", "sha_pinning_required": true}"#,
            Some(r#"{"github_owned_allowed": true, "verified_allowed": false, "patterns_allowed": []}"#),
        )
        .expect("parse");
        assert_eq!(policy.allowed_actions, "selected");
        assert!(policy.sha_pinning_required);
        assert!(policy.github_owned_allowed);
        assert!(policy.patterns_allowed.is_empty());
    }

    #[test]
    fn a_populated_selected_allowlist_keeps_its_patterns() {
        let policy = parse_policy(
            r#"{"enabled": true, "allowed_actions": "selected"}"#,
            Some(
                r#"{"github_owned_allowed": true, "verified_allowed": true,
                    "patterns_allowed": ["hyperpolymath/*", "oven-sh/setup-bun@*"]}"#,
            ),
        )
        .expect("parse");
        assert_eq!(
            policy.patterns_allowed,
            vec![
                "hyperpolymath/*".to_string(),
                "oven-sh/setup-bun@*".to_string()
            ]
        );
    }

    #[test]
    fn a_selected_posture_without_its_second_payload_is_an_error() {
        let err = parse_policy(r#"{"enabled": true, "allowed_actions": "selected"}"#, None)
            .expect_err("selected without selected-actions cannot be understood");
        assert!(err.contains("no selected-actions payload"), "{err}");
    }
}
