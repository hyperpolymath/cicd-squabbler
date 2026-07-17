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
    /// True if the file declares an `on.*.paths` trigger filter.
    pub path_filtered: bool,
    pub kind: WorkflowKind,
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
    /// `slug` is the current repo's `owner/repo`; a reusable workflow whose
    /// `owner/repo` differs is owned upstream. The check's realised [`CheckRun`]
    /// matters: the path-filter trap only manifests as a *Missing* check (the
    /// workflow never triggered off-path), so the appliable pass-through move is
    /// proposed only then — a check that actually ran and *Failed* is a
    /// different problem the filter cannot explain.
    pub fn classify(&self, check: &RequiredCheck, slug: &str) -> Option<Move> {
        let name = check.required_context.as_str();
        let w = self.find_emitting(name)?;

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
    }

    let path_filtered = has_path_filter(text);
    let kind = classify_kind(file, name.as_deref(), text);

    WorkflowInfo {
        file: file.to_string(),
        name,
        job_ids,
        job_names,
        reusable_repos,
        path_filtered,
        kind,
    }
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
}
