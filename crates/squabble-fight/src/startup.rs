// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Host side of failed-run startup attribution.
//!
//! `squabble-core::startup` owns the taxonomy and the moves; this module does
//! the estate-aware half: read a run's own workflow file, extract every
//! external `uses:` coordinate, and attribute each one against the canonical
//! names and the caller's live allow-list.
//!
//! Two rules keep the output honest:
//!
//! * **Only observed runs are attributed.** The caller supplies runs that
//!   actually failed and supplies their separately read job counts. A source
//!   attribution also requires the failed run's complete commit ID to match
//!   the unmodified checkout; this module never invents either fact.
//! * **A run always reports something.** If no coordinate is moved or
//!   unadmitted, the run gets one [`StartupFailureClass::Unattributed`] entry
//!   naming the workflow — never silence, and never a fabricated cause.
//!
//! Nothing here can make a check green, drop a required context, or bypass a
//! gate. The attribution feeds a report; the run stays failed until the
//! workflow genuinely starts and passes.

use squabble_core::chains::{Edge, RepoId, WorkflowFile};
use squabble_core::startup::{
    attribute_zero_job, same_source_revision, AllowList, CanonicalResolution, NativeLockStatus,
    NativeLockVerification, StartupFailureClass, ZeroJobAttribution, ZeroJobRun,
};
use std::collections::BTreeMap;

/// Render an extracted [`Edge`] as a coordinate:
/// `owner/repo[/subpath]@pin`.
///
/// The stored spelling is used; extraction lowercases full SHAs. No pin is
/// resolved here.
pub fn coordinate_of(edge: &Edge) -> String {
    let sub = edge
        .path
        .as_deref()
        .map(|p| format!("/{p}"))
        .unwrap_or_default();
    format!("{}{}@{}", edge.to, sub, edge.pin.as_str())
}

/// Attribute every external `uses:` coordinate in one workflow file.
///
/// Returns one entry per distinct coordinate, including `Unattributed` entries
/// — callers that only want findings filter with
/// [`StartupFailureClass::Unattributed`].
/// Missing canonical resolutions are treated as unverified; `None` for
/// `allow_list` disables admission attribution.
pub fn attribute_workflow(
    repo: &str,
    workflow: &str,
    text: &str,
    canonical: &BTreeMap<RepoId, CanonicalResolution>,
    allow_list: Option<&AllowList>,
) -> Vec<ZeroJobAttribution> {
    let wf = WorkflowFile {
        path: workflow.to_string(),
        text: text.to_string(),
    };
    let mut out: Vec<ZeroJobAttribution> = Vec::new();
    for edge in crate::chains::extract_edges(&RepoId::new(repo), &wf) {
        let written = coordinate_of(&edge);
        let resolution =
            canonical
                .get(&edge.to)
                .cloned()
                .unwrap_or(CanonicalResolution::Unverified {
                    reason: "canonical name was not resolved".to_string(),
                });
        let attribution =
            attribute_zero_job(repo, workflow, &written, edge.kind, &resolution, allow_list);
        if !out.contains(&attribution) {
            out.push(attribution);
        }
    }
    out
}

/// Attribute one failed run from the repo's already-read workflow files.
///
/// Attribution requires an observed zero job count and a full run commit ID
/// matching `checkout_revision`, which the caller must establish from a
/// checkout with an unmodified workflow tree. Otherwise the result is one
/// `Unattributed` entry.
/// Optional `lock` evidence must match that revision: malformed locks and
/// workflow-specific coverage mismatches take precedence over coordinates,
/// while evidence from a different source revision leaves the run unattributed.
/// An unverified optional probe is retained beside independent coordinate
/// attribution. A verified lock, or a coverage mismatch for another workflow,
/// also permits coordinate attribution to continue.
///
/// When the run's workflow file is not among `files` (an unreadable checkout, a
/// workflow deleted since the run), the run still gets an honest
/// `Unattributed` entry with the reason — absence of evidence is never
/// evidence of a clean start.
///
/// When coordinate attribution finds no moved or unadmitted coordinate, the
/// result is a single `Unattributed` entry rather than one per coordinate. The
/// entry kept is the first by coordinate in lexicographic order. Attributed
/// results are also sorted by coordinate. The caller supplies the observed
/// run; only an explicitly observed zero job count permits attribution.
pub fn attribute_run(
    repo: &str,
    run: &ZeroJobRun,
    files: &[WorkflowFile],
    canonical: &BTreeMap<RepoId, CanonicalResolution>,
    allow_list: Option<&AllowList>,
    checkout_revision: Option<&str>,
    lock: Option<&NativeLockVerification>,
) -> Vec<ZeroJobAttribution> {
    if run.jobs != Some(0) {
        let evidence = match run.jobs {
            Some(count) => format!(
                "run {} created {count} job(s), contradicting zero-job startup attribution; \
                 inspect the job's execution evidence before proposing a source or settings change",
                run.id
            ),
            None => format!(
                "run {} has no readable job count, so zero-job startup attribution is unverified; \
                 recover the run's job evidence before proposing a source or settings change",
                run.id
            ),
        };
        return vec![unattributed(repo, &run.workflow_file, evidence)];
    }
    if !same_source_revision(run.head_sha.as_deref(), checkout_revision) {
        return vec![unattributed(
            repo,
            &run.workflow_file,
            format!(
                "run {} source {:?} does not match an established, unmodified checkout {:?}; \
             inspect the run's exact source before proposing a source or settings change",
                run.id, run.head_sha, checkout_revision
            ),
        )];
    }
    if let Some(lock) = lock {
        if !same_source_revision(Some(&lock.head_sha), checkout_revision) {
            return vec![unattributed(repo, &run.workflow_file,
                "native lock evidence belongs to a different source revision; no attribution or move inferred".into())];
        }
        let class = match lock.status {
            NativeLockStatus::Malformed => Some(StartupFailureClass::InvalidActionsLock),
            NativeLockStatus::CoverageMismatch => {
                let path = format!(".github/workflows/{}", run.workflow_file);
                let affected = serde_json::from_str::<serde_json::Value>(&lock.stdout)
                    .ok()
                    .and_then(|report| report.get("workflows").and_then(|v| v.as_array()).cloned())
                    .is_some_and(|workflows| {
                        workflows.iter().any(|workflow| {
                            workflow.get("path").and_then(|v| v.as_str()) == Some(path.as_str())
                                && workflow.get("valid").and_then(|v| v.as_bool()) == Some(false)
                        })
                    });
                affected.then_some(StartupFailureClass::ActionsLockMismatch)
            }
            NativeLockStatus::Unverified => None,
            NativeLockStatus::Verified => None,
        };
        if let Some(class) = class {
            let mut finding = unattributed(
                repo,
                &run.workflow_file,
                format!(
                    "{} at source {} ({} exit {:?}); the complete official output is retained. \
                    This reproduces a source defect, not proof of the sole GitHub startup cause.",
                    lock.reason, lock.head_sha, lock.tool_version, lock.exit_code
                ),
            );
            finding.class = class;
            finding.sequencing = vec![
                "review a candidate produced by the repository's official lock regeneration chain; do not hand-edit actions.lock or widen Actions permissions".into(),
                "verify that candidate against the exact workflows, then rerun the failed workflow; this diagnosis satisfies no required check".into(),
            ];
            return vec![finding];
        }
    }
    // An optional probe cannot erase independently established coordinate
    // evidence. Keep its failure alongside that evidence without inferring
    // any lock repair, permission change or successful verification.
    let retain_probe_failure = |mut findings: Vec<ZeroJobAttribution>| {
        if let Some(probe) = lock.filter(|probe| probe.status == NativeLockStatus::Unverified) {
            findings.push(unattributed(
                repo,
                &run.workflow_file,
                format!("native lock verification is unverified: {}; no repair inferred from that result", probe.reason),
            ));
        }
        findings
    };
    let Some(file) = files.iter().find(|f| f.path == run.workflow_file) else {
        return retain_probe_failure(vec![unattributed(
            repo,
            &run.workflow_file,
            format!(
                "workflow `{}` is not in the local checkout, so its `uses:` coordinates cannot \
                 be read. The run is still a zero-job startup failure — read the workflow at \
                 the run's commit before changing anything.",
                run.workflow_file
            ),
        )]);
    };

    let all = attribute_workflow(repo, &run.workflow_file, &file.text, canonical, allow_list);
    let (mut primary, mut unexplained): (Vec<ZeroJobAttribution>, Vec<ZeroJobAttribution>) = all
        .into_iter()
        .partition(|a| a.class != StartupFailureClass::Unattributed);
    if primary.is_empty() {
        // Prefer the most specific non-answer the workflow produced: a dead
        // coordinate or an unverified name is a fact the operator can act on,
        // and it must not be flattened into "everything resolved canonically".
        unexplained.sort_by(|a, b| a.coordinate.cmp(&b.coordinate));
        return retain_probe_failure(match unexplained.into_iter().next() {
            Some(a) => vec![a],
            None => vec![unattributed(
                repo,
                &run.workflow_file,
                "this workflow references no external `uses:` coordinate at all, so a moved \
                 name or an allow-list refusal cannot explain the zero-job failure. Diagnose \
                 from the run's own workflow."
                    .to_string(),
            )],
        });
    }
    primary.sort_by(|a, b| a.coordinate.cmp(&b.coordinate));
    retain_probe_failure(primary)
}

/// Build a workflow-level explanation without blaming a coordinate or
/// proposing a move.
fn unattributed(repo: &str, workflow: &str, evidence: String) -> ZeroJobAttribution {
    ZeroJobAttribution {
        repo: repo.to_string(),
        workflow: workflow.to_string(),
        // No coordinate is blamed; say so rather than echoing the workflow path
        // into the coordinate field.
        coordinate: "(none attributed)".to_string(),
        canonical: None,
        required_allow_list_pattern: None,
        class: StartupFailureClass::Unattributed,
        evidence,
        sequencing: Vec::new(),
    }
}

/// Parse a `GET /repos/{o}/{r}/actions/runs` response, retaining failed run
/// candidates (`startup_failure` or `failure`) for a separate job-count read.
/// An ordinary failure is not a startup diagnosis by itself.
///
/// `jobs` is left `None` here: the count comes from a second read and is the
/// host's evidence, not something this parser may assume.
/// Matching is case-insensitive. A failed entry without an unsigned integer
/// run ID is an error; a missing or non-string path becomes an empty filename,
/// otherwise the ref suffix is dropped and only the final path component is
/// kept. GitHub reports `path` either as `.github/workflows/ci.yml` or as
/// `.github/workflows/ci.yml@refs/heads/main`; taking the last slash-separated
/// component of the second form would name the file `main`, so every run would
/// fail to match the checkout and be attributed to nothing.
///
/// # Errors
///
/// Returns an error for invalid JSON or a missing or non-array `workflow_runs`.
pub fn parse_startup_runs(body: &str) -> Result<Vec<ZeroJobRun>, String> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("could not parse actions/runs response: {e}"))?;
    let runs = v
        .get("workflow_runs")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "actions/runs response has no `workflow_runs` array".to_string())?;
    let mut out = Vec::new();
    for r in runs {
        let Some(conclusion) = r
            .get("conclusion")
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_lowercase)
            .filter(|c| matches!(c.as_str(), "startup_failure" | "failure"))
        else {
            continue;
        };
        // A startup failure with no usable id cannot be reported, and dropping
        // it would let the command exit 0 saying it observed none. Fail closed.
        let id = r
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                format!(
                    "a `{conclusion}` run has no unsigned integer `id`, so it cannot be \
                     reported: refusing to skip it and claim no failure was observed{}",
                    r.get("path")
                        .and_then(serde_json::Value::as_str)
                        .map(|p| format!(" (path `{p}`)"))
                        .unwrap_or_default()
                )
            })?;
        let path = r
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        // Drop a `@ref` suffix before taking the filename: `.github/
        // workflows/ci.yml@refs/heads/main` must yield `ci.yml`, not
        // `main`. The ref is not evidence of which file ran.
        let path = path.split('@').next().unwrap_or(path);
        let workflow_file = path.rsplit('/').next().unwrap_or(path).to_string();
        let workflow_name = r
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        out.push(ZeroJobRun {
            id,
            conclusion,
            head_sha: r
                .get("head_sha")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            workflow_file,
            workflow_name,
            jobs: None,
        });
    }
    Ok(out)
}

/// `total_count` from a `GET .../runs/{id}/jobs` response.
///
/// # Errors
///
/// Returns an error for invalid JSON or when `total_count` is absent or is not
/// an unsigned integer, or when a present `jobs` field is not an array or has
/// more entries than `total_count`. Zero is a valid count.
pub fn parse_job_count(body: &str) -> Result<u64, String> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("could not parse actions/jobs response: {e}"))?;
    let count = v
        .get("total_count")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "actions/jobs response has no `total_count`".to_string())?;
    if let Some(jobs) = v.get("jobs") {
        let jobs = jobs
            .as_array()
            .ok_or("actions/jobs response has invalid `jobs`")?;
        if jobs.len() as u64 > count {
            return Err("actions/jobs response lists more jobs than its total_count".to_string());
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use squabble_core::chains::EdgeKind;
    use squabble_core::moves::Move;

    const CALLER: &str = "hyperpolymath/hypatia";
    const SHA: &str = "8f2ee50841e216cd8c192eeb68953118190f105c";

    #[test]
    fn missing_or_different_source_cannot_borrow_current_workflow_attribution() {
        let files = [wf("ci.yml", WORKFLOW)];
        let names = canonical(&[(
            "old/standards",
            CanonicalResolution::Moved {
                canonical: "new/standards".into(),
            },
        )]);
        for checkout in [
            None,
            Some("main"),
            Some(&SHA[..8]),
            Some("0000000000000000000000000000000000000000"),
        ] {
            let out = attribute_run(
                CALLER,
                &run(42, "ci.yml"),
                &files,
                &names,
                Some(&AllowList::default()),
                checkout,
                None,
            );
            assert_eq!(out[0].class, StartupFailureClass::Unattributed);
            assert!(out[0].proposed_move().is_none());
            assert!(out[0].required_allow_list_pattern.is_none());
            assert!(out[0].evidence.contains("source"));
        }
        let mut observed = run(42, "ci.yml");
        observed.head_sha = None;
        assert_eq!(
            attribute_run(CALLER, &observed, &files, &names, None, Some(SHA), None)[0].class,
            StartupFailureClass::Unattributed
        );
    }

    fn native_lock(status: NativeLockStatus) -> NativeLockVerification {
        NativeLockVerification {
            head_sha: SHA.into(),
            verifier: "/verified/gh-actions-lock".into(),
            tool_version: "actions-lock version v0.2.0".into(),
            exit_code: Some(2),
            status,
            stdout: String::new(),
            stderr: "recorded native parser error".into(),
            reason: "native parser rejected this source lock".into(),
        }
    }

    #[test]
    fn native_lock_defect_is_source_evidence_without_a_permission_move_or_green() {
        let lock = native_lock(NativeLockStatus::Malformed);
        let out = attribute_run(
            CALLER,
            &run(42, "ci.yml"),
            &[],
            &BTreeMap::new(),
            Some(&AllowList::default()),
            Some(SHA),
            Some(&lock),
        );
        assert_eq!(out[0].class, StartupFailureClass::InvalidActionsLock);
        assert!(out[0].proposed_move().is_none());
        assert!(out[0].missing_cause().is_none());
        assert!(out[0].required_allow_list_pattern.is_none());
        assert!(out[0]
            .evidence
            .contains("not proof of the sole GitHub startup cause"));
        assert!(out[0].sequencing[0].contains("do not hand-edit"));

        let mut stale = lock.clone();
        stale.head_sha = "0000000000000000000000000000000000000000".into();
        assert_eq!(
            attribute_run(
                CALLER,
                &run(42, "ci.yml"),
                &[],
                &BTreeMap::new(),
                None,
                Some(SHA),
                Some(&stale)
            )[0]
            .class,
            StartupFailureClass::Unattributed
        );
        let mut unknown = run(42, "ci.yml");
        unknown.jobs = None;
        assert_eq!(
            attribute_run(
                CALLER,
                &unknown,
                &[],
                &BTreeMap::new(),
                None,
                Some(SHA),
                Some(&lock)
            )[0]
            .class,
            StartupFailureClass::Unattributed
        );
    }

    #[test]
    fn coverage_mismatch_is_workflow_specific_and_probe_failure_is_not_clean() {
        let mut lock = native_lock(NativeLockStatus::CoverageMismatch);
        lock.exit_code = Some(1);
        lock.stdout = serde_json::json!({"workflows": [
            {"path": ".github/workflows/ci.yml", "valid": false},
            {"path": ".github/workflows/other.yml", "valid": true}
        ]})
        .to_string();
        let out = attribute_run(
            CALLER,
            &run(42, "ci.yml"),
            &[],
            &BTreeMap::new(),
            None,
            Some(SHA),
            Some(&lock),
        );
        assert_eq!(out[0].class, StartupFailureClass::ActionsLockMismatch);
        assert!(out[0].proposed_move().is_none());
        let other = attribute_run(
            CALLER,
            &run(43, "other.yml"),
            &[],
            &BTreeMap::new(),
            None,
            Some(SHA),
            Some(&lock),
        );
        assert_eq!(other[0].class, StartupFailureClass::Unattributed);
        lock.status = NativeLockStatus::Unverified;
        assert_eq!(
            attribute_run(
                CALLER,
                &run(42, "ci.yml"),
                &[],
                &BTreeMap::new(),
                None,
                Some(SHA),
                Some(&lock)
            )[0]
            .class,
            StartupFailureClass::Unattributed
        );
    }

    #[test]
    fn absent_or_positive_job_counts_cannot_propose_admission_or_repointing() {
        let files = [wf(
            "ci.yml",
            &format!("jobs:\n  scan:\n    uses: old/standards/.github/workflows/ci.yml@{SHA}\n"),
        )];
        let canonical = BTreeMap::from([(
            RepoId::new("old/standards"),
            CanonicalResolution::Moved {
                canonical: "new/standards".into(),
            },
        )]);
        let allow = AllowList::default();
        for count in [None, Some(1), Some(3)] {
            let mut observed = run(42, "ci.yml");
            observed.jobs = count;
            let findings = attribute_run(
                CALLER,
                &observed,
                &files,
                &canonical,
                Some(&allow),
                Some(SHA),
                None,
            );
            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].class, StartupFailureClass::Unattributed);
            assert!(findings[0].proposed_move().is_none());
            assert!(findings[0].required_allow_list_pattern.is_none());
            assert!(findings[0].evidence.contains(if count.is_some() {
                "contradicting"
            } else {
                "unverified"
            }));
        }
    }

    #[test]
    fn optional_probe_failure_preserves_independent_coordinate_attribution() {
        let files = [wf(
            "ci.yml",
            &format!("jobs:\n  scan:\n    uses: old/standards/.github/workflows/ci.yml@{SHA}\n"),
        )];
        let moved = BTreeMap::from([(
            RepoId::new("old/standards"),
            CanonicalResolution::Moved {
                canonical: "new/standards".into(),
            },
        )]);
        let unchanged = BTreeMap::new();
        let allow = AllowList::default();
        for names in [&moved, &unchanged] {
            let expected = attribute_run(
                CALLER,
                &run(42, "ci.yml"),
                &files,
                names,
                Some(&allow),
                Some(SHA),
                None,
            );
            for reason in [
                "verified binary is unavailable",
                "unrelated untracked file prevents full-input qualification",
            ] {
                let mut probe = native_lock(NativeLockStatus::Unverified);
                probe.reason = reason.into();
                let actual = attribute_run(
                    CALLER,
                    &run(42, "ci.yml"),
                    &files,
                    names,
                    Some(&allow),
                    Some(SHA),
                    Some(&probe),
                );
                assert_eq!(&actual[..expected.len()], expected.as_slice());
                let retained = actual.last().unwrap();
                assert_eq!(actual.len(), expected.len() + 1);
                assert_eq!(retained.class, StartupFailureClass::Unattributed);
                assert!(retained.evidence.contains(reason));
                assert!(retained.proposed_move().is_none());
                assert!(retained.required_allow_list_pattern.is_none());
            }
        }
    }

    fn wf(path: &str, text: &str) -> WorkflowFile {
        WorkflowFile {
            path: path.into(),
            text: text.into(),
        }
    }

    fn run(id: u64, file: &str) -> ZeroJobRun {
        ZeroJobRun {
            id,
            conclusion: "startup_failure".into(),
            head_sha: Some(SHA.into()),
            workflow_file: file.into(),
            workflow_name: None,
            jobs: Some(0),
        }
    }

    fn canonical(pairs: &[(&str, CanonicalResolution)]) -> BTreeMap<RepoId, CanonicalResolution> {
        pairs
            .iter()
            .map(|(k, v)| (RepoId::new(*k), v.clone()))
            .collect()
    }

    const WORKFLOW: &str = "\
jobs:
  scan:
    uses: old/standards/.github/workflows/hypatia-scan.yml@8f2ee50841e216cd8c192eeb68953118190f105c
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
";

    #[test]
    fn a_moved_reusable_is_attributed_with_the_coordinate_as_written() {
        let files = vec![wf("hypatia-scan.yml", WORKFLOW)];
        let names = canonical(&[(
            "old/standards",
            CanonicalResolution::Moved {
                canonical: "hyperpolymath/standards".into(),
            },
        )]);
        let out = attribute_run(
            CALLER,
            &run(40_451_601_571, "hypatia-scan.yml"),
            &files,
            &names,
            None,
            Some(SHA),
            None,
        );
        assert_eq!(out.len(), 1, "{out:#?}");
        let a = &out[0];
        assert_eq!(a.repo, CALLER);
        assert_eq!(a.workflow, "hypatia-scan.yml");
        assert_eq!(
            a.coordinate,
            format!("old/standards/.github/workflows/hypatia-scan.yml@{SHA}")
        );
        assert_eq!(a.canonical.as_deref(), Some("hyperpolymath/standards"));
        assert_eq!(a.class, StartupFailureClass::MovedCoordinate);
        match a.proposed_move() {
            Some(Move::RepointToCanonical { canonical, pin, .. }) => {
                assert_eq!(canonical, "hyperpolymath/standards");
                assert_eq!(pin, SHA);
            }
            other => panic!("expected repoint, got {other:?}"),
        }
    }

    #[test]
    fn an_unadmitted_coordinate_is_attributed_with_its_required_pattern() {
        let files = vec![wf("hypatia-scan.yml", WORKFLOW)];
        let names = canonical(&[(
            "old/standards",
            CanonicalResolution::Moved {
                canonical: "hyperpolymath/standards".into(),
            },
        )]);
        let allow = AllowList {
            patterns: vec!["actions/*".into()],
            github_owned_allowed: true,
            verified_allowed: false,
        };
        let out = attribute_run(
            CALLER,
            &run(1, "hypatia-scan.yml"),
            &files,
            &names,
            Some(&allow),
            Some(SHA),
            None,
        );
        // The moved reusable wins, but the admission pattern is carried so the
        // sequencing is visible in one report.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].class, StartupFailureClass::MovedCoordinate);
        assert_eq!(
            out[0].required_allow_list_pattern.as_deref(),
            Some("hyperpolymath/standards@*")
        );
        assert!(out[0].sequencing.len() >= 2);
    }

    #[test]
    fn a_canonical_unadmitted_coordinate_is_admission_required() {
        let files = vec![wf("hypatia-scan.yml", WORKFLOW)];
        // The workflow uses `old/standards`; with no resolution supplied the
        // attribution uses the written name, which is the only name it has.
        let allow = AllowList {
            patterns: vec!["actions/*".into()],
            github_owned_allowed: true,
            verified_allowed: false,
        };
        let out = attribute_run(
            CALLER,
            &run(1, "hypatia-scan.yml"),
            &files,
            &BTreeMap::new(),
            Some(&allow),
            Some(SHA),
            None,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].class,
            StartupFailureClass::AllowListAdmissionRequired
        );
        assert_eq!(
            out[0].required_allow_list_pattern.as_deref(),
            Some("old/standards@*")
        );
    }

    #[test]
    fn a_workflow_with_no_culprit_gets_one_honest_unattributed_entry() {
        let text = "jobs:\n  a:\n    steps:\n      - uses: actions/checkout@v4\n";
        let files = vec![wf("ci.yml", text)];
        let allow = AllowList {
            patterns: vec!["actions/*".into()],
            github_owned_allowed: true,
            verified_allowed: false,
        };
        let out = attribute_run(
            CALLER,
            &run(2, "ci.yml"),
            &files,
            &BTreeMap::new(),
            Some(&allow),
            Some(SHA),
            None,
        );
        assert_eq!(out.len(), 1, "{out:#?}");
        assert_eq!(out[0].class, StartupFailureClass::Unattributed);
        assert!(out[0].proposed_move().is_none());
        assert!(out[0].evidence.contains("outside this taxonomy"));
    }

    #[test]
    fn a_missing_workflow_file_is_unattributed_not_clean() {
        let out = attribute_run(
            CALLER,
            &run(3, "deleted.yml"),
            &[],
            &BTreeMap::new(),
            None,
            Some(SHA),
            None,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].class, StartupFailureClass::Unattributed);
        assert!(out[0].evidence.contains("not in the local checkout"));
    }

    #[test]
    fn coordinate_rendering_preserves_subpath_and_pin() {
        let file = wf("x.yml", WORKFLOW);
        let edges = crate::chains::extract_edges(&RepoId::new(CALLER), &file);
        let reusable = edges
            .iter()
            .find(|e| e.kind == EdgeKind::ReusableWorkflow)
            .expect("reusable");
        assert_eq!(
            coordinate_of(reusable),
            format!("old/standards/.github/workflows/hypatia-scan.yml@{SHA}")
        );
        let action = edges
            .iter()
            .find(|e| e.kind == EdgeKind::Action)
            .expect("action");
        assert_eq!(coordinate_of(action), "actions/checkout@v4");
    }

    #[test]
    fn run_parsing_keeps_failed_candidates_without_inventing_job_counts() {
        let body = serde_json::json!({
            "total_count": 3,
            "workflow_runs": [
                {"id": 11, "conclusion": "startup_failure", "path": ".github/workflows/hypatia-scan.yml", "name": "Hypatia scan"},
                {"id": 12, "conclusion": "failure", "path": ".github/workflows/ci.yml", "name": "CI"},
                {"id": 13, "conclusion": "STARTUP_FAILURE", "path": ".github/workflows/gov.yml", "name": "gov"},
                {"id": 14, "conclusion": null, "path": ".github/workflows/x.yml"}
            ]
        })
        .to_string();
        let runs = parse_startup_runs(&body).expect("parse");
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].id, 11);
        assert_eq!(runs[0].workflow_file, "hypatia-scan.yml");
        assert_eq!(runs[0].workflow_name.as_deref(), Some("Hypatia scan"));
        assert_eq!(runs[1].workflow_file, "ci.yml");
        assert_eq!(runs[1].conclusion, "failure");
        assert_eq!(runs[2].workflow_file, "gov.yml");
        assert!(runs.iter().all(|r| r.jobs.is_none()));
    }

    #[test]
    fn a_run_path_carrying_a_ref_suffix_still_names_the_workflow_file() {
        // GitHub reports `path` both ways. Taking the last slash-separated
        // component of the suffixed form would name the file after the ref and
        // silently attribute nothing, so the suffix must go first.
        let body = serde_json::json!({
            "workflow_runs": [
                {"id": 21, "conclusion": "startup_failure",
                 "path": ".github/workflows/gov.yml@refs/heads/main"},
                {"id": 22, "conclusion": "startup_failure",
                 "path": ".github/workflows/ci.yml@refs/pull/140/merge"}
            ]
        })
        .to_string();
        let runs = parse_startup_runs(&body).expect("parse");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].workflow_file, "gov.yml");
        assert_eq!(runs[1].workflow_file, "ci.yml");
    }

    #[test]
    fn a_startup_failure_without_a_usable_id_is_an_error_not_a_skip() {
        // Skipping it would leave an empty result, and `zero-job` would then exit
        // 0 reporting that it observed no startup failure — a silent skip.
        let body = serde_json::json!({
            "workflow_runs": [
                {"conclusion": "startup_failure", "path": ".github/workflows/gov.yml"}
            ]
        })
        .to_string();
        let err = parse_startup_runs(&body).expect_err("must refuse");
        assert!(err.contains("no unsigned integer `id`"), "{err}");
        assert!(
            err.contains("gov.yml"),
            "the error must name the run: {err}"
        );

        // A non-integer id is refused the same way. A regular failure without
        // an id cannot have its job count checked either, so it is not skipped.
        let quoted = serde_json::json!({
            "workflow_runs": [{"id": "12", "conclusion": "startup_failure"}]
        })
        .to_string();
        assert!(parse_startup_runs(&quoted).is_err());
        let fine = serde_json::json!({
            "workflow_runs": [{"conclusion": "failure"}]
        })
        .to_string();
        assert!(parse_startup_runs(&fine).is_err());
        let healthy = serde_json::json!({
            "workflow_runs": [{"conclusion": "success"}]
        })
        .to_string();
        assert!(parse_startup_runs(&healthy).unwrap().is_empty());
    }

    #[test]
    fn malformed_run_payloads_are_errors_not_empty_successes() {
        assert!(parse_startup_runs("not json").is_err());
        assert!(parse_startup_runs("{}").is_err());
        assert!(parse_startup_runs("{\"workflow_runs\":[]}")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn job_counts_are_read_strictly() {
        assert_eq!(parse_job_count("{\"total_count\":0}").unwrap(), 0);
        assert_eq!(
            parse_job_count("{\"total_count\":0,\"jobs\":[]}").unwrap(),
            0
        );
        assert!(parse_job_count(r#"{"total_count":0,"jobs":[{"id":1}]}"#).is_err());
        assert!(parse_job_count("{}").is_err());
        assert!(parse_job_count("[]").is_err());
    }
}
