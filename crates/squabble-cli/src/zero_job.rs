// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble zero-job` — investigate failed runs with separate job evidence.
//!
//! A run that dies before any job exists creates **no check run**, so it never
//! appears red on a PR: the board simply looks quiet. This command finds those
//! runs and reports, per run, *which coordinate* caused it:
//!
//! ```text
//! repo · workflow · coordinate as written · canonical target · required allow-list pattern
//! ```
//!
//! The two coordinate classes are first-class and distinct
//! (`squabble_core::startup::StartupFailureClass`):
//!
//! * **moved coordinate** — a reusable coordinate under a renamed/transferred
//!   owner. Move: repoint to the canonical `owner/repo`, *same pin*.
//! * **allow-list admission required** — the canonical coordinate is refused by
//!   the caller's `selected` actions allow-list. Move: admit the reported
//!   `owner/repo@*` pattern.
//!
//! Optional official native lock verification adds distinct parser and
//! workflow-coverage diagnoses, bound to a pristine checkout at the failed
//! run's full commit ID. Neither can prescribe permission changes or green.
//!
//! Inputs: a source-matched local checkout (`<path[=owner/repo]>`), `gh` access
//! for the runs/jobs endpoints, and the live Actions
//! posture for the allow-list. The REST canonical-name read follows redirects
//! and compares `full_name`s — names, never pins.
//!
//! This command **diagnoses**; it never edits settings, repoints a workflow, or
//! declares anything green. Exit `4` means failed candidates remain (a
//! blocking finding) — including unreadable/contradictory evidence, because silence
//! is not a clean result. Exit `0` means none were observed in the scanned
//! window, which is a statement about that window only. Exit `2` is a tool
//! failure.

use crate::chains::BLOCKING_EXIT;
use crate::fetch;
use crate::lock_verifier;
use squabble_core::chains::{RepoId, ScanStatus, WorkflowFile};
use squabble_core::startup::{
    same_source_revision, AllowList, CanonicalResolution, NativeLockVerification, ZeroJobReport,
    ZeroJobRun, ZeroJobRunAttribution,
};
use squabble_fight::chains::{extract_edges, snapshot_local};
use squabble_fight::startup::{
    attribute_run, parse_job_count, parse_startup_runs, resolve_run_workflow,
};
use squabble_forge::rest::{resolve_canonical, GhRest, RestTransport};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::ExitCode;

pub(crate) const USAGE: &str = "usage: squabble zero-job <path[=owner/repo]> [--limit N] [--actions-lock-verifier /absolute/path] [--json]";

struct Args {
    path: String,
    slug: Option<String>,
    /// How many of the repo's most recent runs to scan for startup failures.
    limit: usize,
    json: bool,
    lock_verifier: Option<String>,
}

struct SourceEvidence {
    head_sha: Option<String>,
    lock: Option<NativeLockVerification>,
}

/// Parse one checkout path with an optional `=owner/repo` identity, a run
/// limit (default `30`), JSON output and an optional absolute verifier path.
/// Returns an error for a missing path, extra paths, unknown flags, a missing
/// or invalid limit (must be 1–100), or a missing or relative verifier path.
fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut a = Args {
        path: String::new(),
        slug: None,
        limit: 30,
        json: false,
        lock_verifier: None,
    };
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => a.json = true,
            "--actions-lock-verifier" => {
                let path = it
                    .next()
                    .filter(|p| Path::new(p).is_absolute())
                    .ok_or("--actions-lock-verifier requires an absolute executable path")?;
                a.lock_verifier = Some(path.clone());
            }
            "--limit" => {
                a.limit = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|n: &usize| (1..=100).contains(n))
                    .ok_or("--limit must be an integer from 1 to 100")?
            }
            s if s.starts_with("--") => return Err(format!("unknown flag `{s}`\n{USAGE}")),
            s => {
                if !a.path.is_empty() {
                    return Err(format!("expected one checkout path\n{USAGE}"));
                }
                match s.split_once('=') {
                    Some((p, slug)) => {
                        a.path = p.to_string();
                        a.slug = Some(slug.to_string());
                    }
                    None => a.path = s.to_string(),
                }
            }
        }
    }
    if a.path.is_empty() {
        return Err(USAGE.to_string());
    }
    Ok(a)
}

/// Scan recent failed runs and print attributions as text or JSON.
/// Returns `4` when any candidate remains, including one with unreadable or
/// contradictory job evidence; `0` means none remain in the returned page.
/// Usage, checkout, run listing, job evidence parsing and serialisation failures
/// return `2`; policy and name-resolution failures are retained as report notes.
pub fn run(rest: &[String]) -> ExitCode {
    let args = match parse_args(rest) {
        Ok(a) => a,
        Err(m) => {
            eprintln!("{m}");
            return ExitCode::from(2);
        }
    };

    let root = Path::new(&args.path);
    let before = lock_verifier::checkout_revision(root);
    let snapshot = snapshot_local(root, args.slug.as_deref());
    let slug = snapshot.repo.as_str().to_string();
    if !matches!(snapshot.status, ScanStatus::Scanned { .. }) {
        let reason = match &snapshot.status {
            ScanStatus::Unavailable { reason } => reason.clone(),
            other => format!("{other:?}"),
        };
        eprintln!("squabble zero-job: cannot read workflows for `{slug}`: {reason}");
        return ExitCode::from(2);
    }

    let runs = match GhRuns.startup_runs(&slug, args.limit) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("squabble zero-job: {e}");
            return ExitCode::from(2);
        }
    };

    // Probe the posture only when there is something to attribute — it is an
    // Administration-scoped read and must not be spent for nothing.
    let mut notes = snapshot.notes.clone();
    let after = lock_verifier::checkout_revision(root);
    let head_sha = match (&before, &after) {
        (Ok(a), Ok(b)) if same_source_revision(Some(a), Some(b)) => Some(a.clone()),
        _ => {
            notes.push(format!("local source identity unavailable or changed: before={before:?}; after={after:?}; no source/settings move may be inferred"));
            None
        }
    };
    let matching_run = runs.iter().any(|run| {
        run.jobs == Some(0) && same_source_revision(run.head_sha.as_deref(), head_sha.as_deref())
    });
    let lock = match (&args.lock_verifier, &head_sha) {
        (Some(binary), Some(head)) if matching_run => {
            let workflows = snapshot
                .files
                .iter()
                .map(|file| format!(".github/workflows/{}", file.path))
                .collect::<Vec<_>>();
            Some(lock_verifier::verify(
                Path::new(binary),
                root,
                head,
                &workflows,
            ))
        }
        (Some(_), _) => {
            notes.push("native lock verifier not run: no observed zero-job candidate matches the established checkout revision".into());
            None
        }
        (None, _) => {
            notes.push("native Actions lock not checked; supply --actions-lock-verifier with an independently verified official v0.2.0 binary for offline source diagnosis".into());
            None
        }
    };
    let source = SourceEvidence { head_sha, lock };
    let allow_list = if !matching_run {
        None
    } else {
        probe_allow_list(&slug, &mut notes)
    };

    let report = if runs.is_empty() {
        ZeroJobReport {
            repo: slug,
            checkout_head_sha: source.head_sha.clone(),
            lock_verification: source.lock.clone(),
            runs: Vec::new(),
            notes: {
                notes.push(format!(
                    "no zero-job failure candidate among the last {} run(s) — the zero-job class was \
                     not observed in this window (that is not a claim that CI works)",
                    args.limit
                ));
                notes
            },
        }
    } else {
        collect(
            &slug,
            &snapshot.files,
            runs,
            &GhRest,
            allow_list.as_ref(),
            notes,
            &source,
        )
    };

    if args.json {
        match serde_json::to_string_pretty(&report) {
            Ok(j) => println!("{j}"),
            Err(e) => {
                eprintln!("squabble zero-job: could not serialise report: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        print!("{}", narrate(&report));
    }
    exit_for(&report)
}

/// Read the caller's `selected` allow-list, or explain why no admission
/// attribution is possible. A probe failure is a note, never an invented fact.
fn probe_allow_list(slug: &str, notes: &mut Vec<String>) -> Option<AllowList> {
    match fetch::fetch_actions_policy(slug) {
        Ok(p) if p.allowed_actions == "selected" => Some(AllowList {
            patterns: p.patterns_allowed,
            github_owned_allowed: p.github_owned_allowed,
            verified_allowed: p.verified_allowed,
        }),
        Ok(p) => {
            notes.push(format!(
                "Actions posture is `{}`: only `allowed_actions=selected` can refuse a \
                 coordinate at startup, so no admission is attributed",
                if p.allowed_actions.is_empty() {
                    "unknown"
                } else {
                    p.allowed_actions.as_str()
                }
            ));
            None
        }
        Err(e) => {
            notes.push(format!(
                "Actions permission probe failed ({e}); allow-list admission cannot be \
                 attributed for this run"
            ));
            None
        }
    }
}

/// Assemble a report, resolving names for workflows of runs with zero jobs
/// and matching source revisions, then attributing each candidate. Resolution
/// failures are retained as notes; `source` supplies the checkout and optional
/// native lock evidence used for attribution. No IO occurs beyond `rest`.
fn collect(
    slug: &str,
    files: &[WorkflowFile],
    runs: Vec<ZeroJobRun>,
    rest: &dyn RestTransport,
    allow_list: Option<&AllowList>,
    mut notes: Vec<String>,
    source: &SourceEvidence,
) -> ZeroJobReport {
    let mut repos: BTreeSet<RepoId> = BTreeSet::new();
    for run in &runs {
        if run.jobs != Some(0)
            || !same_source_revision(run.head_sha.as_deref(), source.head_sha.as_deref())
        {
            continue;
        }
        if let Ok(file) = resolve_run_workflow(run, files) {
            for edge in extract_edges(&RepoId::new(slug), file) {
                repos.insert(edge.to);
            }
        }
    }
    let wanted: Vec<RepoId> = repos.into_iter().collect();
    let (canonical, calls) = resolve_canonical(rest, &wanted);
    if calls > 0 {
        notes.push(format!(
            "resolved {calls} upstream coordinate name(s) through REST (redirects followed; \
             names compared, pins untouched)"
        ));
    }
    for (repo, resolution) in &canonical {
        match resolution {
            CanonicalResolution::Moved { .. } | CanonicalResolution::Canonical => {}
            CanonicalResolution::NotFound => notes.push(format!(
                "`{repo}` does not resolve (404) for this token — a dead coordinate creates no \
                 check run; see `squabble chains`"
            )),
            CanonicalResolution::Unverified { reason } => {
                notes.push(format!("`{repo}` canonical name unverified: {reason}"))
            }
        }
    }

    let runs = runs
        .into_iter()
        .map(|run| ZeroJobRunAttribution {
            run_id: run.id,
            head_sha: run.head_sha.clone(),
            workflow: run.workflow_file.clone(),
            jobs: run.jobs,
            attributions: attribute_run(
                slug,
                &run,
                files,
                &canonical,
                allow_list,
                source.head_sha.as_deref(),
                source.lock.as_ref(),
            ),
        })
        .collect();

    ZeroJobReport {
        repo: slug.to_string(),
        checkout_head_sha: source.head_sha.clone(),
        lock_verification: source.lock.clone(),
        runs,
        notes,
    }
}

/// Any observed zero-job run is a blocking finding, attributed or not.
fn exit_for(report: &ZeroJobReport) -> ExitCode {
    if report.has_findings() {
        ExitCode::from(BLOCKING_EXIT)
    } else {
        ExitCode::SUCCESS
    }
}

/// Render runs, attributions, proposed moves, sequencing and caveats as a text
/// report, including unreadable job counts.
fn narrate(r: &ZeroJobReport) -> String {
    let mut o = format!(
        "zero-job: {} — {} failed run candidate(s); job counts are reported individually\n",
        r.repo,
        r.runs.len()
    );
    if let Some(lock) = &r.lock_verification {
        o.push_str(&narrate_lock(lock));
    }
    for run in &r.runs {
        let jobs = match run.jobs {
            Some(n) => format!("{n} job(s)"),
            None => "job count unreadable".to_string(),
        };
        o.push_str(&format!(
            "\nrun {} · {} ({jobs}) · source {}\n",
            run.run_id,
            run.workflow,
            run.head_sha.as_deref().unwrap_or("unreadable")
        ));
        for a in &run.attributions {
            o.push_str(&format!(
                "  [{}] {}\n",
                a.class.label().to_uppercase(),
                a.describe()
            ));
            if let Some(m) = a.proposed_move() {
                o.push_str(&format!("      move: {}\n", m.describe()));
            }
            for step in &a.sequencing {
                o.push_str(&format!("      · {step}\n"));
            }
        }
    }
    if !r.notes.is_empty() {
        o.push_str("\nnotes:\n");
        for n in &r.notes {
            o.push_str(&format!("  - {n}\n"));
        }
    }
    if r.has_findings() {
        o.push_str(
            "\nthis is a diagnosis, not a green: no move here satisfies a required check. \
             Repoint/admission only let the check start; it must then genuinely pass.\n",
        );
    }
    o
}

/// Retain the optional verifier's own diagnostics independently of run output.
fn narrate_lock(lock: &squabble_core::startup::NativeLockVerification) -> String {
    let mut output = format!(
        "native lock: {:?} at {} · {} exit {:?}\n  {}\n",
        lock.status, lock.head_sha, lock.tool_version, lock.exit_code, lock.reason
    );
    for line in lock.stderr.lines() {
        output.push_str(&format!("  {line}\n"));
    }
    if let Ok(report) = serde_json::from_str::<serde_json::Value>(&lock.stdout) {
        if let Some(findings) = report.get("findings").and_then(|v| v.as_array()) {
            for finding in findings {
                if let Some(detail) = finding.get("detail").and_then(|v| v.as_str()) {
                    output.push_str(&format!("  {detail}\n"));
                }
            }
        }
    }
    output
}

/// Live runs via `gh api --include` (REST). One listing call, plus one jobs
/// call per failed candidate to record the zero-job evidence.
struct GhRuns;

impl GhRuns {
    /// Request one page of recent runs with `limit` as `per_page`, retaining
    /// `startup_failure` and ordinary `failure` candidates.
    /// An unreadable jobs request leaves `jobs` as `None`; malformed counts
    /// fail the read. Ordinary failures with observed jobs are excluded.
    ///
    /// # Errors
    ///
    /// Propagates transport and parsing errors from the run listing, returns an
    /// error for a non-success listing status, and rejects malformed job counts
    /// from successful jobs responses.
    fn startup_runs(&self, slug: &str, limit: usize) -> Result<Vec<ZeroJobRun>, String> {
        fetch_startup_runs(&GhRest, slug, limit)
    }
}

/// Read one page of failed run candidates and their job counts through `transport`.
/// Ordinary failures with positive counts are excluded; startup failures with
/// positive counts remain as contradictory evidence. Job request errors and
/// non-success statuses leave the count unknown.
///
/// Returns an error for a failed or non-successful run listing, invalid run
/// data, or malformed job evidence from a successful jobs response.
fn fetch_startup_runs(
    transport: &dyn RestTransport,
    slug: &str,
    limit: usize,
) -> Result<Vec<ZeroJobRun>, String> {
    let path = format!("repos/{slug}/actions/runs?per_page={limit}");
    let resp = transport.get(&path)?;
    match resp.status {
        404 => {
            return Err(format!(
                "no Actions runs are visible for `{slug}` (HTTP 404) — check the slug and \
                     the token's Actions read scope"
            ))
        }
        s if !(200..300).contains(&s) => return Err(format!("`{path}` returned HTTP {s}")),
        _ => {}
    }
    let mut runs = parse_startup_runs(&resp.body)?;
    for run in &mut runs {
        let jobs_path = format!("repos/{slug}/actions/runs/{}/jobs?per_page=1", run.id);
        if let Ok(jobs) = transport.get(&jobs_path) {
            if (200..300).contains(&jobs.status) {
                let count = parse_job_count(&jobs.body)
                    .map_err(|err| format!("invalid job evidence for `{jobs_path}`: {err}"))?;
                run.jobs = Some(count);
            }
        }
    }
    // A regular failure with observed jobs belongs to execution-phase
    // diagnosis. Unknown counts remain visible, and a startup_failure
    // with positive jobs remains as contradictory evidence, never zero.
    runs.retain(|run| run.conclusion != "failure" || !matches!(run.jobs, Some(n) if n > 0));
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use squabble_core::moves::Move;
    use squabble_core::startup::StartupFailureClass;
    use squabble_forge::rest::RestResponse;
    use std::collections::BTreeMap;

    const SHA: &str = "8f2ee50841e216cd8c192eeb68953118190f105c";

    /// These fixtures represent an unmodified checkout at the observed SHA.
    fn collect(
        slug: &str,
        files: &[WorkflowFile],
        runs: Vec<ZeroJobRun>,
        rest: &dyn RestTransport,
        allow_list: Option<&AllowList>,
        notes: Vec<String>,
    ) -> ZeroJobReport {
        super::collect(
            slug,
            files,
            runs,
            rest,
            allow_list,
            notes,
            &SourceEvidence {
                head_sha: Some(SHA.into()),
                lock: None,
            },
        )
    }

    struct Responses(BTreeMap<String, RestResponse>);

    impl RestTransport for Responses {
        fn get(&self, path: &str) -> Result<RestResponse, String> {
            // Exercise the live transport's admission check too: the old
            // reader generated query strings that this check rejected.
            squabble_forge::rest::check_api_path(path)?;
            self.0
                .get(path)
                .cloned()
                .ok_or_else(|| format!("unexpected path {path}"))
        }
    }

    fn recorded_runs(conclusion: &str, jobs_status: u16, jobs: serde_json::Value) -> Responses {
        // Public run41880894104 is a real `failure` with jobs.total_count=0.
        // Other conclusions/counts below are planted boundary controls.
        let slug = "hyperpolymath/boj-server-cartridges";
        Responses(BTreeMap::from([
            (
                format!("repos/{slug}/actions/runs?per_page=30"),
                RestResponse {
                    status: 200,
                    location: None,
                    body: json!({"workflow_runs": [{
                        "id": 41880894104_u64,
                        "head_sha": "e9b571a48a9abe1429e9c271f1044e9c9decf574",
                        "conclusion": conclusion,
                        "path": ".github/workflows/governance.yml"
                    }]})
                    .to_string(),
                },
            ),
            (
                format!("repos/{slug}/actions/runs/41880894104/jobs?per_page=1"),
                RestResponse {
                    status: jobs_status,
                    location: None,
                    body: jobs.to_string(),
                },
            ),
        ]))
    }

    #[test]
    fn regular_failure_with_observed_zero_jobs_is_not_silently_dropped() {
        let rest = recorded_runs("failure", 200, json!({"total_count": 0, "jobs": []}));
        let runs = fetch_startup_runs(&rest, "hyperpolymath/boj-server-cartridges", 30).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, 41880894104);
        assert_eq!(runs[0].jobs, Some(0));
        assert_eq!(runs[0].conclusion, "failure");
        assert_eq!(
            runs[0].head_sha.as_deref(),
            Some("e9b571a48a9abe1429e9c271f1044e9c9decf574")
        );
    }

    #[test]
    fn ordinary_job_failures_are_left_to_execution_phase_diagnosis() {
        let rest = recorded_runs(
            "failure",
            200,
            json!({"total_count": 2, "jobs": [{"id": 1}]}),
        );
        assert!(
            fetch_startup_runs(&rest, "hyperpolymath/boj-server-cartridges", 30)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn missing_counts_remain_visible_and_cannot_propose_settings_changes() {
        let rest = recorded_runs("failure", 403, json!({"message": "not accessible"}));
        let runs = fetch_startup_runs(&rest, "hyperpolymath/boj-server-cartridges", 30).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].jobs, None);
        let report = collect(
            "hyperpolymath/boj-server-cartridges",
            &[moved_workflow()],
            runs,
            &Rest(vec![]),
            Some(&AllowList::default()),
            Vec::new(),
        );
        assert_eq!(exit_for(&report), ExitCode::from(BLOCKING_EXIT));
        assert!(report.runs[0].attributions[0].proposed_move().is_none());
        assert!(narrate(&report).contains("job count unreadable"));
        assert!(!narrate(&report).contains("jobs=0"));
    }

    #[test]
    fn contradictory_job_responses_fail_instead_of_becoming_zero() {
        let rest = recorded_runs(
            "startup_failure",
            200,
            json!({"total_count": 0, "jobs": [{"id": 1}]}),
        );
        assert!(
            fetch_startup_runs(&rest, "hyperpolymath/boj-server-cartridges", 30)
                .unwrap_err()
                .contains("more jobs")
        );
        let rest = recorded_runs(
            "startup_failure",
            200,
            json!({"total_count": 2, "jobs": [{"id": 1}]}),
        );
        let runs = fetch_startup_runs(&rest, "hyperpolymath/boj-server-cartridges", 30).unwrap();
        assert_eq!(runs[0].jobs, Some(2));
        let report = collect(
            "hyperpolymath/boj-server-cartridges",
            &[],
            runs,
            &Rest(vec![]),
            None,
            Vec::new(),
        );
        assert_eq!(exit_for(&report), ExitCode::from(BLOCKING_EXIT));
        assert!(report.runs[0].attributions[0]
            .evidence
            .contains("contradicting"));
        assert!(report.runs[0].attributions[0].proposed_move().is_none());
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

    struct Rest(Vec<(&'static str, CanonicalResolution)>);

    impl RestTransport for Rest {
        fn get(&self, path: &str) -> Result<RestResponse, String> {
            let slug = path
                .strip_prefix("repos/")
                .ok_or_else(|| format!("unexpected path {path}"))?;
            match self
                .0
                .iter()
                .find(|(s, _)| s.eq_ignore_ascii_case(slug))
                .map(|(_, r)| r)
            {
                Some(CanonicalResolution::Moved { canonical }) => Ok(RestResponse {
                    status: 200,
                    location: None,
                    body: json!({"full_name": canonical}).to_string(),
                }),
                Some(CanonicalResolution::NotFound) => Ok(RestResponse {
                    status: 404,
                    location: None,
                    body: "{}".into(),
                }),
                _ => Ok(RestResponse {
                    status: 200,
                    location: None,
                    body: json!({"full_name": slug}).to_string(),
                }),
            }
        }
    }

    fn moved_workflow() -> WorkflowFile {
        wf(
            "hypatia-scan.yml",
            &format!(
                "jobs:\n  scan:\n    uses: old/standards/.github/workflows/hypatia-scan.yml@{SHA}\n"
            ),
        )
    }

    #[test]
    fn ref_suffixes_resolve_the_source_file_before_upstream_collection() {
        let mut file = moved_workflow();
        file.path = "release@nightly.yml".into();
        let files = [file];
        let rest = Rest(vec![(
            "old/standards",
            CanonicalResolution::Moved {
                canonical: "new/standards".into(),
            },
        )]);
        for path in [
            ".github/workflows/release@nightly.yml@main",
            ".github/workflows/release@nightly.yml@feature/topic",
            ".github/workflows/release@nightly.yml@refs/heads/main",
        ] {
            let body = json!({"workflow_runs": [{
                "id": 44, "head_sha": SHA, "conclusion": "startup_failure", "path": path
            }]})
            .to_string();
            let mut observed = parse_startup_runs(&body).unwrap().remove(0);
            observed.jobs = Some(0);
            let report = collect(
                "me/app",
                &files,
                vec![observed],
                &rest,
                None,
                Vec::new(),
            );
            let finding = &report.runs[0].attributions[0];
            assert_eq!(finding.workflow, "release@nightly.yml");
            assert_eq!(finding.class, StartupFailureClass::MovedCoordinate);
            assert!(matches!(
                finding.proposed_move(),
                Some(Move::RepointToCanonical { pin, .. }) if pin == SHA
            ));
            assert!(report.has_findings());
            assert_eq!(exit_for(&report), ExitCode::from(BLOCKING_EXIT));
        }
    }

    #[test]
    fn ambiguous_workflow_names_cannot_trigger_coordinate_reads_or_moves() {
        struct NoCoordinateReads;
        impl RestTransport for NoCoordinateReads {
            fn get(&self, path: &str) -> Result<RestResponse, String> {
                panic!("ambiguous workflow must not resolve any coordinate: {path}");
            }
        }

        let files = [
            wf("release.yml", &moved_workflow().text),
            wf("release.yml@nightly.yml", &moved_workflow().text),
        ];
        for path in [
            ".github/workflows/release.yml@nightly.yml",
            ".github/workflows/release.yml@nightly.yml@main",
            ".github/workflows/release.yml@nightly.yml@refs/heads/main",
        ] {
            let body = json!({"workflow_runs": [{
                "id": 45, "head_sha": SHA, "conclusion": "startup_failure", "path": path
            }]})
            .to_string();
            let mut observed = parse_startup_runs(&body).unwrap().remove(0);
            observed.jobs = Some(0);
            let report = collect(
                "me/app",
                &files,
                vec![observed],
                &NoCoordinateReads,
                Some(&AllowList::default()),
                Vec::new(),
            );
            let finding = &report.runs[0].attributions[0];
            assert_eq!(finding.class, StartupFailureClass::Unattributed);
            assert!(finding.evidence.contains("ambiguous"));
            assert!(finding.proposed_move().is_none());
            assert!(finding.required_allow_list_pattern.is_none());
            assert_eq!(exit_for(&report), ExitCode::from(BLOCKING_EXIT));
        }
    }

    #[test]
    fn a_moved_reusable_run_reports_every_attribution_field() {
        let files = vec![moved_workflow()];
        let rest = Rest(vec![(
            "old/standards",
            CanonicalResolution::Moved {
                canonical: "hyperpolymath/standards".into(),
            },
        )]);
        let report = collect(
            "hyperpolymath/hypatia",
            &files,
            vec![run(40_451_601_571, "hypatia-scan.yml")],
            &rest,
            None,
            Vec::new(),
        );
        assert!(report.has_findings());
        let r = &report.runs[0];
        assert_eq!(r.run_id, 40_451_601_571);
        assert_eq!(r.workflow, "hypatia-scan.yml");
        assert_eq!(r.jobs, Some(0));
        assert_eq!(r.attributions.len(), 1);
        let a = &r.attributions[0];
        assert_eq!(a.repo, "hyperpolymath/hypatia");
        assert_eq!(
            a.coordinate,
            format!("old/standards/.github/workflows/hypatia-scan.yml@{SHA}")
        );
        assert_eq!(a.canonical.as_deref(), Some("hyperpolymath/standards"));
        assert_eq!(a.class, StartupFailureClass::MovedCoordinate);
        assert!(matches!(
            a.proposed_move(),
            Some(Move::RepointToCanonical { .. })
        ));
        assert!(report.notes.iter().any(|n| n.contains("REST")));
        assert_eq!(exit_for(&report), ExitCode::from(BLOCKING_EXIT));
        assert!(narrate(&report).contains("not a green"));
    }

    #[test]
    fn an_allow_list_gap_produces_the_admission_pattern() {
        let files = vec![moved_workflow()];
        let rest = Rest(vec![(
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
        let report = collect(
            "hyperpolymath/hypatia",
            &files,
            vec![run(1, "hypatia-scan.yml")],
            &rest,
            Some(&allow),
            Vec::new(),
        );
        let a = &report.runs[0].attributions[0];
        // Repoint first (moved), admission sequenced next.
        assert_eq!(a.class, StartupFailureClass::MovedCoordinate);
        assert_eq!(
            a.required_allow_list_pattern.as_deref(),
            Some("hyperpolymath/standards@*")
        );
    }

    #[test]
    fn a_canonical_coordinate_refused_by_the_allow_list_is_admission_required() {
        let files = vec![wf(
            "gov.yml",
            &format!(
                "jobs:\n  gov:\n    uses: hyperpolymath/standards/.github/workflows/gov.yml@{SHA}\n"
            ),
        )];
        let allow = AllowList {
            patterns: vec!["actions/*".into()],
            github_owned_allowed: true,
            verified_allowed: false,
        };
        let report = collect(
            "hyperpolymath/hypatia",
            &files,
            vec![run(2, "gov.yml")],
            &Rest(vec![]),
            Some(&allow),
            Vec::new(),
        );
        let a = &report.runs[0].attributions[0];
        assert_eq!(a.class, StartupFailureClass::AllowListAdmissionRequired);
        assert_eq!(
            a.required_allow_list_pattern.as_deref(),
            Some("hyperpolymath/standards@*")
        );
        assert!(matches!(
            a.proposed_move(),
            Some(Move::AllowListAdmissionRequired { .. })
        ));
    }

    #[test]
    fn an_unattributable_run_still_blocks() {
        let files = vec![wf(
            "ci.yml",
            "jobs:\n  a:\n    steps:\n      - uses: actions/checkout@v4\n",
        )];
        let report = collect(
            "hyperpolymath/hypatia",
            &files,
            vec![run(3, "ci.yml")],
            &Rest(vec![]),
            None,
            Vec::new(),
        );
        assert!(report.has_findings());
        assert_eq!(
            report.runs[0].attributions[0].class,
            StartupFailureClass::Unattributed
        );
        // Silence is never a clean result.
        assert_eq!(exit_for(&report), ExitCode::from(BLOCKING_EXIT));
    }

    #[test]
    fn no_runs_is_a_success_with_no_claim_about_ci() {
        let report = ZeroJobReport {
            repo: "hyperpolymath/hypatia".into(),
            checkout_head_sha: None,
            lock_verification: None,
            runs: Vec::new(),
            notes: vec!["no STARTUP_FAILURE run".into()],
        };
        assert_eq!(exit_for(&report), ExitCode::SUCCESS);
        assert!(!narrate(&report).contains("not a green"));
    }

    #[test]
    fn a_dead_coordinate_is_a_note_not_a_fabricated_move() {
        let files = vec![moved_workflow()];
        let rest = Rest(vec![("old/standards", CanonicalResolution::NotFound)]);
        let report = collect(
            "hyperpolymath/hypatia",
            &files,
            vec![run(4, "hypatia-scan.yml")],
            &rest,
            None,
            Vec::new(),
        );
        assert!(report.notes.iter().any(|n| n.contains("404")));
        assert_eq!(
            report.runs[0].attributions[0].class,
            StartupFailureClass::Unattributed
        );
        assert!(report.runs[0].attributions[0].proposed_move().is_none());
    }

    #[test]
    fn args_parse_and_reject_bad_input() {
        let a = parse_args(&["./x=me/x".into(), "--limit".into(), "5".into()]).unwrap();
        assert_eq!(a.path, "./x");
        assert_eq!(a.slug.as_deref(), Some("me/x"));
        assert_eq!(a.limit, 5);
        assert!(!a.json);
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&["x".into(), "--limit".into(), "0".into()]).is_err());
        assert!(parse_args(&["x".into(), "--nope".into()]).is_err());
        assert!(parse_args(&["x".into(), "--actions-lock-verifier".into()]).is_err());
        assert!(parse_args(&[
            "x".into(),
            "--actions-lock-verifier".into(),
            "relative".into()
        ])
        .is_err());
        let a = parse_args(&[
            "x".into(),
            "--actions-lock-verifier".into(),
            "/verified/gh-actions-lock".into(),
        ])
        .unwrap();
        assert_eq!(
            a.lock_verifier.as_deref(),
            Some("/verified/gh-actions-lock")
        );
    }

    #[test]
    fn limit_is_bounded_by_the_single_page_run_cap() {
        for limit in [1, 100] {
            let a = parse_args(&["x".into(), "--limit".into(), limit.to_string()]).unwrap();
            assert_eq!(a.limit, limit);
        }
        for limit in ["0", "101", "1000", "-1", "nope", "18446744073709551616"] {
            let result = parse_args(&["x".into(), "--limit".into(), limit.into()]);
            assert_eq!(
                result.err().as_deref(),
                Some("--limit must be an integer from 1 to 100")
            );
        }
        assert_eq!(
            parse_args(&["x".into(), "--limit".into()]).err().as_deref(),
            Some("--limit must be an integer from 1 to 100")
        );
    }
}
