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
//! SQ-OWN-08, SQ-OWN-11, SQ-OWN-12, SQ-OWN-16 and SQ-NOI-03. Their evidence is the
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

/// The 47 catalogue rows, in ADR 0004 order.
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

    /// The host name right after `marker`, taken from the rest of that line
    /// only (`Could not resolve host: git.sr.ht`), with trailing punctuation
    /// removed. It is empty when the line ends after the marker, so a name on
    /// the next log line is never read.
    fn host_names_after<'s>(&'s self, marker: &'s str) -> impl Iterator<Item = &'s str> + 's {
        self.log.match_indices(marker).map(move |(at, _)| {
            let rest = &self.log[at + marker.len()..];
            let line = rest.lines().next().unwrap_or("");
            let name = line.split_whitespace().next().unwrap_or("");
            name.trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
        })
    }

    /// A host name that contains a dot after `marker`. A name without a dot
    /// (a service container, `localhost`) is a configuration error, not a
    /// transient one.
    fn dotted_name_after(&self, marker: &str) -> bool {
        self.host_names_after(marker).any(|n| n.contains('.'))
    }

    /// A host error after `marker` whose name has no dot, or is missing.
    fn undotted_name_after(&self, marker: &str) -> bool {
        self.host_names_after(marker).any(|n| !n.contains('.'))
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
        c.has("lockfile missing pin for")
            || c.has("lockfile verification did not produce a result")
            || c.has("refs missing from the lockfile")
    }),
    ("SQ-PIN-02", |c| c.has("unrecognised #-form")),
    ("SQ-PIN-03", |c| {
        c.setup_bun_step
            && (c.rate_limited()
                || c.has(
                    "Failed to fetch url https://api.github.com/repos/oven-sh/bun/git/refs/tags",
                ))
    }),
    ("SQ-TOOL-02", |c| {
        c.has("Dependabot detected that Bun is misconfigured")
    }),
    ("SQ-RUN-01", |c| c.rate_limited()),
    ("SQ-RUN-03", |c| {
        c.http_5xx_after("gh: HTTP ")
            || c.http_5xx_after("gh: Server Error (HTTP ")
            || c.http_5xx_after("returned error: ")
            || c.dotted_name_after("Could not resolve host: ")
            || c.dotted_name_after("Failed to connect to ")
    }),
    // A host error whose name has no dot is a referral, never a re-run and
    // never silence (no-silent-skip).
    ("SQ-RUN-05", |c| {
        c.undotted_name_after("Could not resolve host: ")
            || c.undotted_name_after("Failed to connect to ")
    }),
    // The gate failed closed because it could not read GitHub state. Only the
    // unread lines match: a DROPPED CLOSURE line alone is a real drop, and
    // the ones that follow an unread callee are a consequence of it.
    ("SQ-RUN-06", |c| {
        c.has("could not read branch rulesets")
            || c.has("could not read the workflow catalogue")
            || c.has("(offline with no cache entry, or the API refused)")
    }),
    ("SQ-RUN-04", |c| {
        c.has("ambiguous argument") && c.has("unknown revision or path not in the working tree")
    }),
    ("SQ-GEN-01", |c| {
        c.has("committed REPOSITORY-MAP.adoc is stale")
    }),
    ("SQ-GEN-04", |c| {
        c.has("kyaml-format:") && c.has("is not KYAML")
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
    ("SQ-OWN-13", |c| {
        c.has("gh: Repository rule violations found")
    }),
    ("SQ-OWN-14", |c| {
        c.has("non-v8 UUID literal") || c.has("UUID check failed. Use ESTATE-UUID-V8")
    }),
    ("SQ-OWN-15", |c| {
        c.has("Not authorized or organization not found")
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
        if current.as_ref().is_some_and(|(_, key_ind)| ind <= *key_ind) {
            current = None;
        }
        match &current {
            None => current = self_sha_record_key(t).map(|key| (key, ind)),
            // Direct children only: a nested `uses:` list never carries `ref:`.
            Some((key, key_ind)) if ind > *key_ind => {
                flag_if_self_ref(key, t, &callers, &mut flagged);
            }
            Some(_) => {}
        }
    }
    flagged
}

/// The lowercased `owner/repo@<40-hex>` key of a lock record opened by this
/// line, or None for any other line (a list item, or a key of another shape).
fn self_sha_record_key(trimmed: &str) -> Option<String> {
    if trimmed.starts_with('-') {
        return None;
    }
    let key = trimmed
        .strip_suffix(':')?
        .trim()
        .trim_matches(|c| c == '\'' || c == '"');
    let (repo, sha) = key.split_once('@')?;
    (repo.contains('/') && is_sha40(sha))
        .then(|| format!("{}@{}", repo.to_ascii_lowercase(), sha.to_ascii_lowercase()))
}

/// Record KEY in `flagged` when this `ref:` line repeats the SHA in the key
/// and no job-level reusable caller names the key.
fn flag_if_self_ref(key: &str, trimmed: &str, callers: &[String], flagged: &mut Vec<String>) {
    let Some(value) = trimmed.strip_prefix("ref:") else {
        return;
    };
    let sha = key.split_once('@').map_or("", |(_, s)| s);
    let is_caller = callers.iter().any(|c| c == key);
    let already = flagged.iter().any(|f| f == key);
    if unquote(value).eq_ignore_ascii_case(sha) && !is_caller && !already {
        flagged.push(key.to_string());
    }
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
    lines.iter().enumerate().any(|(i, line)| {
        let Some((key_ind, opens_item)) = setup_bun_uses(line) else {
            return false;
        };
        let start = if opens_item {
            i
        } else {
            step_start(&lines, i, key_ind)
        };
        let end = step_end(&lines, i, key_ind);
        setup_bun_step_unpinned(&lines[start..end])
    })
}

/// For a `uses: oven-sh/setup-bun@…` line: the indent of its keys and whether
/// the line opens a `- ` list item. None for any other line.
fn setup_bun_uses(line: &str) -> Option<(usize, bool)> {
    let t = line.trim_start();
    if t.starts_with('#') {
        return None;
    }
    let (opens_item, body) = match t.strip_prefix("- ") {
        Some(b) => (true, b.trim_start()),
        None => (false, t),
    };
    if !body.starts_with("uses:") || !body.contains("oven-sh/setup-bun@") {
        return None;
    }
    let key_ind = indent_of(line) + if opens_item { t.len() - body.len() } else { 0 };
    Some((key_ind, opens_item))
}

/// First line of the step that holds line `i`, walking up past blank lines
/// and sibling keys to the `- ` that opens it.
fn step_start(lines: &[&str], i: usize, key_ind: usize) -> usize {
    let mut start = i;
    while start > 0 {
        let prev = lines[start - 1];
        if !prev.trim().is_empty() && indent_of(prev) < key_ind {
            if prev.trim_start().starts_with("- ") {
                start -= 1;
            }
            break;
        }
        start -= 1;
    }
    start
}

/// One past the last line of the step that holds line `i`.
fn step_end(lines: &[&str], i: usize, key_ind: usize) -> usize {
    let mut end = i + 1;
    while end < lines.len() && (lines[end].trim().is_empty() || indent_of(lines[end]) >= key_ind) {
        end += 1;
    }
    end
}

/// True when the step has `bun-version-file`, no `bun-version`, or a
/// `bun-version` that is empty, `latest`, `canary` or a range.
fn setup_bun_step_unpinned(step: &[&str]) -> bool {
    let mut version: Option<&str> = None;
    let mut uses_file = false;
    for l in step {
        let lt = l.trim_start();
        let lt = lt.strip_prefix("- ").unwrap_or(lt);
        if lt.starts_with("bun-version-file:") {
            uses_file = true;
        } else if let Some(v) = lt.strip_prefix("bun-version:") {
            version = Some(unquote(v));
        }
    }
    uses_file || version.is_none_or(bun_version_is_loose)
}

fn bun_version_is_loose(version: &str) -> bool {
    let lv = version.to_ascii_lowercase();
    lv.is_empty() || lv == "latest" || lv == "canary" || lv.contains(['^', '~', '>', '<', '*', 'x'])
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
    let start = b.iter().position(u8::is_ascii_digit)?;
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
    let mut scan = DiffScan::default();
    for line in diff.lines() {
        if scan.file_header(line)? {
            continue;
        }
        if line.starts_with("@@") {
            scan.end_hunk()?;
        } else if let Some(body) = line.strip_prefix('-') {
            scan.removed_line(body)?;
        } else if let Some(body) = line.strip_prefix('+') {
            scan.added_line(body)?;
        }
    }
    scan.end_hunk()
}

const LOWERED_THRESHOLD: &str = "the diff lowers a threshold, minimum, maximum or coverage value";

/// Added lines that weaken a check, with the reason each gives.
const BANNED_ADDITIONS: &[(&str, &str)] = &[
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

/// What the code-fix guard has seen so far in a unified diff: the file being
/// edited, the file it replaced, and the threshold lines of the current hunk.
#[derive(Default)]
struct DiffScan {
    file: String,
    old_file: String,
    removed_thr: Vec<(String, f64)>,
    added_thr: Vec<(String, f64)>,
}

impl DiffScan {
    /// Read a `---` or `+++` file header. `Ok(true)` when the line was one;
    /// `Err` when the new side is /dev/null for a test file (a deletion).
    fn file_header(&mut self, line: &str) -> Result<bool, String> {
        if let Some(p) = line.strip_prefix("--- ") {
            if line.starts_with("--- a/") || line == "--- /dev/null" {
                self.old_file = p.strip_prefix("a/").unwrap_or(p).to_string();
                return Ok(true);
            }
        }
        if let Some(p) = line.strip_prefix("+++ ") {
            if line.starts_with("+++ b/") || line == "+++ /dev/null" {
                if p != "/dev/null" {
                    self.file = p.strip_prefix("b/").unwrap_or(p).to_string();
                } else if is_test_path(&self.old_file) {
                    return Err(format!("the diff deletes the test file {}", self.old_file));
                } else {
                    self.file.clear();
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// A threshold lowered within the hunk that just ended is unsafe.
    fn end_hunk(&mut self) -> Result<(), String> {
        let lowered = self
            .added_thr
            .iter()
            .any(|(k, n)| self.removed_thr.iter().any(|(rk, rn)| rk == k && n < rn));
        self.removed_thr.clear();
        self.added_thr.clear();
        if lowered {
            Err(LOWERED_THRESHOLD.into())
        } else {
            Ok(())
        }
    }

    fn removed_line(&mut self, body: &str) -> Result<(), String> {
        if body.contains("assert") {
            return Err("the diff removes a line containing `assert`".into());
        }
        if let Some(entry) = threshold_entry(body) {
            self.removed_thr.push(entry);
        }
        Ok(())
    }

    fn added_line(&mut self, body: &str) -> Result<(), String> {
        let base = self.file.rsplit('/').next().unwrap_or("");
        if base == ".hypatia-baseline.json" || base == ".hypatia-ignore" {
            return Err(format!("the diff adds a line to {base}"));
        }
        let lower = body.to_ascii_lowercase();
        if let Some((_, why)) = BANNED_ADDITIONS.iter().find(|(pat, _)| lower.contains(pat)) {
            return Err(format!("the diff {why}"));
        }
        if let Some(entry) = threshold_entry(body) {
            self.added_thr.push(entry);
        }
        Ok(())
    }
}

/// The (digit-free text, value) pair of a threshold, minimum, maximum or
/// coverage line, or None for any other line.
fn threshold_entry(body: &str) -> Option<(String, f64)> {
    if !is_threshold_line(body) {
        return None;
    }
    first_number(body).map(|n| (without_digits(body), n))
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
        assert!(lock.summary.contains("refs missing from the lockfile"));
        assert!(lock
            .refer_when
            .starts_with("The regenerated lock disagrees with the workflow YAML"));
        assert!(lock.refer_when.contains("slsa-github-generator"));
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
        assert_eq!(parsed.len(), 47);
    }

    #[test]
    fn catalogue_shape() {
        assert_eq!(CATALOGUE.len(), 47);
        let uniq: HashSet<_> = CATALOGUE.iter().map(|r| r.id).collect();
        assert_eq!(uniq.len(), 47);
        let n = |c| CATALOGUE.iter().filter(|r| r.class == c).count();
        assert_eq!(n(Fix), 20);
        assert_eq!(n(Act), 5);
        assert_eq!(n(Refer), 3);
        assert_eq!(n(Owner), 16);
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
        assert_eq!(refs, vec!["SQ-RUN-05", "SQ-REF-06", "SQ-REF-08"]);
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
            // standards run 43446385277, Rust coverage (playbook-to-recipe).
            ("SQ-RUN-03", "gh: Server Error (HTTP 502)"),
            ("SQ-RUN-03", "fatal: unable to access 'https://git.sr.ht/x': The requested URL returned error: 503"),
            ("SQ-RUN-03", "fatal: unable to access 'https://git.sr.ht/x': Could not resolve host: git.sr.ht"),
            ("SQ-RUN-03", "curl: (7) Failed to connect to github.com port 443 after 21 ms"),
            // standards runs 43458638630, 43458639621 and 43459052870 (the
            // Actions token budget was spent), read from the job logs.
            (
                "SQ-RUN-06",
                "##[notice]could not read branch rulesets for metadatastician/standards (API error, rate limit, or missing permission) \u{2014} nothing inspected",
            ),
            (
                "SQ-RUN-06",
                "##[error]could not read the workflow catalogue \u{2014} enablement is UNKNOWN, not off",
            ),
            (
                "SQ-RUN-06",
                "##[error]metadatastician/standards@d55304a01b5260b0511aae0d14ed8b8770b00293: the called workflow .github/workflows/rust-ci-reusable.yml could not be read at d55304a01b5260b0511aae0d14ed8b8770b00293 (offline with no cache entry, or the API refused). An unread callee is NOT a closed closure",
            ),
            (
                "SQ-RUN-04",
                "ERR [git] fatal: ambiguous argument '4190136^..bf33aeb': unknown revision or path not in the working tree.",
            ),
            ("SQ-OWN-09", "gh: Bad credentials (HTTP 401)"),
            ("SQ-OWN-09", "fatal: unable to access 'https://x/': The requested URL returned error: 401"),
            ("SQ-OWN-10", "Error: Private Key is in OpenSSH format"),
            ("SQ-OWN-10", "error:1E08010C:DECODER routines::unsupported"),
            // this-ssg run 43156768667 and MetaManifold-WebUI run 43225380901.
            (
                "SQ-LOCK-02",
                "FAIL .github/workflows/release.yml: refs missing from the lockfile: slsa-framework/slsa-github-generator@f7dd8c54c2067bafc12ca7a55595d5ee9b75204a",
            ),
            (
                "SQ-TOOL-02",
                "Error during file fetching; aborting: Dependabot detected that Bun is misconfigured in this repository. Running `bun` results in the following error: This project has a `bun.lock` file but no `package-lock.json`",
            ),
            // quintessence run 43156792336, Apodeixis.jl runs 43257204071 and
            // 43257204302, lithoglyph run 43152462261.
            (
                "SQ-GEN-04",
                "##[error]kyaml-format: build/container/compose.yaml is not KYAML \u{2014} run scripts/kyaml-format.sh build/container/compose.yaml",
            ),
            (
                "SQ-OWN-13",
                "gh: Repository rule violations found\nCode coverage checks require a pull request.\n (HTTP 409)",
            ),
            ("SQ-OWN-14", "./Project.toml: non-v8 UUID literal (<uuid>)"),
            (
                "SQ-OWN-15",
                "ERROR Fail to load the list of plugins installed on the server: Not authorized or organization not found",
            ),
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
            "gh: Server Error (HTTP 404)",
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
    fn only_dotted_host_names_count_as_transient() {
        for log in [
            "Could not resolve host: db",
            "curl: (6) Could not resolve host: localhost",
            "Failed to connect to localhost port 5432",
            "Could not resolve host",
            "Could not resolve host: ",
            // The name must come from the error line, not the next one.
            "Could not resolve host: \n./scripts/check.sh: configuration error",
            "Failed to connect to\nexample.com says no",
        ] {
            assert!(!ids(log, None).contains(&"SQ-RUN-03"), "{log}");
        }
        assert!(ids("Could not resolve host: api.github.com.", None).contains(&"SQ-RUN-03"));
        assert!(!ids("Could not resolve host: api.github.com.", None).contains(&"SQ-RUN-05"));
    }

    #[test]
    fn undotted_host_errors_are_referred_not_silent() {
        for log in [
            "Could not resolve host: db",
            "curl: (6) Could not resolve host: localhost",
            "Failed to connect to localhost port 5432",
            "Could not resolve host: \n./scripts/check.sh: configuration error",
        ] {
            assert_eq!(ids(log, None), vec!["SQ-RUN-05"], "{log}");
        }
        assert_eq!(rule("SQ-RUN-05").unwrap().class, Refer);
    }

    #[test]
    fn unreadable_github_state_is_a_rerun_but_a_real_drop_is_not() {
        let consequence = "DROPPED CLOSURE \u{2014} the reviewed baseline has a -> b, and this source no longer reaches it.";
        // A real drop, with nothing unread, is the gate's own finding.
        assert!(ids(consequence, None).is_empty());
        // Unread callee plus the drops that follow it: one re-run, then the owner.
        let both = format!(
            "the called workflow x.yml could not be read at abc (offline with no cache entry, or the API refused).\n{consequence}"
        );
        assert_eq!(ids(&both, None), vec!["SQ-RUN-06"]);
        // A git credential prompt is not an unread gate (SQ-REF-06).
        let git = "fatal: could not read Username for 'https://github.com'";
        assert!(!ids(git, None).contains(&"SQ-RUN-06"));
        assert_eq!(rule("SQ-RUN-06").unwrap().class, Act);
    }

    #[test]
    fn push_refusals_are_told_apart() {
        // The git form is SQ-OWN-04, the gh API form is SQ-OWN-13; neither claims the other.
        let git = "remote: error: GH013: Repository rule violations found for refs/heads/main.";
        let api = "gh: Repository rule violations found\n (HTTP 409)";
        assert_eq!(ids(git, None), vec!["SQ-OWN-04"]);
        assert_eq!(ids(api, None), vec!["SQ-OWN-13"]);
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
