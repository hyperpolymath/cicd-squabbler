// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Host side of zero-job `startup_failure` attribution.
//!
//! `squabble-core::startup` owns the taxonomy and the moves; this module does
//! the estate-aware half: read a run's own workflow file, extract every
//! external `uses:` coordinate, and attribute each one against the canonical
//! names and the caller's live allow-list.
//!
//! Two rules keep the output honest:
//!
//! * **Only observed runs are attributed.** The caller supplies runs that
//!   actually concluded `STARTUP_FAILURE`; this module never invents one.
//! * **A run always reports something.** If no coordinate is moved or
//!   unadmitted, the run gets one [`StartupFailureClass::Unattributed`] entry
//!   naming the workflow — never silence, and never a fabricated cause.
//!
//! Nothing here can make a check green, drop a required context, or bypass a
//! gate. The attribution feeds a report; the run stays failed until the
//! workflow genuinely starts and passes.

use squabble_core::chains::{Edge, RepoId, WorkflowFile};
use squabble_core::startup::{
    attribute_zero_job, AllowList, CanonicalResolution, StartupFailureClass, ZeroJobAttribution,
    ZeroJobRun,
};
use std::collections::BTreeMap;

/// The coordinate exactly as written, rebuilt from an extracted [`Edge`]:
/// `owner/repo[/subpath]@pin`.
///
/// The pin is reproduced verbatim (a SHA as the SHA, a tag as the tag) — this
/// is evidence, not resolution.
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
        let resolution = canonical
            .get(&edge.to)
            .cloned()
            .unwrap_or(CanonicalResolution::Unverified {
                reason: "canonical name was not resolved".to_string(),
            });
        let attribution = attribute_zero_job(
            repo,
            workflow,
            &written,
            edge.kind,
            &resolution,
            allow_list,
        );
        if !out.contains(&attribution) {
            out.push(attribution);
        }
    }
    out
}

/// Attribute one zero-job run from the repo's already-read workflow files.
///
/// When the run's workflow file is not among `files` (an unreadable checkout, a
/// workflow deleted since the run), the run still gets an honest
/// `Unattributed` entry with the reason — absence of evidence is never
/// evidence of a clean start.
///
/// When no coordinate is moved or unadmitted, the result is a single
/// `Unattributed` entry rather than one per coordinate: the run failed for a
/// reason outside this taxonomy, and the report should say that once. The
/// entry kept is the most specific honest non-answer available (a dead
/// coordinate, a moved action, an unverified name), because "resolves
/// canonically" would be a false statement about a 404.
pub fn attribute_run(
    repo: &str,
    run: &ZeroJobRun,
    files: &[WorkflowFile],
    canonical: &BTreeMap<RepoId, CanonicalResolution>,
    allow_list: Option<&AllowList>,
) -> Vec<ZeroJobAttribution> {
    let Some(file) = files.iter().find(|f| f.path == run.workflow_file) else {
        return vec![unattributed(
            repo,
            &run.workflow_file,
            format!(
                "workflow `{}` is not in the local checkout, so its `uses:` coordinates cannot \
                 be read. The run is still a zero-job startup failure — read the workflow at \
                 the run's commit before changing anything.",
                run.workflow_file
            ),
        )];
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
        return match unexplained.into_iter().next() {
            Some(a) => vec![a],
            None => vec![unattributed(
                repo,
                &run.workflow_file,
                "this workflow references no external `uses:` coordinate at all, so a moved \
                 name or an allow-list refusal cannot explain the zero-job failure. Diagnose \
                 from the run's own workflow."
                    .to_string(),
            )],
        };
    }
    primary.sort_by(|a, b| a.coordinate.cmp(&b.coordinate));
    primary
}

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

/// Parse a `GET /repos/{o}/{r}/actions/runs` response, keeping only runs that
/// concluded `STARTUP_FAILURE`.
///
/// `jobs` is left `None` here: the count comes from a second read and is the
/// host's evidence, not something this parser may assume.
pub fn parse_startup_runs(body: &str) -> Result<Vec<ZeroJobRun>, String> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("could not parse actions/runs response: {e}"))?;
    let runs = v
        .get("workflow_runs")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "actions/runs response has no `workflow_runs` array".to_string())?;
    Ok(runs
        .iter()
        .filter_map(|r| {
            if !r
                .get("conclusion")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|c| c.eq_ignore_ascii_case("startup_failure"))
            {
                return None;
            }
            let id = r.get("id").and_then(serde_json::Value::as_u64)?;
            let path = r
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let workflow_file = path.rsplit('/').next().unwrap_or(path).to_string();
            let workflow_name = r
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            Some(ZeroJobRun {
                id,
                workflow_file,
                workflow_name,
                jobs: None,
            })
        })
        .collect())
}

/// `total_count` from a `GET .../runs/{id}/jobs` response.
pub fn parse_job_count(body: &str) -> Result<u64, String> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("could not parse actions/jobs response: {e}"))?;
    v.get("total_count")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "actions/jobs response has no `total_count`".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use squabble_core::chains::EdgeKind;
    use squabble_core::moves::Move;

    const CALLER: &str = "hyperpolymath/hypatia";
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
        let out = attribute_run(CALLER, &run(40_451_601_571, "hypatia-scan.yml"), &files, &names, None);
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
            Some(Move::RepointToCanonical {
                canonical, pin, ..
            }) => {
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
        };
        let out = attribute_run(
            CALLER,
            &run(1, "hypatia-scan.yml"),
            &files,
            &names,
            Some(&allow),
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
        };
        let out = attribute_run(
            CALLER,
            &run(1, "hypatia-scan.yml"),
            &files,
            &BTreeMap::new(),
            Some(&allow),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].class, StartupFailureClass::AllowListAdmissionRequired);
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
        };
        let out = attribute_run(
            CALLER,
            &run(2, "ci.yml"),
            &files,
            &BTreeMap::new(),
            Some(&allow),
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
    fn run_parsing_keeps_only_startup_failures() {
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
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, 11);
        assert_eq!(runs[0].workflow_file, "hypatia-scan.yml");
        assert_eq!(runs[0].workflow_name.as_deref(), Some("Hypatia scan"));
        assert_eq!(runs[1].workflow_file, "gov.yml");
        assert!(runs.iter().all(|r| r.jobs.is_none()));
    }

    #[test]
    fn malformed_run_payloads_are_errors_not_empty_successes() {
        assert!(parse_startup_runs("not json").is_err());
        assert!(parse_startup_runs("{}").is_err());
        assert!(parse_startup_runs("{\"workflow_runs\":[]}").unwrap().is_empty());
    }

    #[test]
    fn job_counts_are_read_strictly() {
        assert_eq!(parse_job_count("{\"total_count\":0}").unwrap(), 0);
        assert_eq!(parse_job_count("{\"total_count\":0,\"jobs\":[]}").unwrap(), 0);
        assert!(parse_job_count("{}").is_err());
        assert!(parse_job_count("[]").is_err());
    }
}
