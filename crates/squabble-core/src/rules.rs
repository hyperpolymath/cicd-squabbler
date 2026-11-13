// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! The first-line rule catalogue (ADR 0004, increments 1 and 2).
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
//! SQ-OWN-08, SQ-OWN-11, SQ-OWN-12 and SQ-NOI-03. Their evidence is the
//! workflow source, run metadata or PR state, not a log line. SQ-LOCK-01,
//! SQ-PIN-03 and SQ-GEN-03 also have static detectors below.

use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

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

use RuleClass::{Act, Fix, Noise, Owner, Refer};

/// The catalogue is data, not code: one tab-separated row per rule, in ADR
/// 0004 order, with `#` comments. Adding a rule is a row there, not a change
/// here.
const CATALOGUE_TSV: &str = include_str!("../data/first-line-rules.tsv");

/// The 39 catalogue rows, in ADR 0004 order.
///
/// The data file ships inside the crate, so a malformed row is a defect in
/// this crate that a caller cannot repair. The catalogue never loads short:
/// it reports the row and exits with 70 (EX_SOFTWARE) instead. The shipped
/// file is checked by the `catalogue_shape` test.
pub static CATALOGUE: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    parse_catalogue(CATALOGUE_TSV).unwrap_or_else(|err| {
        eprintln!("squabble-core: the first-line rule catalogue is malformed: {err}");
        std::process::exit(70)
    })
});

/// Parse the catalogue data file. A bad row is an error, never skipped, so a
/// rule cannot go missing without notice.
fn parse_catalogue(tsv: &'static str) -> Result<Vec<Rule>, String> {
    let mut rules: Vec<Rule> = Vec::new();
    for (index, line) in tsv.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let rule = parse_row(index + 1, line)?;
        if rules.iter().any(|earlier| earlier.id == rule.id) {
            return Err(format!(
                "first-line-rules.tsv line {}: rule id {} appears twice",
                index + 1,
                rule.id
            ));
        }
        rules.push(rule);
    }
    Ok(rules)
}

/// One row: `id`, class, summary, fix, check, refer-when, tab-separated.
fn parse_row(lineno: usize, line: &'static str) -> Result<Rule, String> {
    let fields: Vec<&'static str> = line.split('\t').collect();
    let [id, class, summary, fix, check, refer_when] = fields[..] else {
        return Err(format!(
            "first-line-rules.tsv line {lineno}: expected 6 tab-separated fields, found {}",
            fields.len()
        ));
    };
    let class = match class {
        "FIX" => Fix,
        "ACT" => Act,
        "REFER" => Refer,
        "OWNER" => Owner,
        "NOISE" => Noise,
        other => {
            return Err(format!(
                "first-line-rules.tsv line {lineno}: unknown class {other:?}"
            ))
        }
    };
    for (name, value) in [
        ("id", id),
        ("summary", summary),
        ("fix", fix),
        ("check", check),
        ("refer-when", refer_when),
    ] {
        if value.trim().is_empty() {
            return Err(format!(
                "first-line-rules.tsv line {lineno}: {name} is empty"
            ));
        }
    }
    Ok(Rule {
        id,
        class,
        summary,
        fix,
        check,
        refer_when,
    })
}

/// Look a rule up by id.
pub fn rule(id: &str) -> Option<&'static Rule> {
    LazyLock::force(&CATALOGUE).iter().find(|r| r.id == id)
}

/// What a log matcher can see: the log text and whether the failed step is
/// setup-bun.
struct LogCtx<'a> {
    log: &'a str,
    setup_bun_step: bool,
}

impl LogCtx<'_> {
    fn has(&self, needle: &str) -> bool {
        self.log.contains(needle)
    }

    fn rate_limited(&self) -> bool {
        self.has("API rate limit exceeded")
    }

    /// A status of 500 to 599 right after `marker`, as in `gh: HTTP 502`.
    fn http_5xx_after(&self, marker: &str) -> bool {
        let bytes = self.log.as_bytes();
        self.log.match_indices(marker).any(|(at, _)| {
            let status = &bytes[at + marker.len()..];
            status.len() >= 3 && status[0] == b'5' && status[1..3].iter().all(u8::is_ascii_digit)
        })
    }
}

type LogMatcher = (&'static str, fn(&LogCtx) -> bool);

/// One row per rule that has a runtime log signature. The order is the
/// precedence: `match_log` returns ids in this order and the caller acts on
/// the first, so SQ-PIN-03 (the root cause of a setup-bun rate limit) comes
/// before SQ-RUN-01 (re-run).
static LOG_MATCHERS: &[LogMatcher] = &[
    ("SQ-LOCK-01", |c| c.has("invalid lockfile pin")),
    ("SQ-LOCK-02", |c| {
        c.has("lockfile missing pin for") || c.has("lockfile verification did not produce a result")
    }),
    ("SQ-PIN-02", |c| c.has("unrecognised #-form")),
    ("SQ-PIN-03", |c| {
        c.setup_bun_step
            && (c.rate_limited()
                || c.has(
                    "Failed to fetch url https://api.github.com/repos/oven-sh/bun/git/refs/tags",
                ))
    }),
    ("SQ-RUN-01", |c| c.rate_limited()),
    ("SQ-RUN-03", |c| {
        c.http_5xx_after("gh: HTTP ")
            || c.http_5xx_after("returned error: ")
            || c.has("Could not resolve host")
            || c.has("Failed to connect to")
    }),
    ("SQ-RUN-04", |c| {
        c.has("ambiguous argument") && c.has("unknown revision or path not in the working tree")
    }),
    ("SQ-GEN-01", |c| {
        c.has("committed REPOSITORY-MAP.adoc is stale")
    }),
    ("SQ-GEN-02", |c| {
        c.has("K9-E006") || (c.has("line is") && c.has("octets"))
    }),
    ("SQ-PERM-01", |c| {
        c.has("Resource not accessible by integration") && c.has("get-a-workflow-run")
    }),
    ("SQ-CODE-01", |c| {
        c.has("target/debug/build/") && c.has("No such file")
    }),
    ("SQ-REF-06", |c| {
        c.has("could not read Username for 'https://github.com'")
    }),
    ("SQ-OWN-01", |c| c.has("Get Pages site failed")),
    ("SQ-OWN-02", |c| {
        c.has("The ssh-private-key argument is empty")
    }),
    ("SQ-OWN-04", |c| c.has("GH013")),
    ("SQ-OWN-09", |c| {
        c.has("Bad credentials") || c.has("returned error: 401")
    }),
    ("SQ-OWN-10", |c| {
        c.has("Private Key is in OpenSSH format")
            || c.has("DECODER routines::unsupported")
            || c.has("Invalid keyData")
    }),
    ("SQ-NOI-01", |c| c.has("deed=cancelled")),
    ("SQ-NOI-02", |c| {
        c.has("couldn't find remote ref refs/pull/")
    }),
];

/// Rule ids whose runtime signature appears in a job log, in precedence
/// order. `step` is the name of the failed step, when known; SQ-PIN-03 needs
/// it to name setup-bun.
pub fn match_log(log: &str, step: Option<&str>) -> Vec<&'static str> {
    let ctx = LogCtx {
        log,
        setup_bun_step: step.is_some_and(|s| s.to_ascii_lowercase().contains("setup-bun")),
    };
    LOG_MATCHERS
        .iter()
        .filter(|(_, matches)| matches(&ctx))
        .map(|(id, _)| *id)
        .collect()
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

/// A job in a workflow file: its id and the lines under it.
struct JobBlock<'a> {
    id: &'a str,
    lines: Vec<&'a str>,
}

/// Split a workflow into its jobs by indentation. A line-based reader, like
/// the other static detectors here: it does not parse YAML anchors or flow
/// style, so an unusual layout is missed, never misread as a finding.
fn job_blocks(workflow_yaml: &str) -> Vec<JobBlock<'_>> {
    let mut blocks: Vec<JobBlock<'_>> = Vec::new();
    let mut in_jobs = false;
    let mut job_indent: Option<usize> = None;
    for line in workflow_yaml.lines() {
        let text = line.trim_start();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let indent = indent_of(line);
        if indent == 0 {
            in_jobs = text.trim_end() == "jobs:";
            continue;
        }
        if !in_jobs {
            continue;
        }
        let at_job_level = *job_indent.get_or_insert(indent) == indent;
        let key = unquote(text).strip_suffix(':');
        match (at_job_level, key, blocks.last_mut()) {
            (true, Some(id), _) => blocks.push(JobBlock {
                id,
                lines: Vec::new(),
            }),
            (false, _, Some(block)) => block.lines.push(line),
            _ => {}
        }
    }
    blocks
}

/// True when a job condition is exactly `always()`, bare or in `${{ }}`.
fn is_always(condition: &str) -> bool {
    let condition = unquote(condition);
    let inner = condition
        .strip_prefix("${{")
        .and_then(|rest| rest.strip_suffix("}}"))
        .map_or(condition, str::trim);
    inner == "always()"
}

/// Static SQ-GEN-03 detection (Hypatia WF029): ids of jobs that run with
/// `if: always()` after `needs:` and download an artifact (`uses:
/// actions/download-artifact`) without a `run-id`. A cancelled run still runs
/// such a job, which then fails on the artifact the cancelled jobs never
/// uploaded. Other `always()` jobs, such as a summary that downloads nothing,
/// are not flagged.
pub fn always_after_cancel(workflow_yaml: &str) -> Vec<String> {
    job_blocks(workflow_yaml)
        .into_iter()
        .filter(runs_after_cancel_and_downloads)
        .map(|job| job.id.to_string())
        .collect()
}

/// The values of a job's `name:` lines, one level inside the job.
fn field_values<'a>(job: &JobBlock<'a>, name: &str) -> Vec<&'a str> {
    job.lines
        .iter()
        .filter_map(|line| line.trim_start().strip_prefix(name))
        .collect()
}

fn runs_after_cancel_and_downloads(job: &JobBlock<'_>) -> bool {
    let always = field_values(job, "if:").into_iter().any(is_always);
    let needs_jobs = !field_values(job, "needs:").is_empty();
    let downloads = job
        .lines
        .iter()
        .any(|l| l.contains("uses:") && l.contains("actions/download-artifact"));
    let reads_other_run = !field_values(job, "run-id:").is_empty();
    always && needs_jobs && downloads && !reads_other_run
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
    fn catalogue_rows_carry_the_data_files_fields() {
        let lock = rule("SQ-LOCK-02").expect("SQ-LOCK-02 is in the catalogue");
        assert_eq!(lock.class, Fix);
        assert!(lock
            .summary
            .starts_with("lockfile missing pin for an action called inside another action"));
        assert!(lock.summary.contains("did not produce a result"));
        assert!(lock.summary.contains("stale lock"));
        assert_eq!(lock.fix, "Regenerate the lock with gh actions-lock");
        assert_eq!(lock.check, "The lock-sync gate passes and the job starts");
        assert_eq!(
            lock.refer_when,
            "The regenerated lock disagrees with the workflow YAML"
        );
        assert_eq!(CATALOGUE[0].id, "SQ-LOCK-01", "ADR 0004 order is kept");
        assert_eq!(CATALOGUE[CATALOGUE.len() - 1].id, "SQ-NOI-03");
        for r in CATALOGUE.iter() {
            assert!(!r.id.contains(char::is_whitespace), "{} has a space", r.id);
        }
    }

    #[test]
    fn parser_skips_comments_and_blank_lines() {
        let parsed = parse_catalogue(
            "# a comment\n\n   \n  # an indented comment\nSQ-X-01\tFIX\tsummary\tfix\tcheck\trefer\nSQ-X-02\tNOISE\ta\tb\tc\td\n",
        )
        .expect("two well-formed rows");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "SQ-X-01");
        assert_eq!(parsed[0].class, Fix);
        assert_eq!(parsed[1].class, Noise);
    }

    #[test]
    fn parser_rejects_a_short_row() {
        let err = parse_catalogue("SQ-X-01\tFIX\tsummary\tfix\tcheck\n").unwrap_err();
        assert!(err.contains("line 1"), "{err}");
        assert!(
            err.contains("expected 6 tab-separated fields, found 5"),
            "{err}"
        );
    }

    #[test]
    fn parser_rejects_an_unknown_class() {
        let err = parse_catalogue("SQ-X-01\tMAYBE\tsummary\tfix\tcheck\trefer\n").unwrap_err();
        assert!(err.contains("unknown class"), "{err}");
    }

    #[test]
    fn parser_rejects_an_empty_field() {
        let err = parse_catalogue("SQ-X-01\tFIX\tsummary\tfix\tcheck\t \n").unwrap_err();
        assert!(err.contains("refer-when is empty"), "{err}");
    }

    #[test]
    fn parser_rejects_a_duplicate_id() {
        let err =
            parse_catalogue("SQ-X-01\tFIX\ta\tb\tc\td\nSQ-X-01\tACT\ta\tb\tc\td\n").unwrap_err();
        assert!(err.contains("appears twice"), "{err}");
    }

    #[test]
    fn shipped_catalogue_file_parses() {
        let parsed = parse_catalogue(CATALOGUE_TSV).expect("the shipped data file is well formed");
        assert_eq!(parsed.len(), 39);
    }

    #[test]
    fn catalogue_shape() {
        assert_eq!(CATALOGUE.len(), 39);
        let uniq: HashSet<_> = CATALOGUE.iter().map(|r| r.id).collect();
        assert_eq!(uniq.len(), 39);
        let n = |c| CATALOGUE.iter().filter(|r| r.class == c).count();
        assert_eq!(n(Fix), 18);
        assert_eq!(n(Act), 4);
        assert_eq!(n(Refer), 2);
        assert_eq!(n(Owner), 12);
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
            // Real signatures from hypatia runs 43137798791, 43358533401,
            // 43158586027, 43367835718 and 43361512802, and the canary.
            (
                "SQ-LOCK-02",
                "##[error]lockfile verification did not produce a result for this action. Unable to resolve action `dtolnay/rust-toolchain@v1`",
            ),
            ("SQ-RUN-03", "gh: HTTP 502 (https://api.github.com/graphql)"),
            ("SQ-RUN-03", "fatal: unable to access 'https://git.sr.ht/x': The requested URL returned error: 503"),
            ("SQ-RUN-03", "fatal: unable to access 'https://x/': Could not resolve host: x"),
            (
                "SQ-RUN-04",
                "ERR [git] fatal: ambiguous argument '4190136^..bf33aeb': unknown revision or path not in the working tree.",
            ),
            ("SQ-OWN-09", "gh: Bad credentials (HTTP 401)"),
            ("SQ-OWN-09", "fatal: unable to access 'https://x/': The requested URL returned error: 401"),
            ("SQ-OWN-10", "Error: Private Key is in OpenSSH format"),
            ("SQ-OWN-10", "error:1E08010C:DECODER routines::unsupported"),
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
    fn status_codes_are_matched_by_class_not_by_prefix() {
        for log in [
            "gh: HTTP 404",
            "gh: HTTP 5",
            "gh: HTTP 5x2",
            "returned error: 403",
        ] {
            assert!(!ids(log, None).contains(&"SQ-RUN-03"), "{log}");
        }
        // 403 is a rate limit or a missing permission, never a bad token here.
        assert!(!ids("returned error: 403", None).contains(&"SQ-OWN-09"));
        // The status must follow a known marker; a bare HTTP 599 is not enough.
        assert!(ids("HTTP 599 from somewhere", None).is_empty());
        assert_eq!(ids("gh: HTTP 599", None), vec!["SQ-RUN-03"]);
    }

    #[test]
    fn matcher_order_is_the_precedence() {
        let log = "Failed to fetch url https://api.github.com/repos/oven-sh/bun/git/refs/tags API rate limit exceeded";
        assert_eq!(ids(log, Some("setup-bun")), vec!["SQ-PIN-03", "SQ-RUN-01"]);
        assert_eq!(ids(log, Some("checkout")), vec!["SQ-RUN-01"]);
        // Every matcher names a catalogue rule, and none is listed twice.
        let matched: Vec<&str> = LOG_MATCHERS.iter().map(|(id, _)| *id).collect();
        let uniq: HashSet<_> = matched.iter().collect();
        assert_eq!(uniq.len(), matched.len());
        assert!(matched.iter().all(|id| rule(id).is_some()));
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

    fn gate(job_if: &str, extra: &str) -> String {
        format!(
            "name: g\njobs:\n  scan:\n    runs-on: x\n    steps:\n      - run: true\n  deposit:\n    needs: [scan]\n    {job_if}\n    steps:\n      - uses: actions/download-artifact@v8.0.2\n{extra}"
        )
    }

    #[test]
    fn always_after_cancel_flags_the_download_after_needs() {
        for cond in [
            "if: always()",
            "if: ${{ always() }}",
            "if: \"always()\"",
            "if: always() # keep",
        ] {
            assert_eq!(
                always_after_cancel(&gate(cond, "")),
                vec!["deposit"],
                "{cond}"
            );
        }
    }

    #[test]
    fn always_after_cancel_spares_what_is_not_the_defect() {
        for (cond, extra) in [
            ("if: ${{ !cancelled() }}", ""),
            ("if: always() && github.event_name == 'push'", ""),
            ("if: always()", "        with:\n          run-id: 7\n"),
        ] {
            assert!(always_after_cancel(&gate(cond, extra)).is_empty(), "{cond}");
        }
        // always() with needs but nothing downloaded: a summary job.
        let summary = "jobs:\n  a:\n    steps:\n      - run: true\n  sum:\n    needs: [a]\n    if: always()\n    steps:\n      - uses: actions/checkout@v7\n";
        assert!(always_after_cancel(summary).is_empty());
        // always() with a download but no needs: nothing was cancelled before it.
        let alone = "jobs:\n  d:\n    if: always()\n    steps:\n      - uses: actions/download-artifact@v8\n";
        assert!(always_after_cancel(alone).is_empty());
        assert!(always_after_cancel("name: x\non: push\n").is_empty());
    }

    #[test]
    fn always_after_cancel_reads_each_job_on_its_own() {
        let yaml = format!(
            "{}  other:\n    needs: [scan]\n    if: always()\n    steps:\n      - run: true\n",
            gate("if: always()", "")
        );
        assert_eq!(always_after_cancel(&yaml), vec!["deposit"]);
    }
}
