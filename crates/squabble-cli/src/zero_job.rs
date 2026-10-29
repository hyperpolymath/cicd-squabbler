// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble zero-job` — attribute every zero-job `startup_failure` run.
//!
//! A run that dies before any job exists creates **no check run**, so it never
//! appears red on a PR: the board simply looks quiet. This command finds those
//! runs and reports, per run, *which coordinate* caused it:
//!
//! ```text
//! repo · workflow · coordinate as written · canonical target · required allow-list pattern
//! ```
//!
//! The two classes are first-class and distinct
//! (`squabble_core::startup::StartupFailureClass`):
//!
//! * **moved coordinate** — a reusable coordinate under a renamed/transferred
//!   owner. Move: repoint to the canonical `owner/repo`, *same pin*.
//! * **allow-list admission required** — the canonical coordinate is refused by
//!   the caller's `selected` actions allow-list. Move: admit the reported
//!   `owner/repo@*` pattern.
//!
//! Inputs: a local checkout (`<path[=owner/repo]>`) for the workflow text the
//! run used, `gh` access for the runs/jobs endpoints, and the live Actions
//! posture for the allow-list. The REST canonical-name read follows redirects
//! and compares `full_name`s — names, never pins.
//!
//! This command **diagnoses**; it never edits settings, repoints a workflow, or
//! declares anything green. Exit `4` means zero-job runs were observed (a
//! blocking finding) — including runs it could not attribute, because silence
//! is not a clean result. Exit `0` means none were observed in the scanned
//! window, which is a statement about that window only. Exit `2` is a tool
//! failure.

use crate::chains::BLOCKING_EXIT;
use crate::fetch;
use squabble_core::chains::{RepoId, ScanStatus, WorkflowFile};
use squabble_core::startup::{
    AllowList, CanonicalResolution, ZeroJobReport, ZeroJobRun, ZeroJobRunAttribution,
};
use squabble_fight::chains::{extract_edges, snapshot_local};
use squabble_fight::startup::{attribute_run, parse_job_count, parse_startup_runs};
use squabble_forge::rest::{resolve_canonical, GhRest, RestTransport};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::ExitCode;

pub(crate) const USAGE: &str = "usage: squabble zero-job <path[=owner/repo]> [--limit N] [--json]";

struct Args {
    path: String,
    slug: Option<String>,
    /// How many of the repo's most recent runs to scan for startup failures.
    limit: usize,
    json: bool,
}

/// Parse one checkout path with an optional `=owner/repo` identity, a run
/// limit (default `30`) and JSON output. Returns an error for a missing path,
/// extra paths, unknown flags or a missing or invalid limit (must be 1–100).
fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut a = Args {
        path: String::new(),
        slug: None,
        limit: 30,
        json: false,
    };
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => a.json = true,
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

/// Scan recent runs and print startup-failure attributions as text or JSON.
/// Returns `4` when any startup failure is listed, even if its job count could
/// not be read; `0` means none in the returned page. Usage, checkout, run
/// listing and serialisation failures return `2`; policy and name-resolution
/// failures are retained as report notes.
pub fn run(rest: &[String]) -> ExitCode {
    let args = match parse_args(rest) {
        Ok(a) => a,
        Err(m) => {
            eprintln!("{m}");
            return ExitCode::from(2);
        }
    };

    let snapshot = snapshot_local(Path::new(&args.path), args.slug.as_deref());
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
    let allow_list = if runs.is_empty() {
        None
    } else {
        probe_allow_list(&slug, &mut notes)
    };

    let report = if runs.is_empty() {
        ZeroJobReport {
            repo: slug,
            runs: Vec::new(),
            notes: {
                notes.push(format!(
                    "no `STARTUP_FAILURE` run among the last {} run(s) — the zero-job class was \
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

/// Pure assembly: resolve canonical names for every coordinate the failing
/// workflows use, then attribute each run. No IO beyond `rest`, so the whole
/// report shape is unit-testable.
fn collect(
    slug: &str,
    files: &[WorkflowFile],
    runs: Vec<ZeroJobRun>,
    rest: &dyn RestTransport,
    allow_list: Option<&AllowList>,
    mut notes: Vec<String>,
) -> ZeroJobReport {
    let mut repos: BTreeSet<RepoId> = BTreeSet::new();
    for run in &runs {
        if let Some(file) = files.iter().find(|f| f.path == run.workflow_file) {
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
            workflow: run.workflow_file.clone(),
            jobs: run.jobs,
            attributions: attribute_run(slug, &run, files, &canonical, allow_list),
        })
        .collect();

    ZeroJobReport {
        repo: slug.to_string(),
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
        "zero-job: {} — {} `startup_failure` run(s) (jobs=0, no check run exists)\n",
        r.repo,
        r.runs.len()
    );
    for run in &r.runs {
        let jobs = match run.jobs {
            Some(n) => format!("{n} job(s)"),
            None => "job count unreadable".to_string(),
        };
        o.push_str(&format!(
            "\nrun {} · {} ({jobs})\n",
            run.run_id, run.workflow
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

/// Live runs via `gh api --include` (REST). One listing call, plus one jobs
/// call per startup failure to record the zero-job evidence.
struct GhRuns;

impl GhRuns {
    /// Request one page of recent runs with `limit` as `per_page`, retaining
    /// startup failures.
    /// Read each matching run's job count; a failed jobs request or invalid
    /// count leaves `jobs` as `None`, and positive counts are retained too.
    ///
    /// # Errors
    ///
    /// Propagates transport and parsing errors from the run listing, and
    /// returns an error for any non-success HTTP status on that listing.
    fn startup_runs(&self, slug: &str, limit: usize) -> Result<Vec<ZeroJobRun>, String> {
        let transport = GhRest;
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
                    // Absence of a count is recorded as `None`, not as zero.
                    run.jobs = parse_job_count(&jobs.body).ok();
                }
            }
        }
        Ok(runs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use squabble_core::moves::Move;
    use squabble_core::startup::StartupFailureClass;
    use squabble_forge::rest::RestResponse;

    const SHA: &str = "8f2ee50841e216cd8c192eeb68953118190f105c";

    fn wf(path: &str, text: &str) -> WorkflowFile {
        WorkflowFile {
            path: path.into(),
            text: text.into(),
        }
    }

    fn run(id: u64, file: &str) -> ZeroJobRun {
        ZeroJobRun {
            id,
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
