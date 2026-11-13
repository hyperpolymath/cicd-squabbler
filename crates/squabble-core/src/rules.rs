// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! The first-line rule catalogue (ADR 0004, increment 1).
//!
//! Pure classification, guards and the referral message. Nothing here does
//! IO or applies a fix; the executors (opening PRs, editing locks) follow in
//! a later increment. The squabbler fixes only what is safe, and otherwise
//! refers the problem to Hypatia (ADR 0003).
//!
//! `match_log` covers the rules that have a runtime log signature. These
//! rules have none and are left out of it: SQ-LOCK-03, SQ-PIN-01, SQ-PIN-04,
//! SQ-PIN-05, SQ-TOOL-01, SQ-CANON-01, SQ-RUN-02, SQ-CODE-02, SQ-CODE-03,
//! SQ-CODE-04, SQ-REF-08, SQ-OWN-03, SQ-OWN-05, SQ-OWN-06, SQ-OWN-07,
//! SQ-OWN-08 and SQ-NOI-03. Their evidence is the workflow source, run
//! metadata or PR state, not a log line. SQ-LOCK-01 and SQ-PIN-03 also have
//! static detectors below.

use serde::{Deserialize, Serialize};

/// What the squabbler may do about a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleClass {
    /// One PR that changes CI configuration only.
    Fix,
    /// A first-line action without a code change (re-run once, update branch).
    Act,
    /// Referral to Hypatia.
    Refer,
    /// Only the owner can act; the squabbler reports.
    Owner,
    /// Not a defect; the squabbler labels it.
    Noise,
}

/// One row of the ADR 0004 catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Rule {
    pub id: &'static str,
    pub class: RuleClass,
    pub summary: &'static str,
    pub fix: &'static str,
    pub check: &'static str,
    pub refer_when: &'static str,
}

const fn r(
    id: &'static str,
    class: RuleClass,
    summary: &'static str,
    fix: &'static str,
    check: &'static str,
    refer_when: &'static str,
) -> Rule {
    Rule {
        id,
        class,
        summary,
        fix,
        check,
        refer_when,
    }
}

use RuleClass::{Act, Fix, Noise, Owner, Refer};

/// The 32 catalogue rows, in ADR 0004 order.
pub static CATALOGUE: &[Rule] = &[
    r(
        "SQ-LOCK-01",
        Fix,
        "invalid lockfile pin: an actions.lock record keyed owner/repo@<40-hex> repeats that SHA as its ref",
        "Refresh scripts/check-lock-sync.sh from template main, then set each flagged ref to the highest full-semver tag at the SHA, else highest semver tag, else first tag, else a branch containing the commit",
        "The template lock-sync gate exits 0 and the failed workflow no longer reports invalid lockfile pin; scripts/check-lock-sync.sh needs GNU awk and mikefarah yq v4, and a missing tool means the check did not run, never a pass",
        "No tag or branch holds the SHA; tags tie; the lock also lacks entries",
    ),
    r(
        "SQ-LOCK-02",
        Fix,
        "lockfile missing pin for an action called inside another action",
        "Regenerate the lock with gh actions-lock",
        "The lock-sync gate passes and the job starts",
        "The regenerated lock disagrees with the workflow YAML",
    ),
    r(
        "SQ-LOCK-03",
        Fix,
        "reusable-calling workflow ends with zero jobs; the lock records the callee's old owner_id after a transfer",
        "Repoint uses: to the canonical owner/name at a standards commit whose allowlist already covers metadatastician/standards/.github/workflows/*@* (8d1a2aa or later; governance / Allowlist Preflight reads the allowlist at the pinned commit), and relock with scripts/relock-standards-pin.sh from standards main",
        "Each record's owner_id equals the repository API owner id; the next run has jobs; check-allowed-actions.sh <allowlist at target> .github/workflows reports 0 gaps",
        "The pinned SHA is not reachable from the callee's default branch, or the callee is outside the estate, or no standards commit with that allowlist pattern is also free of pin-contract violations for the repository",
    ),
    r(
        "SQ-PIN-01",
        Fix,
        "a pinned reusable ends startup_failure on every push while a newer audited SHA runs green elsewhere",
        "Repin to the newest audited green SHA and update the closure rows and pinned-reusable-sources.json together",
        "The next push run has jobs",
        "No audited closure row exists for the target SHA, or the new SHA declares more secrets",
    ),
    r(
        "SQ-PIN-02",
        Fix,
        "the DEED lane fails unrecognised #-form on a literal the current ABNF admits: the repository pins an old standards SHA",
        "Bump every standards pin site in .github/workflows and scripts to one current SHA",
        "No pin site keeps the old SHA and validate-deed.sh passes at the new pin",
        "The newer linter rejects existing deed content; deed meaning is never edited",
    ),
    r(
        "SQ-PIN-03",
        Fix,
        "setup-bun fails on the GitHub API rate limit, or a setup-bun step has no exact bun-version",
        "Set bun-version to the estate pin 1.4.2",
        "standards scripts/check-setup-bun-pinned.sh passes",
        "The repository pins another Bun on purpose, or the step is in a third-party, forked or vendored workflow",
    ),
    r(
        "SQ-PIN-04",
        Fix,
        "cargo install --git of the old panic-attacker name with no pin in a workflow",
        "Use hyperpolymath/panic-attack at the pin in gitbot-fleet bots/panicbot/canon/pin.toml",
        "No unpinned --git install remains",
        "The name maps to more than one repository",
    ),
    r(
        "SQ-PIN-05",
        Fix,
        "a workflow whose job is a cross-repository uses: <owner>/<repo>/.github/workflows/<x>.yml@<sha> ends failure or startup_failure with zero jobs on every push",
        "Replace the caller with the inline-job form that rsr-template-repo main uses for the same workflow (it runs the gates from the pinned SHA without a cross-repository workflow_call) and keep actions.lock in sync",
        "The next push run is success with more than 0 jobs and the lock-sync gate passes",
        "The template has no inline form for that workflow, a required context name would change, the repository customised the caller on purpose, or the inline form still gives 0 jobs",
    ),
    r(
        "SQ-TOOL-01",
        Fix,
        "different nickel or yq versions across CI, Containerfile, mise and setup scripts",
        "One version everywhere, sha256-pinned (Nickel 1.18.0; yq v4.53.3)",
        "One version found; the installer verifies the sha256",
        "No upstream checksums exist (state trust-on-first-download, never verified)",
    ),
    r(
        "SQ-GEN-01",
        Fix,
        "estate-rules reports the committed REPOSITORY-MAP.adoc is stale",
        "Run bash scripts/gen-repo-map.sh . and commit the map",
        "tests/shape/repo_map_determinism_test.sh passes",
        "The map still differs after regeneration",
    ),
    r(
        "SQ-GEN-02",
        Fix,
        "a deed or k9 line is over 998 octets (K9-E006)",
        "Wrap the string or description, layout only",
        "deed_lint.js and k9-validate --strict pass",
        "A wrap would change a value (identifier, hash, URL)",
    ),
    r(
        "SQ-CANON-01",
        Fix,
        "an instance's canon-lockstep job is an old inline copy and goes red on a canon release",
        "Run rsr-template-repo scripts/adopt-canon-lockstep.sh",
        "The canon-lockstep job passes with the lag notice",
        "The script refuses (lock coverage), or the pin itself must change (A2ML is never edited automatically)",
    ),
    r(
        "SQ-PERM-01",
        Fix,
        "Resource not accessible by integration at get-a-workflow-run in a job that uploads SARIF on a private repository",
        "Add actions: read to that job's permissions, with a comment that names the reason",
        "The upload step passes on a re-run",
        "The permission is already present, or the repository is public",
    ),
    r(
        "SQ-RUN-01",
        Act,
        "a setup or download step fails with API rate limit exceeded and the same workflow passes on an adjacent commit",
        "Re-run the failed jobs once after the rate-limit reset, only if that commit is still the head",
        "The re-run passes",
        "It fails twice on the same commit, the log has no rate-limit text, or the step is not a setup or download step",
    ),
    r(
        "SQ-RUN-02",
        Act,
        "a PR is blocked with required checks green under a strict ruleset and its base is behind the default branch",
        "Update the branch by merging the base; never rebase a branch the squabbler did not open",
        "The required checks pass on the new head",
        "Merge conflict, or still blocked on an up-to-date head",
    ),
    r(
        "SQ-CODE-01",
        Fix,
        "a Rust readiness or integration test cannot find its binary under target/debug/build",
        "Use PathBuf::from(env!(\"CARGO_BIN_EXE_<bin>\")); a code fix, first-line only when the code-fix guard passes",
        "cargo test --test readiness passes",
        "The fix would hit the code-fix unsafe list, the code is vendored or third-party, or the check does not pass on the PR head",
    ),
    r(
        "SQ-CODE-02",
        Fix,
        "Validate Hypatia Baseline fails on new code_safety findings",
        "Fix the code behind the finding; never touch .hypatia-baseline.json",
        "The baseline job passes with the baseline file unchanged",
        "The fix would hit the code-fix unsafe list, the code is vendored or third-party, or the check does not pass on the PR head",
    ),
    r(
        "SQ-CODE-03",
        Fix,
        "Sonar S7688 or S7679 in shell scripts",
        "Make the mechanical edit; decline S1192 in test files, where repetition is a convention",
        "bash -n, shellcheck and the repository's shell tests pass",
        "The fix would hit the code-fix unsafe list, the code is vendored or third-party, or the check does not pass on the PR head",
    ),
    r(
        "SQ-REF-06",
        Refer,
        "the DEED job fails could not read Username for https://github.com on a private repository",
        "Root cause: OTP 28 (28.3 and 28.5.0.8) miscompiles Hypatia.Rules.PinSites.walk/4; the template #328 shape only shifts memory layout. The fix is hyperpolymath/hypatia#1062. Never change the workflow to hide it",
        "DEED syntax passes on a private PR",
        "Always, until hypatia#1062 lands",
    ),
    r(
        "SQ-REF-08",
        Refer,
        "Repo self-tests fails in dismiss-code-scanning-alerts-test.sh with <file>:<line> is not a write permission after lines were added above a permissions: block that .github/code-scanning-dismissals.json keys by line number",
        "Suggest keeping the recorded line where it is: put new comments inline or below it; never re-key the dismissal (it must match the live Scorecard alert's start_line on main)",
        "The test reports 0 failed",
        "Always when the permission line itself must move or change (re-keying after Scorecard re-scans main is the owner's call)",
    ),
    r(
        "SQ-CODE-04",
        Fix,
        "an adapter prints a valid findings array but exits other than 0 or 1 and is reported as ran",
        "Accept arrays only for exit 0 or 1 (fleet-report-back.sh translate_or_error)",
        "Tests for exits 2, 3, 9 and 137",
        "The fix would hit the code-fix unsafe list, the code is vendored or third-party, or the check does not pass on the PR head",
    ),
    r(
        "SQ-OWN-01",
        Owner,
        "Get Pages site failed: Not Found",
        "Report: Pages is a repository setting; never enable it",
        "The pages job is green or absent",
        "(owner)",
    ),
    r(
        "SQ-OWN-02",
        Owner,
        "a mirror job is enabled and webfactory/ssh-agent fails: the ssh-private-key argument is empty",
        "Report: add the secret or set the enable variable to false",
        "The mirror job passes or is skipped",
        "(owner)",
    ),
    r(
        "SQ-OWN-03",
        Owner,
        "pull_request_target or workflow_run jobs check out or run PR code with secrets or a write token",
        "Report only (security)",
        "(owner)",
        "(owner)",
    ),
    r(
        "SQ-OWN-04",
        Owner,
        "a push is rejected with GH013: commits must have verified signatures",
        "Report; never amend or force-push someone else's branch",
        "(owner)",
        "(owner)",
    ),
    r(
        "SQ-OWN-05",
        Owner,
        "a committed data file holds a local absolute path with an account name",
        "Report (privacy)",
        "(owner)",
        "(owner)",
    ),
    r(
        "SQ-OWN-06",
        Owner,
        "a bot PR shows only external checks and no Actions runs: the workflow-approval state",
        "Report if the PR is meant to auto-merge",
        "(owner)",
        "(owner)",
    ),
    r(
        "SQ-OWN-07",
        Owner,
        "analyze (actions, none) stays red while GitHub's default code-scanning setup is on",
        "Report: the owner turns default setup off; never work around it in a workflow",
        "(owner)",
        "(owner)",
    ),
    r(
        "SQ-OWN-08",
        Owner,
        "a workflow ends startup_failure with 0 jobs and no check-runs or annotations while the byte-identical file with the same SHA pins succeeds in rsr-template-repo",
        "Report: the cause is repository-level (Actions policy, allowed-actions list, Pages setting); the owner reads the failed run's banner or Settings > Actions; never rewrite the workflow to chase it",
        "(owner)",
        "(owner)",
    ),
    r(
        "SQ-NOI-01",
        Noise,
        "Fork governance failed (deed=cancelled) on a head that a newer commit replaced",
        "Label superseded; read the newest head",
        "The newest head is not deed=cancelled",
        "deed=cancelled on the newest head",
    ),
    r(
        "SQ-NOI-02",
        Noise,
        "couldn't find remote ref refs/pull/<N>/merge and the PR closed before the job started",
        "None: do not re-run a closed PR",
        "The PR API shows the PR closed",
        "The PR is still open (conflict or deleted ref)",
    ),
    r(
        "SQ-NOI-03",
        Noise,
        "Inbox Steward ends startup_failure only on runs triggered by chatgpt-codex-connector[bot]",
        "None",
        "Count of startup failures by actor",
        "Inbox Steward becomes a required check",
    ),
];

/// Look a rule up by id.
pub fn rule(id: &str) -> Option<&'static Rule> {
    CATALOGUE.iter().find(|r| r.id == id)
}

/// Rule ids whose runtime signature appears in a job log. `step` is the name
/// of the failed step, when known; SQ-PIN-03 needs it to name setup-bun.
pub fn match_log(log: &str, step: Option<&str>) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    let has = |s: &str| log.contains(s);
    let rate_limited = has("API rate limit exceeded");
    let setup_bun_step = step.is_some_and(|s| s.to_ascii_lowercase().contains("setup-bun"));

    if has("invalid lockfile pin") {
        out.push("SQ-LOCK-01");
    }
    if has("lockfile missing pin for") {
        out.push("SQ-LOCK-02");
    }
    if has("unrecognised #-form") {
        out.push("SQ-PIN-02");
    }
    if setup_bun_step
        && (rate_limited
            || has("Failed to fetch url https://api.github.com/repos/oven-sh/bun/git/refs/tags"))
    {
        out.push("SQ-PIN-03");
    }
    if rate_limited {
        out.push("SQ-RUN-01");
    }
    if has("committed REPOSITORY-MAP.adoc is stale") {
        out.push("SQ-GEN-01");
    }
    if has("K9-E006") || (has("line is") && has("octets")) {
        out.push("SQ-GEN-02");
    }
    if has("Resource not accessible by integration") && has("get-a-workflow-run") {
        out.push("SQ-PERM-01");
    }
    if has("target/debug/build/") && has("No such file") {
        out.push("SQ-CODE-01");
    }
    if has("could not read Username for 'https://github.com'") {
        out.push("SQ-REF-06");
    }
    if has("Get Pages site failed") {
        out.push("SQ-OWN-01");
    }
    if has("The ssh-private-key argument is empty") {
        out.push("SQ-OWN-02");
    }
    if has("GH013") {
        out.push("SQ-OWN-04");
    }
    if has("deed=cancelled") {
        out.push("SQ-NOI-01");
    }
    if has("couldn't find remote ref refs/pull/") {
        out.push("SQ-NOI-02");
    }
    out
}

fn is_sha40(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    let s = match s.find(" #") {
        Some(i) if !s.starts_with(['\'', '"']) => s[..i].trim(),
        _ => s,
    };
    s.trim_matches(|c| c == '\'' || c == '"')
}

/// Static SQ-LOCK-01 detection. Returns the lowercased keys
/// (`owner/repo@<sha>`) of records whose `ref:` repeats the SHA in the key,
/// unless a job-level reusable caller names that key. A nested `uses:` is not
/// an exemption.
pub fn self_sha_lock_records(lock_yaml: &str, job_level_callers: &[String]) -> Vec<String> {
    let callers: Vec<String> = job_level_callers
        .iter()
        .map(|c| c.trim().to_ascii_lowercase())
        .collect();
    let mut flagged = Vec::new();
    let mut current: Option<(String, usize)> = None;
    for line in lock_yaml.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let ind = indent_of(line);
        if let Some((_, key_ind)) = &current {
            if ind <= *key_ind {
                current = None;
            }
        }
        if current.is_none() && !t.starts_with('-') {
            if let Some(k) = t.strip_suffix(':') {
                let k = k.trim().trim_matches(|c| c == '\'' || c == '"');
                if let Some((repo, sha)) = k.split_once('@') {
                    if repo.contains('/') && is_sha40(sha) {
                        current = Some((
                            format!("{}@{}", repo.to_ascii_lowercase(), sha.to_ascii_lowercase()),
                            ind,
                        ));
                    }
                }
            }
            continue;
        }
        if let Some((key, key_ind)) = &current {
            // Direct children only: a nested `uses:` list never carries `ref:`.
            if ind > *key_ind {
                if let Some(v) = t.strip_prefix("ref:") {
                    let sha = key.split_once('@').map(|(_, s)| s).unwrap_or("");
                    if unquote(v).eq_ignore_ascii_case(sha)
                        && !callers.contains(key)
                        && !flagged.contains(key)
                    {
                        flagged.push(key.clone());
                    }
                }
            }
        }
    }
    flagged
}

/// Static SQ-PIN-03 detection: a `oven-sh/setup-bun` step whose `bun-version`
/// is missing, `latest`, `canary`, a range, or which uses `bun-version-file`.
pub fn unpinned_setup_bun(workflow_yaml: &str) -> bool {
    let lines: Vec<&str> = workflow_yaml.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with('#') {
            continue;
        }
        let (starts_dash, body) = match t.strip_prefix("- ") {
            Some(b) => (true, b.trim_start()),
            None => (false, t),
        };
        if !body.starts_with("uses:") || !body.contains("oven-sh/setup-bun@") {
            continue;
        }
        let key_ind = indent_of(line) + if starts_dash { t.len() - body.len() } else { 0 };
        let mut start = i;
        if !starts_dash {
            while start > 0 {
                let p = lines[start - 1];
                if p.trim().is_empty() {
                    start -= 1;
                    continue;
                }
                if indent_of(p) < key_ind {
                    if p.trim_start().starts_with("- ") {
                        start -= 1;
                    }
                    break;
                }
                start -= 1;
            }
        }
        let mut end = i + 1;
        while end < lines.len() {
            let n = lines[end];
            if !n.trim().is_empty() && indent_of(n) < key_ind {
                break;
            }
            end += 1;
        }
        let mut version: Option<&str> = None;
        let mut file = false;
        for l in &lines[start..end] {
            let lt = l.trim_start();
            let lt = lt.strip_prefix("- ").unwrap_or(lt);
            if let Some(v) = lt.strip_prefix("bun-version-file:") {
                let _ = v;
                file = true;
            } else if let Some(v) = lt.strip_prefix("bun-version:") {
                version = Some(unquote(v));
            }
        }
        let bad = file
            || match version {
                None => true,
                Some(v) => {
                    let lv = v.to_ascii_lowercase();
                    lv.is_empty()
                        || lv == "latest"
                        || lv == "canary"
                        || lv.contains(['^', '~', '>', '<', '*', 'x'])
                }
            };
        if bad {
            return true;
        }
    }
    false
}

/// Facts the guards read. The caller gathers them; nothing here fetches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuardInput {
    /// Value of `ESTATE_AUTOFIX`, if set.
    pub autofix: Option<String>,
    /// Fix attempts so far for this repository, rule and head commit.
    pub attempts_on_head: u32,
    /// Re-runs so far of the failed job.
    pub reruns_of_job: u32,
    /// A fix PR for this repository and rule is already open.
    pub open_fix_pr: bool,
    /// Whether the rule's check passed on the PR head (None: not run yet).
    pub check_passed: Option<bool>,
    /// The unified diff of the proposed fix, for the code-fix guard.
    pub diff: Option<String>,
}

/// What to do about a matched rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Decision {
    Apply,
    ReportOnly { reason: String },
    Refer { reason: String },
    OwnerReport,
    Label,
}

/// A `FIX` whose change touches code (not only CI configuration) must pass
/// [`code_fix_is_safe`] first (ADR 0003, owner ruling 2026-11-13).
pub fn is_code_fix(rule: &Rule) -> bool {
    rule.class == RuleClass::Fix && rule.id.starts_with("SQ-CODE-")
}

/// Apply the ADR 0004 guards to a rule.
pub fn decide(rule: &Rule, g: &GuardInput) -> Decision {
    match rule.class {
        RuleClass::Owner => return Decision::OwnerReport,
        RuleClass::Noise => return Decision::Label,
        RuleClass::Refer => {
            return Decision::Refer {
                reason: rule.refer_when.to_string(),
            }
        }
        RuleClass::Fix | RuleClass::Act => {}
    }
    if g.autofix.as_deref() != Some("on") {
        return Decision::ReportOnly {
            reason: "ESTATE_AUTOFIX is not on".to_string(),
        };
    }
    if rule.class == RuleClass::Fix && g.attempts_on_head >= 2 {
        return Decision::Refer {
            reason: "attempt limit reached: 2 fix attempts on this head".to_string(),
        };
    }
    if rule.class == RuleClass::Act && g.reruns_of_job >= 1 {
        return Decision::Refer {
            reason: "re-run limit reached: the job was already re-run once".to_string(),
        };
    }
    if rule.class == RuleClass::Fix && g.open_fix_pr {
        return Decision::ReportOnly {
            reason: "a fix PR is already open".to_string(),
        };
    }
    if is_code_fix(rule) {
        match g.diff.as_deref() {
            None => {
                return Decision::ReportOnly {
                    reason: "no diff to check".to_string(),
                }
            }
            Some(d) => {
                if let Err(reason) = code_fix_is_safe(d) {
                    return Decision::Refer { reason };
                }
            }
        }
    }
    if g.check_passed == Some(false) {
        return Decision::Refer {
            reason: "the rule's check did not pass on the PR head".to_string(),
        };
    }
    Decision::Apply
}

fn first_number(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let start = b.iter().position(|c| c.is_ascii_digit())?;
    let mut end = start;
    while end < b.len() && (b[end].is_ascii_digit() || b[end] == b'.') {
        end += 1;
    }
    s[start..end].trim_end_matches('.').parse().ok()
}

fn without_digits(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_ascii_digit() && *c != '.')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_test_path(p: &str) -> bool {
    let l = p.to_ascii_lowercase();
    l.contains("test") || l.contains("spec")
}

fn is_threshold_line(l: &str) -> bool {
    let l = l.to_ascii_lowercase();
    ["threshold", "min", "max", "coverage"]
        .iter()
        .any(|k| l.contains(k))
}

/// The code-fix guard, over a unified diff. `Err(reason)` when the diff
/// weakens a check: it removes an assertion, adds a skip or ignore, lets a
/// step fail silently, lowers a threshold, deletes a test file, or adds a
/// suppression or a baseline entry. Conservative by design: a false `Err`
/// becomes a referral, a false `Ok` would let a weakened check through.
pub fn code_fix_is_safe(diff: &str) -> Result<(), String> {
    let mut file = String::new();
    let mut old_file = String::new();
    let mut removed_thr: Vec<(String, f64)> = Vec::new();
    let mut added_thr: Vec<(String, f64)> = Vec::new();

    fn lowered(removed: &[(String, f64)], added: &[(String, f64)]) -> bool {
        added
            .iter()
            .any(|(k, n)| removed.iter().any(|(rk, rn)| rk == k && n < rn))
    }

    for line in diff.lines() {
        if let Some(p) = line.strip_prefix("--- ") {
            if line.starts_with("--- a/") || line == "--- /dev/null" {
                old_file = p.strip_prefix("a/").unwrap_or(p).to_string();
                continue;
            }
        }
        if let Some(p) = line.strip_prefix("+++ ") {
            if line.starts_with("+++ b/") || line == "+++ /dev/null" {
                if p == "/dev/null" {
                    if is_test_path(&old_file) {
                        return Err(format!("the diff deletes the test file {old_file}"));
                    }
                    file.clear();
                } else {
                    file = p.strip_prefix("b/").unwrap_or(p).to_string();
                }
                continue;
            }
        }
        if line.starts_with("@@") {
            if lowered(&removed_thr, &added_thr) {
                return Err(
                    "the diff lowers a threshold, minimum, maximum or coverage value".into(),
                );
            }
            removed_thr.clear();
            added_thr.clear();
            continue;
        }
        if let Some(body) = line.strip_prefix('-') {
            if body.contains("assert") {
                return Err("the diff removes a line containing `assert`".into());
            }
            if is_threshold_line(body) {
                if let Some(n) = first_number(body) {
                    removed_thr.push((without_digits(body), n));
                }
            }
        } else if let Some(body) = line.strip_prefix('+') {
            let base = file.rsplit('/').next().unwrap_or("");
            if base == ".hypatia-baseline.json" || base == ".hypatia-ignore" {
                return Err(format!("the diff adds a line to {base}"));
            }
            let lb = body.to_ascii_lowercase();
            let banned: &[(&str, &str)] = &[
                ("#[ignore", "adds #[ignore]"),
                ("skip", "adds a skip"),
                ("xit(", "adds xit("),
                ("continue-on-error: true", "adds continue-on-error: true"),
                ("|| true", "adds `|| true`"),
                ("hypatia: allow", "adds a `hypatia: allow` suppression"),
                ("nosemgrep", "adds a nosemgrep suppression"),
                ("#[allow(", "adds an #[allow(...)] suppression"),
                ("#![allow(", "adds an #![allow(...)] suppression"),
            ];
            for (pat, why) in banned {
                if lb.contains(pat) {
                    return Err(format!("the diff {why}"));
                }
            }
            if is_threshold_line(body) {
                if let Some(n) = first_number(body) {
                    added_thr.push((without_digits(body), n));
                }
            }
        }
    }
    if lowered(&removed_thr, &added_thr) {
        return Err("the diff lowers a threshold, minimum, maximum or coverage value".into());
    }
    Ok(())
}

/// Everything Hypatia needs to reproduce a referred problem. Absent values
/// print as `unknown` in the message; they never disappear.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Referral {
    pub rule_id: String,
    pub problem: String,
    pub fix: Option<String>,
    pub risk: String,
    pub repo: Option<String>,
    /// Full 40-hex commit SHA.
    pub commit: Option<String>,
    pub run_url: Option<String>,
    pub job_url: Option<String>,
    pub workflow_file: Option<String>,
    pub job_name: Option<String>,
    pub step_name: Option<String>,
    /// Last error lines and exit code, verbatim.
    pub signature: Option<String>,
    /// Toolchain versions from the setup step, free text.
    pub toolchain: Vec<String>,
    /// `name=sha` for checked-out tool repositories (hypatia, standards).
    pub tool_pins: Vec<String>,
    /// What the squabbler tried and why it stopped (the unsafe reason).
    pub tried: Option<String>,
    pub reproduces_on_rerun: Option<bool>,
    pub reproduces_on_base: Option<bool>,
}

fn or_unknown(v: &Option<String>) -> &str {
    v.as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("unknown")
}

fn list_or_unknown(v: &[String]) -> String {
    if v.is_empty() {
        "unknown".to_string()
    } else {
        v.join("; ")
    }
}

fn tri(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

/// The message the user reads when a problem goes to Hypatia (ADR 0003):
/// status, Hypatia's view of the problem, the fix if known, the risk, then an
/// Evidence section that lists every field.
pub fn referral_message(r: &Referral, hypatia_reachable: bool) -> String {
    let opening = if hypatia_reachable {
        "Referred to Hypatia. Not done yet: wait for its answer."
    } else {
        "Hypatia is unreachable. This is a job for Hypatia; nothing was changed."
    };
    let signature = match r.signature.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(s) => format!("\n    {}", s.lines().collect::<Vec<_>>().join("\n    ")),
        None => " unknown".to_string(),
    };
    format!(
        "{opening}\nProblem: {}\nFix: {}\nRisk: {}\nEvidence:\n  Rule: {}\n  Repository: {}\n  Commit: {}\n  Run URL: {}\n  Job URL: {}\n  Workflow file: {}\n  Job: {}\n  Step: {}\n  Signature:{signature}\n  Toolchain: {}\n  Tool pins: {}\n  Tried and stopped because: {}\n  Reproduces on re-run: {}\n  Reproduces on base: {}",
        r.problem,
        r.fix.as_deref().unwrap_or("not known yet"),
        r.risk,
        or_unknown(&Some(r.rule_id.clone())),
        or_unknown(&r.repo),
        or_unknown(&r.commit),
        or_unknown(&r.run_url),
        or_unknown(&r.job_url),
        or_unknown(&r.workflow_file),
        or_unknown(&r.job_name),
        or_unknown(&r.step_name),
        list_or_unknown(&r.toolchain),
        list_or_unknown(&r.tool_pins),
        or_unknown(&r.tried),
        tri(r.reproduces_on_rerun),
        tri(r.reproduces_on_base),
    )
}

/// The one branch a rule's fix PR lives on.
pub fn fix_branch(rule_id: &str) -> String {
    format!("squabbler/fix/{}", rule_id.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn ids(log: &str, step: Option<&str>) -> Vec<&'static str> {
        match_log(log, step)
    }

    #[test]
    fn catalogue_shape() {
        assert_eq!(CATALOGUE.len(), 32);
        let uniq: HashSet<_> = CATALOGUE.iter().map(|r| r.id).collect();
        assert_eq!(uniq.len(), 32);
        let n = |c| CATALOGUE.iter().filter(|r| r.class == c).count();
        assert_eq!(n(Fix), 17);
        assert_eq!(n(Act), 2);
        assert_eq!(n(Refer), 2);
        assert_eq!(n(Owner), 8);
        assert_eq!(n(Noise), 3);
        assert!(rule("SQ-OWN-04").is_some());
        for gone in [
            "SQ-REF-01",
            "SQ-REF-02",
            "SQ-REF-03",
            "SQ-REF-04",
            "SQ-REF-05",
            "SQ-REF-07",
        ] {
            assert!(rule(gone).is_none(), "{gone} was renamed or merged");
        }
        let refs: Vec<_> = CATALOGUE
            .iter()
            .filter(|r| r.class == Refer)
            .map(|r| r.id)
            .collect();
        assert_eq!(refs, vec!["SQ-REF-06", "SQ-REF-08"]);
        assert!(rule("SQ-XXX-99").is_none());
    }

    #[test]
    fn log_signatures_positive_and_negative() {
        let cases: &[(&str, &str)] = &[
            ("SQ-LOCK-01", "Error: invalid lockfile pin for x"),
            ("SQ-LOCK-02", "lockfile missing pin for actions/upload-artifact@bbbca2dd"),
            ("SQ-PIN-02", "line 3: unrecognised #-form"),
            ("SQ-RUN-01", "HttpError: API rate limit exceeded for 1.2.3.4"),
            ("SQ-GEN-01", "FAIL: committed REPOSITORY-MAP.adoc is stale"),
            ("SQ-GEN-02", "FAIL a.deed: line 9: line is 1200 octets"),
            ("SQ-GEN-02", "error K9-E006 too long"),
            (
                "SQ-PERM-01",
                "Resource not accessible by integration - https://docs.github.com/rest/actions/workflow-runs#get-a-workflow-run",
            ),
            ("SQ-CODE-01", "failed to run target/debug/build/x/out: No such file or directory"),
            ("SQ-REF-06", "fatal: could not read Username for 'https://github.com': terminal prompts disabled"),
            ("SQ-OWN-01", "Get Pages site failed: Not Found"),
            ("SQ-OWN-02", "Error: The ssh-private-key argument is empty."),
            ("SQ-OWN-04", "remote: error: GH013: Repository rule violations"),
            ("SQ-NOI-01", "Fork governance failed (deed=cancelled)"),
            ("SQ-NOI-02", "fatal: couldn't find remote ref refs/pull/12/merge"),
        ];
        for (id, log) in cases {
            assert!(ids(log, None).contains(id), "{id} should match {log:?}");
            assert!(!ids("all good, nothing to see", None).contains(id));
        }
        // Negatives that share words with a signature.
        assert!(!ids("line is fine", None).contains(&"SQ-GEN-02"));
        assert!(!ids("Resource not accessible by integration", None).contains(&"SQ-PERM-01"));
        assert!(!ids("No such file", None).contains(&"SQ-CODE-01"));
        assert!(!ids("deed=passed", None).contains(&"SQ-NOI-01"));
    }

    #[test]
    fn setup_bun_log_needs_the_step_name() {
        let log = "Failed to fetch url https://api.github.com/repos/oven-sh/bun/git/refs/tags (403) API rate limit exceeded";
        let with = ids(log, Some("Run oven-sh/setup-bun@abc"));
        assert!(with.contains(&"SQ-PIN-03") && with.contains(&"SQ-RUN-01"));
        let without = ids(log, Some("Install deps"));
        assert!(!without.contains(&"SQ-PIN-03") && without.contains(&"SQ-RUN-01"));
        assert!(!ids(log, None).contains(&"SQ-PIN-03"));
        assert!(!ids("fine", Some("setup-bun")).contains(&"SQ-PIN-03"));
    }

    const SHA: &str = "4d101475d8b20a2381f78447822ac1eab6504dd8";

    fn lock(key_sha: &str, ref_val: &str) -> String {
        format!(
            "version: 'v0.0.3'\nworkflows:\n    'a.yml':\n        - 'x/y@{key_sha}'\ndependencies:\n    'x/y@{key_sha}':\n        ref: '{ref_val}'\n        commit: 'sha1-{key_sha}'\n        owner_id: 1\n        uses:\n            - 'p/q@{SHA}'\n    'p/q@{SHA}':\n        ref: 'v1.0.0'\n        owner_id: 2\n"
        )
    }

    #[test]
    fn self_sha_flags_composite_and_spares_job_level_and_tags() {
        let l = lock(SHA, SHA);
        let flagged = self_sha_lock_records(&l, &[]);
        assert_eq!(flagged, vec![format!("x/y@{SHA}")]);
        // job-level caller exempts it; nested uses does not.
        assert!(self_sha_lock_records(&l, &[format!("X/Y@{SHA}")]).is_empty());
        // ref is a tag: fine.
        assert!(self_sha_lock_records(&lock(SHA, "v1.2.3"), &[]).is_empty());
    }

    #[test]
    fn self_sha_handles_uppercase_sha() {
        let up = SHA.to_ascii_uppercase();
        let l = lock(SHA, &up);
        assert_eq!(self_sha_lock_records(&l, &[]), vec![format!("x/y@{SHA}")]);
        let l2 = lock(&up, SHA);
        assert_eq!(self_sha_lock_records(&l2, &[]), vec![format!("x/y@{SHA}")]);
        assert!(self_sha_lock_records(&l2, &[format!("x/y@{SHA}")]).is_empty());
    }

    fn wf(with: &str) -> String {
        format!(
            "jobs:\n  a:\n    steps:\n      - uses: actions/checkout@v7\n        with:\n          bun-version: 9.9.9\n      - uses: oven-sh/setup-bun@abc # v2\n{with}      - run: bun install\n"
        )
    }

    #[test]
    fn setup_bun_static_detection() {
        // The unrelated checkout step's bun-version must not leak in.
        assert!(unpinned_setup_bun(&wf("")));
        assert!(unpinned_setup_bun(&wf(
            "        with:\n          bun-version: latest\n"
        )));
        assert!(unpinned_setup_bun(&wf(
            "        with:\n          bun-version: canary\n"
        )));
        assert!(unpinned_setup_bun(&wf(
            "        with:\n          bun-version: \"^1.4\"\n"
        )));
        assert!(unpinned_setup_bun(&wf(
            "        with:\n          bun-version: 1.x\n"
        )));
        assert!(unpinned_setup_bun(&wf(
            "        with:\n          bun-version-file: .bun-version\n"
        )));
        assert!(!unpinned_setup_bun(&wf(
            "        with:\n          bun-version: \"1.4.2\"\n"
        )));
        assert!(!unpinned_setup_bun(&wf(
            "        with:\n          bun-version: 1.4.2\n"
        )));
        assert!(!unpinned_setup_bun(
            "steps:\n  - uses: actions/checkout@v7\n"
        ));
        // name-first step form.
        let named = "steps:\n  - name: bun\n    uses: oven-sh/setup-bun@abc\n    with:\n      bun-version: 1.4.2\n";
        assert!(!unpinned_setup_bun(named));
    }

    const GOOD_DIFF: &str = "diff --git a/tests/a.rs b/tests/a.rs\n--- a/tests/a.rs\n+++ b/tests/a.rs\n@@ -1,2 +1,2 @@\n-    let p = \"x\";\n+    let p = PathBuf::from(env!(\"CARGO_BIN_EXE_squabble\"));\n     assert_eq!(1, 1);\n";

    fn on() -> GuardInput {
        GuardInput {
            autofix: Some("on".into()),
            ..GuardInput::default()
        }
    }

    #[test]
    fn every_decision_branch() {
        let g = |a, rr, pr, c| GuardInput {
            attempts_on_head: a,
            reruns_of_job: rr,
            open_fix_pr: pr,
            check_passed: c,
            ..on()
        };
        let fix = rule("SQ-GEN-01").unwrap();
        let act = rule("SQ-RUN-01").unwrap();
        let refer = rule("SQ-REF-06").unwrap();
        let owner = rule("SQ-OWN-01").unwrap();
        let noise = rule("SQ-NOI-01").unwrap();
        let none = GuardInput::default();

        assert_eq!(decide(owner, &none), Decision::OwnerReport);
        assert_eq!(decide(noise, &none), Decision::Label);
        assert!(matches!(
            decide(refer, &g(0, 0, false, None)),
            Decision::Refer { .. }
        ));

        for bad in [None, Some("off".to_string()), Some("ON".to_string())] {
            let gi = GuardInput {
                autofix: bad,
                ..GuardInput::default()
            };
            assert_eq!(
                decide(fix, &gi),
                Decision::ReportOnly {
                    reason: "ESTATE_AUTOFIX is not on".into()
                }
            );
            assert!(matches!(decide(act, &gi), Decision::ReportOnly { .. }));
        }
        assert!(matches!(
            decide(fix, &g(2, 0, false, None)),
            Decision::Refer { .. }
        ));
        assert_eq!(decide(fix, &g(1, 0, false, None)), Decision::Apply);
        assert!(matches!(
            decide(act, &g(0, 1, false, None)),
            Decision::Refer { .. }
        ));
        assert_eq!(decide(act, &g(5, 0, true, None)), Decision::Apply);
        assert_eq!(
            decide(fix, &g(0, 0, true, None)),
            Decision::ReportOnly {
                reason: "a fix PR is already open".into()
            }
        );
        assert!(matches!(
            decide(fix, &g(0, 0, false, Some(false))),
            Decision::Refer { .. }
        ));
        assert!(matches!(
            decide(act, &g(0, 0, false, Some(false))),
            Decision::Refer { .. }
        ));
        assert_eq!(decide(fix, &g(0, 0, false, Some(true))), Decision::Apply);
        assert_eq!(decide(fix, &g(0, 0, false, None)), Decision::Apply);
    }

    #[test]
    fn code_fix_decisions_use_the_diff() {
        let code = rule("SQ-CODE-01").unwrap();
        assert!(is_code_fix(code) && !is_code_fix(rule("SQ-GEN-01").unwrap()));
        // No diff: report only, never apply.
        assert_eq!(
            decide(code, &on()),
            Decision::ReportOnly {
                reason: "no diff to check".into()
            }
        );
        // Safe diff: apply (and the other guards still run).
        let safe = GuardInput {
            diff: Some(GOOD_DIFF.into()),
            ..on()
        };
        assert_eq!(decide(code, &safe), Decision::Apply);
        let failed = GuardInput {
            check_passed: Some(false),
            ..safe.clone()
        };
        assert!(matches!(decide(code, &failed), Decision::Refer { .. }));
        // Unsafe diff: refer with the reason.
        let bad = GuardInput {
            diff: Some("@@ -1 +1 @@\n-    assert!(ok);\n".into()),
            ..on()
        };
        match decide(code, &bad) {
            Decision::Refer { reason } => assert!(reason.contains("assert")),
            d => panic!("expected Refer, got {d:?}"),
        }
        // Kill switch still comes first.
        let off = GuardInput {
            diff: Some(GOOD_DIFF.into()),
            ..GuardInput::default()
        };
        assert!(matches!(decide(code, &off), Decision::ReportOnly { .. }));
        // A CI-config fix needs no diff.
        assert_eq!(decide(rule("SQ-GEN-01").unwrap(), &on()), Decision::Apply);
    }

    fn unsafe_diff(file: &str, line: &str) -> String {
        format!("--- a/{file}\n+++ b/{file}\n@@ -1 +1 @@\n{line}\n")
    }

    #[test]
    fn code_guard_accepts_a_clean_diff() {
        assert_eq!(code_fix_is_safe(GOOD_DIFF), Ok(()));
        assert_eq!(code_fix_is_safe(""), Ok(()));
        // Raising a threshold, deleting a non-test file, an unrelated baseline file.
        assert_eq!(
            code_fix_is_safe(&unsafe_diff(
                "ci.toml",
                "-min_coverage = 80\n+min_coverage = 85"
            )),
            Ok(())
        );
        assert_eq!(
            code_fix_is_safe("--- a/src/old.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-fn x() {}\n"),
            Ok(())
        );
        assert_eq!(
            code_fix_is_safe(&unsafe_diff("src/baseline.json", "+{}")),
            Ok(())
        );
        // An added assertion is fine.
        assert_eq!(
            code_fix_is_safe(&unsafe_diff("t.rs", "+    assert!(ok);")),
            Ok(())
        );
    }

    #[test]
    fn code_guard_rejects_each_unsafe_pattern() {
        let rejects: &[(&str, String)] = &[
            (
                "removed assert",
                unsafe_diff("t.rs", "-    assert_eq!(a, b);"),
            ),
            ("#[ignore]", unsafe_diff("t.rs", "+#[ignore]")),
            ("skip", unsafe_diff("t.js", "+  it.skip('x', () => {});")),
            ("skip word", unsafe_diff("t.py", "+@pytest.mark.skip")),
            ("t.Skip", unsafe_diff("t_test.go", "+\tt.Skip(\"later\")")),
            ("xit", unsafe_diff("t.js", "+  xit('x', () => {});")),
            (
                "continue-on-error",
                unsafe_diff("ci.yml", "+        continue-on-error: true"),
            ),
            ("|| true", unsafe_diff("ci.sh", "+cargo test || true")),
            (
                "threshold lowered",
                unsafe_diff(
                    "ci.toml",
                    "-coverage_threshold = 80\n+coverage_threshold = 60",
                ),
            ),
            (
                "min lowered",
                unsafe_diff("c.json", "-  \"min\": 10,\n+  \"min\": 5,"),
            ),
            (
                "baseline entry",
                unsafe_diff(".hypatia-baseline.json", "+  {\"id\": 1},"),
            ),
            (
                "hypatia-ignore",
                unsafe_diff("sub/.hypatia-ignore", "+src/"),
            ),
            (
                "hypatia allow",
                unsafe_diff("a.rs", "+let x = 1; // hypatia: allow a/b -- why"),
            ),
            ("nosemgrep", unsafe_diff("a.js", "+eval(x) // nosemgrep")),
            ("#[allow(", unsafe_diff("a.rs", "+#[allow(clippy::all)]")),
            (
                "test file deleted",
                "--- a/tests/a_test.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-fn t() {}\n".to_string(),
            ),
        ];
        for (name, d) in rejects {
            assert!(code_fix_is_safe(d).is_err(), "{name} should be rejected");
        }
        // The reason names the pattern.
        let e = code_fix_is_safe(&unsafe_diff("a.rs", "+#[allow(dead_code)]")).unwrap_err();
        assert!(e.contains("allow"));
        let e = code_fix_is_safe("--- a/tests/a_test.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n")
            .unwrap_err();
        assert!(e.contains("tests/a_test.rs"));
    }

    fn full_referral() -> Referral {
        Referral {
            rule_id: "SQ-REF-08".into(),
            problem: "PROB".into(),
            fix: Some("FIXIT".into()),
            risk: "RISKY".into(),
            repo: Some("o/r".into()),
            commit: Some("a".repeat(40)),
            run_url: Some("https://example.test/run/1".into()),
            job_url: Some("https://example.test/job/2".into()),
            workflow_file: Some(".github/workflows/x.yml".into()),
            job_name: Some("build".into()),
            step_name: Some("Run tests".into()),
            signature: Some("error: boom\nexit code 101".into()),
            toolchain: vec!["otp 28.3".into(), "bun 1.4.2".into()],
            tool_pins: vec!["hypatia=abc".into()],
            tried: Some("code guard refused the diff".into()),
            reproduces_on_rerun: Some(true),
            reproduces_on_base: Some(false),
        }
    }

    const LABELS: &[&str] = &[
        "Evidence:",
        "Rule:",
        "Repository:",
        "Commit:",
        "Run URL:",
        "Job URL:",
        "Workflow file:",
        "Job:",
        "Step:",
        "Signature:",
        "Toolchain:",
        "Tool pins:",
        "Tried and stopped because:",
        "Reproduces on re-run:",
        "Reproduces on base:",
    ];

    #[test]
    fn referral_message_forms() {
        let r = full_referral();
        for (reachable, opening) in [
            (
                true,
                "Referred to Hypatia. Not done yet: wait for its answer.",
            ),
            (
                false,
                "Hypatia is unreachable. This is a job for Hypatia; nothing was changed.",
            ),
        ] {
            let m = referral_message(&r, reachable);
            assert!(m.starts_with(opening));
            let (p, f, k, e) = (
                m.find("PROB"),
                m.find("FIXIT"),
                m.find("RISKY"),
                m.find("Evidence:"),
            );
            assert!(p.is_some() && f > p && k > f && e > k);
            for l in LABELS {
                assert!(m.contains(l), "missing label {l}");
            }
            assert!(!m.contains("unknown"));
            assert!(m.contains("error: boom") && m.contains("exit code 101"));
            assert!(m.contains("otp 28.3; bun 1.4.2") && m.contains("hypatia=abc"));
            assert!(
                m.contains("Reproduces on re-run: yes") && m.contains("Reproduces on base: no")
            );
        }
    }

    #[test]
    fn referral_message_prints_unknown_for_absent_fields() {
        let r = Referral {
            rule_id: "SQ-REF-06".into(),
            problem: "PROB".into(),
            risk: "RISKY".into(),
            ..Referral::default()
        };
        let m = referral_message(&r, true);
        for l in LABELS {
            assert!(m.contains(l), "missing label {l}");
        }
        assert!(m.contains("Fix: not known yet"));
        for l in [
            "Repository: unknown",
            "Commit: unknown",
            "Run URL: unknown",
            "Job URL: unknown",
            "Workflow file: unknown",
            "Job: unknown",
            "Step: unknown",
            "Signature: unknown",
            "Toolchain: unknown",
            "Tool pins: unknown",
            "Tried and stopped because: unknown",
            "Reproduces on re-run: unknown",
            "Reproduces on base: unknown",
        ] {
            assert!(m.contains(l), "missing {l}");
        }
    }

    #[test]
    fn fix_branch_is_lowercased() {
        assert_eq!(fix_branch("SQ-LOCK-01"), "squabbler/fix/sq-lock-01");
    }
}
