// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Workflow ground-truthing — "solutions at source" for check classification.
//!
//! Doctrine #14 says fix the canonical origin, not the downstream symptom. To
//! do that for a red gate the squabbler must know *where each required check
//! comes from*: which workflow emits it, whether that workflow delegates to a
//! reusable workflow owned in another repo, whether it is a path-filtered gate
//! that strands PRs, and whether its job is CI-configuration (the squabbler's
//! lane) or a code/build/scan job (out of lane → escalate).
//!
//! This is a deliberately *tolerant, line-based* scan of `.github/workflows/`,
//! not a full YAML engine: it needs only a handful of facts and must never
//! error on a shape it doesn't recognise. Where it cannot determine a fact it
//! returns `None`/`false` and the fight falls back to a conservative move —
//! it never guesses a classification it cannot ground.

use squabble_core::gate::{CheckRun, RequiredCheck};
use squabble_core::moves::{EscalationKind, ExpertGroup, Move, OwnershipDisposition};
use std::path::Path;

/// What kind of work a workflow's failing job represents — decides lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowKind {
    /// Lints CI/workflow configuration itself — the squabbler's own lane.
    Hygiene,
    /// Builds/compiles/lints application code or containers — out of lane.
    CodeBuild,
    /// Scans for secrets / vulnerabilities — out of lane (security expert).
    Security,
    /// Formal-verification / proof obligations — out of lane (proof expert).
    Proof,
    /// Nothing recognised.
    Other,
}

/// The facts extracted from one workflow file.
#[derive(Debug, Clone)]
pub struct WorkflowInfo {
    pub file: String,
    pub name: Option<String>,
    pub job_ids: Vec<String>,
    pub job_names: Vec<String>,
    /// `owner/repo` of every reusable workflow this file `uses:`.
    pub reusable_repos: Vec<String>,
    /// Every external (non-local) `uses:` ref, normalised to `owner/repo@ref`
    /// with any subpath collapsed — the same key shape as the actions lockfile
    /// and the estate allowlist. Input to the Actions-policy probes (issue
    /// #15): the mode-1 classification matches these against
    /// `patterns_allowed`.
    pub external_uses: Vec<String>,
    /// The subset of `external_uses` pinned to a tag or branch rather than a
    /// 40-hex SHA. Under `sha_pinning_required=true` these refuse to start —
    /// the mode-2 Actions-policy `startup_failure` (issue #15).
    pub tag_pinned_uses: Vec<String>,
    /// True if the file declares an `on.*.paths` trigger filter.
    pub path_filtered: bool,
    /// Executable policy contradicts the canonical descriptile location.
    pub retired_descriptile_policy: bool,
    /// A bare jobs block contains only whitespace or commented examples.
    pub empty_jobs: bool,
    pub kind: WorkflowKind,
}

/// The repository's live Actions permissions posture — the *why* behind an
/// Actions-policy `startup_failure`, fetched by the host from
/// `repos/{o}/{r}/actions/permissions` (+ `.../selected-actions`) when a
/// required context resolves to `STARTUP_FAILURE` (issue #15). Plain host
/// data: carrying it here keeps `squabble-core` estate-free.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionsPolicyFacts {
    /// `all`, `local_only`, or `selected`. Empty string = unknown.
    pub allowed_actions: String,
    /// Whether GitHub requires every `uses:` pinned to a full-length SHA.
    pub sha_pinning_required: bool,
    /// `github_owned_allowed` from the selected-actions endpoint.
    pub github_owned_allowed: bool,
    /// `patterns_allowed` (meaningful only when `allowed_actions == selected`).
    pub patterns_allowed: Vec<String>,
}

impl ActionsPolicyFacts {
    /// `allowed_actions=selected` — the mode-1 posture.
    pub fn is_selected(&self) -> bool {
        self.allowed_actions == "selected"
    }

    /// Is `owner_repo` (normalised `owner/repo`, no `@ref`) permitted by a
    /// `selected` posture? Mirrors the estate preflight
    /// (`standards` `scripts/check-allowed-actions.sh`): GitHub-owned
    /// `actions/*` and `github/*` pass when `github_owned_allowed`; a pattern
    /// covers when it is an exact `owner/repo`, an owner-wide `owner/*`, or a
    /// trailing-`*` prefix form such as `r-lib/*`.
    ///
    /// Deliberately NOT consulted when the posture is not `selected` — under
    /// `all` nothing is blocked; under `local_only` *everything* external is
    /// blocked but that is out of issue #15's scope, so callers gate on
    /// [`Self::is_selected`] first.
    pub fn covers(&self, owner_repo: &str) -> bool {
        let owner = owner_repo.split('/').next().unwrap_or("");
        if self.github_owned_allowed && matches!(owner, "actions" | "github") {
            return true;
        }
        self.patterns_allowed.iter().any(|p| {
            let base = p.split('@').next().unwrap_or(p.as_str());
            base == owner_repo
                || base == format!("{owner}/*")
                || base
                    .strip_suffix('*')
                    .is_some_and(|prefix| !prefix.is_empty() && owner_repo.starts_with(prefix))
        })
    }

    /// The first `external_uses` entry (normalised `owner/repo@ref`) this
    /// posture does NOT cover, as `owner/repo` — the minimal unit a
    /// `ReconcileActionsPolicy` move must unblock. `None` under any posture
    /// other than `selected`, or when everything used is covered.
    pub fn first_uncovered<'a>(&self, external_uses: &'a [String]) -> Option<&'a str> {
        if !self.is_selected() {
            return None;
        }
        external_uses.iter().find_map(|u| {
            let owner_repo = u.split('@').next().unwrap_or(u.as_str());
            (!self.covers(owner_repo)).then_some(owner_repo)
        })
    }
}

impl WorkflowInfo {
    /// Does this workflow emit the given check context? For reusable-workflow
    /// jobs GitHub reports `<caller-job-id> / <reusable-job-name>`, so a check
    /// containing " / " is matched on its prefix (the caller job id); a plain
    /// check is matched against job ids and job display names.
    fn emits(&self, check: &str) -> bool {
        let target = check.split(" / ").next().unwrap_or(check).trim();
        self.job_ids.iter().any(|j| j == target)
            || self.job_names.iter().any(|n| n == target)
            || self.name.as_deref() == Some(target)
    }
}

/// The whole workflow directory, ground-truthed.
#[derive(Debug, Clone, Default)]
pub struct WorkflowFacts {
    pub workflows: Vec<WorkflowInfo>,
}

impl WorkflowFacts {
    /// Scan `<repo_root>/.github/workflows/*.{yml,yaml}`. Never errors: an
    /// unreadable dir or file is simply skipped.
    pub fn load(repo_root: &Path) -> Self {
        let dir = repo_root.join(".github/workflows");
        let mut workflows = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut paths: Vec<_> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    matches!(
                        p.extension().and_then(|e| e.to_str()),
                        Some("yml") | Some("yaml")
                    )
                })
                .collect();
            paths.sort();
            for p in paths {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    let file = p
                        .file_name()
                        .and_then(|f| f.to_str())
                        .unwrap_or("")
                        .to_string();
                    workflows.push(parse_workflow(&file, &text));
                }
            }
        }
        WorkflowFacts { workflows }
    }

    fn find_emitting(&self, check: &str) -> Option<&WorkflowInfo> {
        self.workflows.iter().find(|w| w.emits(check))
    }

    /// Classify one unsatisfied required check into the single most defensible
    /// [`Move`], using the emitting workflow's facts. Returns `None` when no
    /// workflow could be attributed — the caller then falls back to the pure
    /// engine's conservative default rather than guessing.
    ///
    /// Workflows marked as using a retired descriptile policy or lacking
    /// uncommented job definitions are flagged as non-functional before
    /// ownership and lane classification.
    ///
    /// `slug` is the current repo's `owner/repo`; a reusable workflow whose
    /// `owner/repo` differs is owned upstream. The check's realised [`CheckRun`]
    /// matters: the path-filter trap only manifests as a *Missing* check (the
    /// workflow never triggered off-path), so the appliable pass-through move is
    /// proposed only then — a check that actually ran and *Failed* is a
    /// different problem the filter cannot explain.
    ///
    /// A workflow that checks a retired descriptile path, or has only commented
    /// jobs, is classified as a non-functional gate regardless of [`CheckRun`].
    pub fn classify(&self, check: &RequiredCheck, slug: &str) -> Option<Move> {
        self.classify_with_policy(check, slug, None, false)
    }

    /// As [`Self::classify`], but with the Actions-policy why-probe (issue #15)
    /// consulted first for a check whose realised run was a `STARTUP_FAILURE`.
    ///
    /// `startup_failed` must be true only when the host actually *observed*
    /// that conclusion for this exact required context (the rollup carries
    /// it); `policy` is the repository's live Actions permissions posture, or
    /// `None` when the probe was unavailable/not attempted. With `None` the
    /// classification degrades to exactly [`Self::classify`]'s behaviour —
    /// probing never *invents* a diagnosis.
    ///
    /// Ordering when both modes fire for the same check: mode-1 (allowlist
    /// reconciliation) precedes mode-2 (SHA-pinning), because the allowlist
    /// rule is evaluated at startup before the pin rule — and the estate
    /// decision-of-record is `allowed_actions=all` with pinning KEPT, so the
    /// settings move is the estate-conformant first step; the pin move then
    /// surfaces on the next pass once the check can start.
    pub fn classify_with_policy(
        &self,
        check: &RequiredCheck,
        slug: &str,
        policy: Option<&ActionsPolicyFacts>,
        startup_failed: bool,
    ) -> Option<Move> {
        let name = check.required_context.as_str();
        let w = self.find_emitting(name)?;

        // 0. Actions-policy attribution (issue #15). A required-context
        //    STARTUP_FAILURE is mis-attributed by the rules below (a reusable
        //    caller reads as OwnedUpstream; a security workflow reads as
        //    Escalate-Security) when the real cause is the repository's own
        //    Actions posture refusing to start the run. Both replacement moves
        //    only let the check START — a legitimate non-bypass outcome.
        if startup_failed {
            if let Some(p) = policy {
                // Mode 1 — external `uses:` not covered by a `selected`
                // allowlist. An EMPTY pattern list refuses every external at
                // parse time; the estate decision-of-record posture
                // (allowed_actions=all, pinning kept) reconciles it.
                if let Some(blocked) = p.first_uncovered(&w.external_uses) {
                    if p.patterns_allowed.is_empty() {
                        return Some(Move::SetActionsAllowedAll);
                    }
                    return Some(Move::ReconcileActionsPolicy {
                        blocked_ref: blocked.to_string(),
                        add_patterns: vec![format!("{blocked}@*")],
                    });
                }
                // Mode 2 — a tag-pinned `uses:` under `sha_pinning_required`.
                // Applies to GitHub-owned actions too (the pin rule makes no
                // ownership exception), so this is not gated on `first_uncovered`.
                if p.sha_pinning_required && !w.tag_pinned_uses.is_empty() {
                    return Some(Move::PinWorkflowActions {
                        workflow: w.file.clone(),
                        refs: w.tag_pinned_uses.clone(),
                    });
                }
            }
        }

        if w.retired_descriptile_policy {
            return Some(Move::FlagNonFunctionalGate {
                check: name.to_string(),
                evidence: format!(
                    "`{}` requires a retired descriptile path; reconcile its policy with .machine_readable/descriptiles/ and SD004 before retrying",
                    w.file
                ),
            });
        }
        if w.empty_jobs {
            return Some(Move::FlagNonFunctionalGate {
                check: name.to_string(),
                evidence: format!(
                    "`{}` contains only commented jobs; GitHub cannot create a check from this template",
                    w.file
                ),
            });
        }

        // 1. Owned upstream: the job delegates to a reusable workflow living in
        //    another repo. The fix belongs there, not on this PR.
        if let Some(repo) = w.reusable_repos.iter().find(|r| r.as_str() != slug) {
            return Some(Move::AssignGateOwner {
                check: name.to_string(),
                owner: repo.clone(),
                disposition: OwnershipDisposition::OwnedUpstream { repo: repo.clone() },
                rationale: format!(
                    "emitted by `{}` which calls the reusable workflow in `{repo}`",
                    w.file
                ),
            });
        }

        // 2. Path-filter trap → the one appliable self-win. A required check is
        //    *Missing* (never created → gate Blocked) because its in-repo
        //    workflow is path-filtered and this PR is off-path. The documented
        //    estate fix (boj-server "CI / Required Status Checks") is to drop
        //    the `on.*.paths` filter so the required check is always created.
        //    This is strictly gate-*strengthening* — the check then runs on more
        //    PRs, never fewer — so it can never be a bypass, and it is a pure
        //    local file edit (`squabble fight --apply` can enact it with no
        //    network). Gated on `Missing`: a check that ran and Failed did
        //    trigger, so the filter is not its cause.
        if w.path_filtered && matches!(check.run, CheckRun::Missing) {
            return Some(Move::InjectPathFilterPassThrough {
                check: name.to_string(),
                workflow: w.file.clone(),
            });
        }

        // 3. In-lane CI-configuration hygiene: the squabbler owns workflow
        //    config, but the concrete violation must be ground-truthed first.
        if w.kind == WorkflowKind::Hygiene {
            return Some(Move::GroundTruthCheckNames {
                workflow: w.file.clone(),
            });
        }

        // 4. Out of lane → escalate to the matching specialist group. A
        //    `red→green code-fixer` is exactly what this repo IS-NOT.
        let (group, obligation, what) = match w.kind {
            WorkflowKind::CodeBuild => (
                ExpertGroup::HypatiaFleet,
                EscalationKind::DispatchFix,
                "builds/lints code or containers",
            ),
            WorkflowKind::Security => (
                ExpertGroup::Security,
                EscalationKind::Scan,
                "scans for secrets/vulnerabilities",
            ),
            WorkflowKind::Proof => (
                ExpertGroup::Proof,
                EscalationKind::VerifyClaim,
                "checks a formal-verification obligation",
            ),
            // Recognised the workflow but not its kind — stay conservative.
            WorkflowKind::Other | WorkflowKind::Hygiene => return None,
        };
        Some(Move::EscalateToExpert {
            check: name.to_string(),
            group,
            obligation,
            evidence: format!(
                "`{name}` runs in `{}` which {what} — out of the squabbler's CI-config lane",
                w.file
            ),
        })
    }
}

/// Parse the handful of facts we need from one workflow file's text.
fn parse_workflow(file: &str, text: &str) -> WorkflowInfo {
    let mut name = None;
    let mut job_ids = Vec::new();
    let mut job_names = Vec::new();
    let mut reusable_repos = Vec::new();
    let mut external_uses = Vec::new();
    let mut tag_pinned_uses = Vec::new();
    let mut in_jobs = false;
    let mut seen_jobs_header = false;

    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();

        if name.is_none() {
            if let Some(rest) = t.strip_prefix("name:") {
                // Only the top-level workflow name (zero indent).
                if indent == 0 {
                    name = Some(unquote(rest.trim()));
                }
            }
        }

        if indent == 0 && t.starts_with("jobs:") {
            in_jobs = true;
            seen_jobs_header = true;
            continue;
        }
        if seen_jobs_header && indent == 0 && !t.is_empty() && !t.starts_with('#') {
            // A new top-level key ends the jobs block.
            in_jobs = t.starts_with("jobs:");
        }

        if in_jobs && indent == 2 {
            if let Some(id) = t.strip_suffix(':') {
                if is_ident(id) {
                    job_ids.push(id.to_string());
                }
            }
        }
        if in_jobs && indent >= 4 {
            if let Some(rest) = t.strip_prefix("name:") {
                job_names.push(unquote(rest.trim()));
            }
        }

        if let Some(reuse) = reusable_repo(t) {
            if !reusable_repos.contains(&reuse) {
                reusable_repos.push(reuse);
            }
        }

        if let Some(target) = uses_target(t) {
            if let Some(norm) = normalize_external_use(target) {
                if !external_uses.contains(&norm) {
                    external_uses.push(norm.clone());
                }
                let reference = norm.rsplit('@').next().unwrap_or_default();
                if !is_sha_pin(reference) && !tag_pinned_uses.contains(&norm) {
                    tag_pinned_uses.push(norm);
                }
            }
        }
    }

    let path_filtered = has_path_filter(text);
    let kind = classify_kind(file, name.as_deref(), text);

    WorkflowInfo {
        file: file.to_string(),
        name,
        job_ids,
        job_names,
        reusable_repos,
        external_uses,
        tag_pinned_uses,
        path_filtered,
        retired_descriptile_policy: has_retired_descriptile_policy(text),
        empty_jobs: has_empty_jobs(text),
        kind,
    }
}

enum BlockState {
    None,
    Run { min_indent: usize, scalar: String },
    Other(usize),
}

/// Return whether a `run` scalar directly checks a known descriptile at either
/// retired `.machine_readable` location.
///
/// Quoted inline scalars are YAML-decoded. Text outside `run` scalars and
/// commands that do not begin with a supported file-existence check are ignored.
fn has_retired_descriptile_policy(text: &str) -> bool {
    let mut state = BlockState::None;

    for line in text.lines() {
        if line.trim().is_empty() {
            if let BlockState::Run { scalar, .. } = &mut state {
                scalar.push('\n');
            }
            continue;
        }
        let indent = line.chars().take_while(|c| c.is_whitespace()).count();
        let trimmed = line[indent..].trim_end();

        match &mut state {
            BlockState::Run { min_indent, scalar } if indent > *min_indent => {
                scalar.push_str(line);
                scalar.push('\n');
                continue;
            }
            BlockState::Other(min_indent) if indent > *min_indent => {
                continue;
            }
            BlockState::Run { scalar, .. } => {
                if decoded_scalar_has_retired_policy(scalar) {
                    return true;
                }
            }
            BlockState::None | BlockState::Other(_) => {}
        }
        state = BlockState::None;

        let is_run_key = trimmed.starts_with("- run:") || trimmed.starts_with("run:");
        let is_block_start = trimmed.ends_with('|')
            || trimmed.ends_with('>')
            || trimmed.ends_with("|-")
            || trimmed.ends_with(">-");

        if is_run_key {
            let scalar = trimmed
                .strip_prefix("- run:")
                .or_else(|| trimmed.strip_prefix("run:"))
                .unwrap()
                .trim_start();
            if scalar.starts_with('|') || scalar.starts_with('>') {
                state = BlockState::Run {
                    min_indent: indent,
                    scalar: format!("{scalar}\n"),
                };
            } else {
                let scalar_trim = scalar.trim();
                let decoded = if scalar_trim.starts_with(['\'', '"']) {
                    let Ok(value) = serde_yaml_ng::from_str::<String>(scalar_trim) else {
                        continue;
                    };
                    value
                } else {
                    scalar_trim.to_string()
                };
                if command_has_retired_policy(decoded.trim()) {
                    return true;
                }
            }
        } else if is_block_start {
            state = BlockState::Other(indent);
        }
    }

    matches!(state, BlockState::Run { ref scalar, .. } if decoded_scalar_has_retired_policy(scalar))
}

/// Return whether a YAML block scalar contains a recognised retired-path check.
/// Invalid scalars and scalars without a matching command line return `false`.
fn decoded_scalar_has_retired_policy(scalar: &str) -> bool {
    serde_yaml_ng::from_str::<String>(scalar)
        .is_ok_and(|decoded| decoded.lines().any(command_has_retired_policy))
}

/// Return whether a command starts with a supported existence check for a
/// retired descriptile path. Shell condition keywords and negation are allowed
/// before `check_file`, `test`, `[` or `[[` checks.
fn command_has_retired_policy(command: &str) -> bool {
    let mut words = command.split_whitespace().peekable();
    if matches!(words.peek(), Some(&"if" | &"elif" | &"while" | &"until")) {
        words.next();
    }
    if words.peek() == Some(&"!") {
        words.next();
    }
    let target = match words.next() {
        Some("check_file") => words.next(),
        Some("test" | "[" | "[[") => {
            if words.peek() == Some(&"!") {
                words.next();
            }
            if matches!(words.next(), Some("-f" | "-e")) {
                words.next()
            } else {
                None
            }
        }
        _ => None,
    };
    let Some(target) = target else {
        return false;
    };
    let target = target.trim_end_matches(';').trim_matches(['\'', '"']);
    [
        "STATE",
        "META",
        "ECOSYSTEM",
        "AGENTIC",
        "NEUROSYM",
        "PLAYBOOK",
        "ANCHOR",
    ]
    .iter()
    .any(|name| {
        target == format!(".machine_readable/{name}.a2ml")
            || target == format!(".machine_readable/6a2/{name}.a2ml")
    })
}

/// Return whether a bare top-level `jobs:` block contains no uncommented job.
fn has_empty_jobs(text: &str) -> bool {
    let mut in_jobs = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if in_jobs {
            // A non-comment indented value is outside this narrow diagnosis.
            return !line.starts_with(char::is_whitespace);
        }
        if line.strip_prefix("jobs:").is_some_and(|rest| {
            let rest = rest.trim();
            rest.is_empty() || rest.starts_with('#')
        }) {
            in_jobs = true;
        }
    }
    in_jobs
}

/// Extract `owner/repo` from a reusable-workflow `uses:` line, i.e. one whose
/// target contains `/.github/workflows/`. Action uses (`owner/repo@sha`) are
/// ignored — they are not gate-emitting reusables.
fn reusable_repo(line: &str) -> Option<String> {
    let rest = line.strip_prefix("uses:").map(str::trim)?;
    if !rest.contains("/.github/workflows/") {
        return None;
    }
    let target = rest.split('@').next().unwrap_or(rest);
    let mut segs = target.split('/');
    let owner = segs.next()?;
    let repo = segs.next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// The raw `uses:` target of a trimmed workflow line, with any trailing
/// comment cut and quotes removed. `None` for non-`uses:` lines.
fn uses_target<'a>(t: &'a str) -> Option<&'a str> {
    let rest = t.strip_prefix("uses:")?.trim();
    let token = rest.split_whitespace().next()?;
    Some(token.trim_matches(['\'', '"']))
}

/// Normalise an external (non-local) `uses:` target to `owner/repo@ref`,
/// collapsing any subpath — the same key shape as the actions lockfile, which
/// is what the Actions-policy probes (issue #15) compare `patterns_allowed`
/// against. Local actions (`./`, `$/`) and `docker://` images are not
/// external refs and yield `None`.
fn normalize_external_use(target: &str) -> Option<String> {
    if target.starts_with("./") || target.starts_with("$/") || target.starts_with("docker://") {
        return None;
    }
    let at = target.rfind('@')?;
    let (path, reference) = (&target[..at], &target[at + 1..]);
    if path.is_empty() || reference.is_empty() {
        return None;
    }
    let mut segs = path.split('/');
    let (owner, repo) = (segs.next()?, segs.next()?);
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}@{reference}"))
}

/// Is `reference` a full-length (40-hex) SHA pin, any case? Anything shorter
/// or non-hex is a tag/branch pin — refused at startup when the repository
/// sets `sha_pinning_required=true` (issue #15, mode 2).
fn is_sha_pin(reference: &str) -> bool {
    reference.len() == 40 && reference.chars().all(|c| c.is_ascii_hexdigit())
}

/// True if the file declares an `on.*.paths` filter (the path-filter trap).
fn has_path_filter(text: &str) -> bool {
    let mut before_jobs = true;
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();
        if indent == 0 && t.starts_with("jobs:") {
            before_jobs = false;
        }
        if before_jobs && (t == "paths:" || t.starts_with("paths:")) {
            return true;
        }
    }
    false
}

fn classify_kind(file: &str, name: Option<&str>, text: &str) -> WorkflowKind {
    let hay = format!("{} {} {}", file, name.unwrap_or(""), text).to_ascii_lowercase();

    // Hygiene: lints workflows/CI config itself.
    let hygiene = (hay.contains("workflow")
        && (hay.contains("lint") || hay.contains("security linter")))
        || file.contains("workflow-linter")
        || (hay.contains(".github/workflows")
            && (hay.contains("spdx") || hay.contains("pinned") || hay.contains("permissions:")));
    if hygiene {
        return WorkflowKind::Hygiene;
    }

    // Proof: formal verification.
    if [
        "gnatprove",
        "coqc",
        "lean",
        "agda",
        "isabelle",
        "echidna",
        "proof-check",
    ]
    .iter()
    .any(|k| hay.contains(k))
    {
        return WorkflowKind::Proof;
    }

    // Security: secret/vuln scanning.
    if [
        "trufflehog",
        "gitleaks",
        "codeql",
        "scorecard",
        "secret-scan",
        "cargo audit",
        "cargo-audit",
    ]
    .iter()
    .any(|k| hay.contains(k))
    {
        return WorkflowKind::Security;
    }

    // CodeBuild: builds/compiles/lints code or containers.
    if [
        "shellcheck",
        "cargo build",
        "cargo test",
        "podman build",
        "nerdctl build",
        "docker build",
        "container-build",
        "npm ",
        "make ",
        "just build",
    ]
    .iter()
    .any(|k| hay.contains(k))
    {
        return WorkflowKind::CodeBuild;
    }

    WorkflowKind::Other
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('"').unwrap_or(s);
    let s = s.strip_suffix('"').unwrap_or(s);
    let s = s.strip_prefix('\'').unwrap_or(s);
    let s = s.strip_suffix('\'').unwrap_or(s);
    s.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CI: &str = r#"
name: CI
on:
  pull_request:
    branches: [ main ]
permissions: read-all
jobs:
  lint-shell:
    runs-on: ubuntu-latest
    steps:
    - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0
    - name: Run ShellCheck
      uses: ludeeus/action-shellcheck@00cae500b08a931fb5698e11e79bfbd38e612a38
  container-build:
    runs-on: ubuntu-latest
    steps:
    - run: podman build -t x -f Containerfile .
"#;

    const GOVERNANCE: &str = r#"
name: Governance
on:
  pull_request:
    branches: [main, master]
permissions:
  contents: read
jobs:
  governance:
    uses: hyperpolymath/standards/.github/workflows/governance-reusable.yml@d7c22711e830e1f383846472f6e9b99debdb201e
"#;

    const WFLINT: &str = r#"
name: Workflow Security Linter
on:
  pull_request:
    paths:
      - '.github/workflows/**'
permissions: read-all
jobs:
  lint-workflows:
    runs-on: ubuntu-latest
    steps:
    - name: Check SPDX Headers
      run: echo hi
"#;

    fn facts() -> WorkflowFacts {
        WorkflowFacts {
            workflows: vec![
                parse_workflow("ci.yml", CI),
                parse_workflow("governance.yml", GOVERNANCE),
                parse_workflow("workflow-linter.yml", WFLINT),
            ],
        }
    }

    #[test]
    fn parses_job_ids_and_name() {
        let ci = parse_workflow("ci.yml", CI);
        assert_eq!(ci.name.as_deref(), Some("CI"));
        assert!(ci.job_ids.contains(&"lint-shell".to_string()));
        assert!(ci.job_ids.contains(&"container-build".to_string()));
        assert_eq!(ci.kind, WorkflowKind::CodeBuild);
    }

    #[test]
    fn detects_reusable_repo_owner() {
        let g = parse_workflow("governance.yml", GOVERNANCE);
        assert_eq!(
            g.reusable_repos,
            vec!["hyperpolymath/standards".to_string()]
        );
        assert!(g.job_ids.contains(&"governance".to_string()));
    }

    #[test]
    fn detects_path_filter_and_hygiene_kind() {
        let w = parse_workflow("workflow-linter.yml", WFLINT);
        assert!(w.path_filtered);
        assert_eq!(w.kind, WorkflowKind::Hygiene);
    }

    fn req(name: &str, run: CheckRun) -> RequiredCheck {
        RequiredCheck::new(name, run)
    }

    #[test]
    fn classify_reusable_check_as_owned_upstream() {
        let m = facts()
            .classify(
                &req("governance / Well-Known (RFC 9116 + RSR)", CheckRun::Failed),
                "hyperpolymath/ipv6-only",
            )
            .unwrap();
        match m {
            Move::AssignGateOwner { owner, .. } => assert_eq!(owner, "hyperpolymath/standards"),
            other => panic!("expected owner assignment, got {other:?}"),
        }
    }

    #[test]
    fn classify_codebuild_check_as_escalation() {
        let m = facts()
            .classify(
                &req("lint-shell", CheckRun::Failed),
                "hyperpolymath/ipv6-only",
            )
            .unwrap();
        assert!(matches!(
            m,
            Move::EscalateToExpert {
                group: ExpertGroup::HypatiaFleet,
                ..
            }
        ));
    }

    #[test]
    fn classify_hygiene_check_stays_in_lane() {
        // A hygiene check that ran and *Failed* is a real hygiene finding, not a
        // path-filter strand: it stays in-lane as GroundTruthCheckNames even
        // though its workflow is path-filtered.
        let m = facts()
            .classify(
                &req("lint-workflows", CheckRun::Failed),
                "hyperpolymath/ipv6-only",
            )
            .unwrap();
        assert!(matches!(m, Move::GroundTruthCheckNames { .. }));
    }

    #[test]
    fn classify_missing_path_filtered_check_is_appliable_passthrough() {
        // The same path-filtered workflow, but its required check never ran
        // (Missing → gate Blocked): now the appliable self-win is to strip the
        // filter so the required check is always created.
        let m = facts()
            .classify(
                &req("lint-workflows", CheckRun::Missing),
                "hyperpolymath/ipv6-only",
            )
            .unwrap();
        match m {
            Move::InjectPathFilterPassThrough { check, workflow } => {
                assert_eq!(check, "lint-workflows");
                assert_eq!(workflow, "workflow-linter.yml");
            }
            other => panic!("expected path-filter pass-through, got {other:?}"),
        }
    }

    #[test]
    fn unknown_check_is_unclassified() {
        assert!(facts()
            .classify(
                &req("something / nobody-emits", CheckRun::Failed),
                "hyperpolymath/ipv6-only"
            )
            .is_none());
    }
    #[test]
    fn retired_policy_is_a_gate_conflict_with_a_canonical_negative_control() {
        let bad = "name: Compliance\njobs:\n  compliance:\n    steps:\n      - run: test -f .machine_readable/STATE.a2ml\n";
        let parsed = parse_workflow("compliance.yml", bad);
        assert!(parsed.retired_descriptile_policy);
        let facts = WorkflowFacts {
            workflows: vec![parsed],
        };
        assert!(matches!(
            facts.classify(&req("compliance", CheckRun::Missing), "owner/repo"),
            Some(Move::FlagNonFunctionalGate { .. })
        ));
        let fixed = bad.replace(
            ".machine_readable/STATE",
            ".machine_readable/descriptiles/STATE",
        );
        assert!(!parse_workflow("compliance.yml", &fixed).retired_descriptile_policy);
        assert!(!has_retired_descriptile_policy(
            "# test -f .machine_readable/STATE.a2ml"
        ));
        assert!(!has_retired_descriptile_policy(
            "- run: echo 'test -f .machine_readable/STATE.a2ml'"
        ));
        for scalar in [
            r#"run: "test -f .machine_readable/STATE.a2ml""#,
            r#"run: 'test -f .machine_readable/STATE.a2ml'"#,
            r#"run: "test\x20-f\u0020.machine_readable/STATE.a2ml""#,
            r#"run: "test -f \".machine_readable/STATE.a2ml\"""#,
        ] {
            assert!(has_retired_descriptile_policy(scalar), "{scalar}");
        }
        assert!(!has_retired_descriptile_policy(
            r#"- run: "printf '%s\n' '# test -f .machine_readable/STATE.a2ml'""#
        ));
    }

    #[test]
    fn folded_retired_policy_command_is_a_non_functional_gate() {
        let workflow = r#"name: Compliance
jobs:
  compliance:
    steps:
      - run: >
          test -f
          .machine_readable/STATE.a2ml
"#;
        let facts = WorkflowFacts {
            workflows: vec![parse_workflow("compliance.yml", workflow)],
        };

        assert!(matches!(
            facts.classify(&req("compliance", CheckRun::Missing), "owner/repo"),
            Some(Move::FlagNonFunctionalGate { .. })
        ));
    }

    #[test]
    fn commented_jobs_cannot_supply_a_check() {
        let template = "name: E2E\njobs:\n  # test:\n  #   runs-on: ubuntu-latest\n";
        let parsed = parse_workflow("e2e.yml", template);
        assert!(parsed.empty_jobs);
        let facts = WorkflowFacts {
            workflows: vec![parsed],
        };
        assert!(matches!(
            facts.classify(&req("E2E", CheckRun::Missing), "owner/repo"),
            Some(Move::FlagNonFunctionalGate { .. })
        ));
        assert!(!has_empty_jobs("jobs:\n  test:\n    steps: []\n"));
        assert!(!has_empty_jobs("# jobs:\n"));
        assert!(has_empty_jobs("jobs: # template\n  # test:\n"));
        assert!(!has_empty_jobs(
            "jobs: # real jobs\n  test:\n    steps: []\n"
        ));
        assert!(!has_empty_jobs("jobs: { test: {} }\n"));
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    /// The two workflows of the `examples/actions-policy-deadlock.json`
    /// narrative, as text: a reusable-caller refused by an empty allowlist,
    /// and a tag-pinned workflow refused under enforced SHA pinning.
    const SECRET_SCANNER: &str = r#"
name: Secret Scanner
on:
  pull_request:
permissions:
  contents: read
jobs:
  scan:
    uses: hyperpolymath/standards/.github/workflows/secret-scanner-reusable.yml@8f2ee50841e216cd8c192eeb68953118190f105c
    secrets: inherit
"#;

    const CODEQL_TAGGED: &str = r#"
name: CodeQL Security Analysis
on:
  pull_request:
permissions:
  contents: read
jobs:
  analyze:
    runs-on: ubuntu-latest
    steps:
    - uses: actions/checkout@v7.0.1
    - uses: github/codeql-action/init@v4.38.0
    - uses: github/codeql-action/analyze@v4.38.0 # inline comment must be cut
    - uses: ./local-helper
"#;

    fn policy_facts() -> WorkflowFacts {
        WorkflowFacts {
            workflows: vec![
                parse_workflow("secret-scanner.yml", SECRET_SCANNER),
                parse_workflow("codeql.yml", CODEQL_TAGGED),
            ],
        }
    }

    #[test]
    fn external_uses_are_normalised_and_tag_pins_detected() {
        let w = parse_workflow("codeql.yml", CODEQL_TAGGED);
        // Subpath collapsed, deduplicated, the comment-bearing ref parsed,
        // the local action excluded, all in file order.
        assert_eq!(
            w.external_uses,
            vec![
                "actions/checkout@v7.0.1".to_string(),
                "github/codeql-action@v4.38.0".to_string(),
            ]
        );
        assert_eq!(w.tag_pinned_uses, w.external_uses);
        assert!(w.reusable_repos.is_empty());
    }

    #[test]
    fn sha_pinned_reusable_is_external_but_not_tag_pinned() {
        let w = parse_workflow("secret-scanner.yml", SECRET_SCANNER);
        assert_eq!(
            w.external_uses,
            vec!["hyperpolymath/standards@8f2ee50841e216cd8c192eeb68953118190f105c".to_string()]
        );
        assert!(w.tag_pinned_uses.is_empty());
        assert_eq!(w.reusable_repos, vec!["hyperpolymath/standards".to_string()]);
    }

    #[test]
    fn covers_matches_exact_owner_wide_and_prefix_patterns() {
        let p = ActionsPolicyFacts {
            allowed_actions: "selected".into(),
            sha_pinning_required: true,
            github_owned_allowed: true,
            patterns_allowed: vec![
                "hyperpolymath/*".into(),
                "oven-sh/setup-bun@*".into(),
                "r-lib/*".into(),
            ],
        };
        assert!(p.covers("actions/checkout"), "github-owned under github_owned_allowed");
        assert!(p.covers("github/codeql-action"));
        assert!(p.covers("hyperpolymath/standards"), "owner-wide");
        assert!(p.covers("oven-sh/setup-bun"), "exact owner/repo");
        assert!(p.covers("r-lib/actions"), "prefix glob");
        assert!(!p.covers("step-security/harden-runner"));
        // Without github_owned_allowed the GitHub-owned refs are NOT free.
        let strict = ActionsPolicyFacts {
            github_owned_allowed: false,
            ..p.clone()
        };
        assert!(!strict.covers("actions/checkout"));
        // Under a non-`selected` posture, first_uncovered is inert (mode 1 is
        // scoped to `selected` by issue #15).
        let all = ActionsPolicyFacts {
            allowed_actions: "all".into(),
            ..p
        };
        assert!(all.first_uncovered(&["step-security/harden-runner@v2".to_string()]).is_none());
    }

    #[test]
    fn empty_selected_allowlist_classifies_to_set_allowed_all() {
        // Mode 2 shape (pinning required, tags exist anywhere in the tree)
        // AND mode 1 shape (externals uncovered) present at once: mode 1
        // wins per the documented ordering — the allowlist rule is evaluated
        // first at startup, and allowed_actions=all is the estate's posture
        // of record.
        let facts = policy_facts();
        let policy = ActionsPolicyFacts {
            allowed_actions: "selected".into(),
            sha_pinning_required: true,
            github_owned_allowed: true,
            patterns_allowed: vec![],
        };
        assert_eq!(
            facts.classify_with_policy(
                &RequiredCheck::new("scan", CheckRun::Failed),
                "hyperpolymath/cicd-squabbler",
                Some(&policy),
                true,
            ),
            Some(Move::SetActionsAllowedAll),
            "empty patterns under selected → the estate default posture"
        );
    }

    #[test]
    fn selected_allowlist_gap_classifies_to_reconcile_with_the_missing_pattern() {
        let facts = policy_facts();
        let policy = ActionsPolicyFacts {
            allowed_actions: "selected".into(),
            sha_pinning_required: true,
            github_owned_allowed: true,
            patterns_allowed: vec!["oven-sh/setup-bun@*".into()],
        };
        assert_eq!(
            facts.classify_with_policy(
                &RequiredCheck::new("scan", CheckRun::Failed),
                "hyperpolymath/cicd-squabbler",
                Some(&policy),
                true,
            ),
            Some(Move::ReconcileActionsPolicy {
                blocked_ref: "hyperpolymath/standards".into(),
                add_patterns: vec!["hyperpolymath/standards@*".into()],
            })
        );
    }

    #[test]
    fn tag_pinned_under_sha_pinning_classifies_to_pin_not_escalate_security() {
        // Issue #15's canonical mis-attribution: a tag-pinned codeql.yml
        // STARTUP_FAILURE used to read as Escalate{Security}.
        let facts = policy_facts();
        let policy = ActionsPolicyFacts {
            allowed_actions: "all".into(), // no mode-1 surface at all
            sha_pinning_required: true,
            github_owned_allowed: true,
            patterns_allowed: vec![],
        };
        assert_eq!(
            facts.classify_with_policy(
                &RequiredCheck::new("analyze", CheckRun::Failed),
                "hyperpolymath/cicd-squabbler",
                Some(&policy),
                true,
            ),
            Some(Move::PinWorkflowActions {
                workflow: "codeql.yml".into(),
                refs: vec![
                    "actions/checkout@v7.0.1".into(),
                    "github/codeql-action@v4.38.0".into(),
                ],
            }),
            "a tag-vs-SHA refusal is a pinning fix, not a security scan"
        );
    }

    #[test]
    fn without_an_observed_startup_failure_the_policy_branches_stay_silent() {
        // classify() (no policy) and classify_with_policy(policy, startup=false)
        // must agree EXACTLY — the probe names a cause only when the platform
        // reported one. A Security-kind workflow then escalates as before.
        let facts = policy_facts();
        let policy = ActionsPolicyFacts {
            allowed_actions: "selected".into(),
            sha_pinning_required: true,
            github_owned_allowed: true,
            patterns_allowed: vec![],
        };
        let check = RequiredCheck::new("analyze", CheckRun::Failed);
        assert_eq!(
            facts.classify_with_policy(&check, "o/r", Some(&policy), false),
            facts.classify(&check, "o/r")
        );
        // The reusable caller keeps its ORIGINAL (mis)attribution when the
        // probe was not triggered: evidence first, posture second.
        let scan = RequiredCheck::new("scan", CheckRun::Failed);
        let degraded = facts.classify_with_policy(&scan, "o/r", Some(&policy), false);
        assert!(
            matches!(degraded, Some(Move::AssignGateOwner { .. })),
            "without STARTUP_FAILURE evidence the reusable reads OwnedUpstream as before: {degraded:?}"
        );
    }
}
