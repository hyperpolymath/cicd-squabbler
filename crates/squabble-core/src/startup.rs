// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Zero-job `startup_failure` attribution — the class that is invisible in PR
//! check lists.
//!
//! A workflow that fails *before any job exists* creates no check run at all,
//! so a PR looks tidy while the required context is simply absent. This module
//! models the two measured causes as **first-class** classes, each with its own
//! move:
//!
//! * [`StartupFailureClass::MovedCoordinate`] — a reusable coordinate names an
//!   owner/repo that has been renamed or transferred. GitHub answers "workflow
//!   was not found" and creates zero jobs. The move is
//!   [`crate::moves::Move::RepointToCanonical`]: rewrite the coordinate to the
//!   canonical `owner/repo` **carrying the same pin**. Repointing is not
//!   re-pinning; this module never resolves a pin.
//! * [`StartupFailureClass::AllowListAdmissionRequired`] — the coordinate (or,
//!   after a move, its *canonical* target) is refused by the caller's
//!   `allowed_actions=selected` allow-list. The move is
//!   [`crate::moves::Move::AllowListAdmissionRequired`]: admit the minimal
//!   `owner/repo@*` pattern. No workflow edit can make the run start.
//!
//! Both moves only let a required check *start*. Neither drops a required
//! context nor weakens a check, so `squabble ≠ bypass` holds: the green still
//! has to come from the check genuinely passing afterwards. This module has no
//! path to [`crate::gate::GateState::Green`] and constructs no
//! [`crate::gate::CheckRun`] — it produces a diagnosis and sequencing advice,
//! nothing else.
//!
//! # Why names, not pins
//!
//! A moved owner is a *name* problem. Resolving the pin (or trusting a tag)
//! cannot see it: the revision may be perfectly valid in the new repo while the
//! coordinate still names the old one. The resolver therefore compares
//! canonical `full_name`s (REST follows redirects), and a difference is the
//! finding. [`crate::chains`] applies the same rule estate-wide before a run
//! proves it.
//!
//! # Fail-closed
//!
//! With no allow-list posture, or a coordinate that could not be checked, the
//! attribution is [`StartupFailureClass::Unattributed`] and **no move** is
//! proposed. An unreadable fact is never turned into a diagnosis.

use crate::chains::{EdgeKind, Pin};
use crate::gate::MissingCause;
use crate::moves::Move;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// The caller's allow-list
// ---------------------------------------------------------------------------

/// The caller repository's `selected`-actions allow-list.
///
/// Only meaningful under `allowed_actions=selected`; hosts pass `None` for any
/// other posture (under `all` nothing external is refused, and under
/// `local_only` *everything* external is — a different class). Pattern matching
/// mirrors the estate preflight (`standards` `scripts/check-allowed-actions.sh`)
/// and is shared by every other reader so the two cannot disagree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowList {
    /// `patterns_allowed`, as GitHub returns them (may include an `@ref`).
    #[serde(default)]
    pub patterns: Vec<String>,
    /// `github_owned_allowed`: GitHub-owned `actions/*` and `github/*` pass
    /// without an explicit pattern.
    #[serde(default)]
    pub github_owned_allowed: bool,
}

impl AllowList {
    /// Is `owner_repo` (normalised, no `@ref`) permitted? An exact
    /// `owner/repo`, an owner-wide `owner/*`, or a trailing-`*` prefix form all
    /// cover. Patterns may carry an `@ref`, which is ignored: the allow-list is
    /// a name rule, not a revision rule.
    /// Matching is case-sensitive; GitHub-owned names also pass when
    /// `github_owned_allowed` is set.
    pub fn covers(&self, owner_repo: &str) -> bool {
        let owner = owner_repo.split('/').next().unwrap_or("");
        if self.github_owned_allowed && matches!(owner, "actions" | "github") {
            return true;
        }
        self.patterns.iter().any(|p| {
            let base = p.split('@').next().unwrap_or(p.as_str());
            base == owner_repo
                || base == format!("{owner}/*")
                || base
                    .strip_suffix('*')
                    .is_some_and(|prefix| !prefix.is_empty() && owner_repo.starts_with(prefix))
        })
    }

    /// The minimal pattern that would admit `owner_repo`, or `None` when it is
    /// already covered. This is the *exact* string an owner must add to the
    /// selected-actions allow-list — never a broader `owner/*` than needed.
    pub fn missing_pattern(&self, owner_repo: &str) -> Option<String> {
        (!self.covers(owner_repo)).then(|| format!("{owner_repo}@*"))
    }

    /// Whether there are no explicit patterns. GitHub-owned coordinates may
    /// still be covered when `github_owned_allowed` is set.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Coordinates and their canonical resolution
// ---------------------------------------------------------------------------

/// A `uses:` coordinate exactly as written: `owner/repo[/subpath]@ref`.
///
/// The written text is kept verbatim (`written`) because that is what the
/// attribution must quote back; the parsed parts exist only so a repoint can
/// rebuild the coordinate with the same subpath and the same pin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coordinate {
    /// The raw coordinate, as it appears after `uses:`.
    pub written: String,
    /// `owner/repo`, as written (case preserved).
    pub owner_repo: String,
    /// Subpath inside the repo, if any (an action's directory or the reusable
    /// workflow's path).
    #[serde(default)]
    pub path: Option<String>,
    /// The pin, untouched. `Pin::Sha` is stored lowercased by [`Pin::parse`];
    /// that is the same immutable revision, never a re-resolution.
    pub pin: Pin,
}

impl Coordinate {
    /// Parse `owner/repo[/subpath]@ref`. Returns `None` for a local action, a
    /// `docker://` image, or anything without an `owner/repo` head and a
    /// non-empty ref.
    pub fn parse(raw: &str) -> Option<Self> {
        if raw.starts_with("./") || raw.starts_with("$/") || raw.starts_with("docker://") {
            return None;
        }
        let at = raw.rfind('@')?;
        let (path, reference) = (&raw[..at], &raw[at + 1..]);
        if reference.is_empty() {
            return None;
        }
        let mut segs = path.splitn(3, '/');
        let owner = segs.next().filter(|s| !s.is_empty())?;
        let repo = segs.next().filter(|s| !s.is_empty())?;
        let sub = segs.next().filter(|s| !s.is_empty());
        Some(Self {
            written: raw.to_string(),
            owner_repo: format!("{owner}/{repo}"),
            path: sub.map(str::to_string),
            pin: Pin::parse(reference),
        })
    }

    /// The same coordinate rewritten under a canonical `owner/repo`: subpath
    /// preserved, **pin carried unchanged**.
    pub fn rewritten(&self, canonical: &str) -> String {
        let sub = self
            .path
            .as_deref()
            .map(|p| format!("/{p}"))
            .unwrap_or_default();
        format!("{canonical}{sub}@{}", self.pin.as_str())
    }
}

/// What a REST read of a repository endpoint concluded about its canonical
/// name. REST follows redirects, so the endpoint answers for the *current*
/// repo; comparing `full_name` with the requested name is the only way to see
/// a move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "resolution", rename_all = "kebab-case")]
pub enum CanonicalResolution {
    /// Resolves under the name as written.
    Canonical,
    /// Resolves to a different `full_name` — renamed or transferred. The
    /// reference relies on a redirect, and a lapsed redirect (or a re-registered
    /// old name) is a zero-job `startup_failure`.
    Moved { canonical: String },
    /// The endpoint answered 404: deleted, or not visible to this reader.
    NotFound,
    /// The name could not be checked (no transport, a redirect without a name,
    /// a rate limit). Nothing is concluded about the repo.
    Unverified { reason: String },
}

impl CanonicalResolution {
    /// The target `owner/repo` for a move; `None` for all other resolutions,
    /// including a name that is already canonical.
    pub fn canonical(&self) -> Option<&str> {
        match self {
            Self::Moved { canonical } => Some(canonical),
            _ => None,
        }
    }

    /// Whether resolution identified a renamed or transferred repository.
    pub fn is_moved(&self) -> bool {
        matches!(self, Self::Moved { .. })
    }

    /// One-line human description; `None` for the uninteresting case.
    pub fn describe(&self) -> Option<String> {
        match self {
            Self::Canonical => None,
            Self::Moved { canonical } => Some(format!(
                "resolves to `{canonical}` (renamed or transferred) — repoint, same pin"
            )),
            Self::NotFound => Some("does not resolve (404) — dead for this reader".to_string()),
            Self::Unverified { reason } => Some(format!("unverified — {reason}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Attribution
// ---------------------------------------------------------------------------

/// The two first-class zero-job classes, plus the honest non-answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StartupFailureClass {
    /// The reusable coordinate's owner/repo moved. GitHub reports "workflow was
    /// not found" and creates zero jobs. Move: repoint to the canonical
    /// `owner/repo`, same pin.
    MovedCoordinate,
    /// The coordinate is refused by the caller's `selected` allow-list. GitHub
    /// refuses the run at startup (jobs=0). Move: admit the `owner/repo@*`
    /// pattern the report names.
    AllowListAdmissionRequired,
    /// A zero-job startup failure was observed but cannot be attributed from
    /// the facts available. No move is proposed (fail-closed).
    Unattributed,
}

impl StartupFailureClass {
    /// A human-readable class label for reports.
    pub fn label(&self) -> &'static str {
        match self {
            Self::MovedCoordinate => "moved coordinate",
            Self::AllowListAdmissionRequired => "allow-list admission required",
            Self::Unattributed => "unattributed",
        }
    }
}

/// One zero-job run's attribution for one coordinate: repo, workflow,
/// coordinate as written, canonical target, the required allow-list pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZeroJobAttribution {
    /// The caller repo whose run failed (`owner/repo`).
    pub repo: String,
    /// The workflow file, relative to `.github/workflows/`.
    pub workflow: String,
    /// The coordinate exactly as written in that workflow.
    pub coordinate: String,
    /// The canonical `owner/repo` the resolver observed, when it observed one.
    #[serde(default)]
    pub canonical: Option<String>,
    /// The `owner/repo@*` pattern that must be admitted for this coordinate to
    /// start, when the live allow-list does not cover it.
    #[serde(default)]
    pub required_allow_list_pattern: Option<String>,
    pub class: StartupFailureClass,
    /// Why this class was chosen, quoting only observed facts.
    pub evidence: String,
    /// Ordered next steps — diagnosis and sequencing only, never a green.
    #[serde(default)]
    pub sequencing: Vec<String>,
}

impl ZeroJobAttribution {
    /// The single legitimate move for this class, if one is proposable.
    ///
    /// `None` for [`StartupFailureClass::Unattributed`], and for a
    /// `MovedCoordinate` without a canonical target or a parseable coordinate.
    /// Admission also requires a recorded allow-list pattern.
    pub fn proposed_move(&self) -> Option<Move> {
        match self.class {
            StartupFailureClass::MovedCoordinate => {
                let canonical = self.canonical.clone()?;
                let parsed = Coordinate::parse(&self.coordinate)?;
                Some(Move::RepointToCanonical {
                    written: self.coordinate.clone(),
                    canonical,
                    pin: parsed.pin.as_str().to_string(),
                })
            }
            StartupFailureClass::AllowListAdmissionRequired => {
                let pattern = self.required_allow_list_pattern.clone()?;
                let effective = self
                    .canonical
                    .clone()
                    .or_else(|| Coordinate::parse(&self.coordinate).map(|c| c.owner_repo))
                    .unwrap_or_else(|| self.coordinate.clone());
                Some(Move::AllowListAdmissionRequired {
                    written: self.coordinate.clone(),
                    canonical: effective,
                    pattern,
                })
            }
            StartupFailureClass::Unattributed => None,
        }
    }

    /// The gate-level [`MissingCause`] this attribution explains, for a
    /// required context that never reported.
    ///
    /// This is the bridge between the zero-job report and the gate model: a
    /// diagnosed cause can explain a `Missing` requirement, but (as
    /// `squabble-core::gate` proves) no cause is a path to `Green`. `None` for
    /// an unattributed run — an unknown cause is not invented.
    pub fn missing_cause(&self) -> Option<MissingCause> {
        match self.class {
            StartupFailureClass::MovedCoordinate => Some(MissingCause::MovedUpstreamCoordinate),
            StartupFailureClass::AllowListAdmissionRequired => {
                Some(MissingCause::AllowListAdmissionRequired)
            }
            StartupFailureClass::Unattributed => None,
        }
    }

    /// One-line human summary, naming the written coordinate and canonical
    /// target so the report is attributable without the JSON.
    pub fn describe(&self) -> String {
        let target = self
            .canonical
            .as_deref()
            .map(|c| format!(" → canonical `{c}`"))
            .unwrap_or_default();
        let pattern = self
            .required_allow_list_pattern
            .as_deref()
            .map(|p| format!(" (needs allow-list `{p}`)"))
            .unwrap_or_default();
        format!(
            "{}: `{}`{} in `{}`{} — {}",
            self.class.label(),
            self.coordinate,
            target,
            self.workflow,
            pattern,
            self.evidence
        )
    }
}

/// Attribute one observed zero-job `startup_failure` to one coordinate.
///
/// The caller must have observed the failure (the run concluded
/// `STARTUP_FAILURE` with no jobs); this function does not invent one. The
/// decisive inputs are the *canonical resolution* of the coordinate (names, not
/// pins) and the caller's live allow-list (or `None` when the posture is not
/// `selected`/unreadable).
///
/// Ordering: a moved **reusable** coordinate wins over admission, because the
/// run cannot even find the workflow to refuse; its attribution still carries
/// the canonical pattern, so the operator sees both steps in order. A moved
/// **action** repo is *not* a workflow-startup class (it fails its own job), so
/// only admission or `Unattributed` can describe it.
pub fn attribute_zero_job(
    repo: &str,
    workflow: &str,
    written: &str,
    kind: EdgeKind,
    resolution: &CanonicalResolution,
    allow_list: Option<&AllowList>,
) -> ZeroJobAttribution {
    let parsed = Coordinate::parse(written);
    let written_repo = parsed
        .as_ref()
        .map(|c| c.owner_repo.clone())
        .unwrap_or_else(|| written.to_string());
    let canonical = resolution.canonical().map(str::to_string);
    let effective = canonical.clone().unwrap_or_else(|| written_repo.clone());
    let required_pattern = allow_list.and_then(|a| a.missing_pattern(&effective));

    // A dead coordinate has no canonical target to repoint to. That is a dead
    // upstream (chains' blocking finding), not an admission fix — say so and
    // propose nothing.
    if matches!(resolution, CanonicalResolution::NotFound) {
        return ZeroJobAttribution {
            repo: repo.to_string(),
            workflow: workflow.to_string(),
            coordinate: written.to_string(),
            canonical: None,
            required_allow_list_pattern: required_pattern,
            class: StartupFailureClass::Unattributed,
            evidence: format!(
                "`{written}` does not resolve (404): the coordinate is dead, so there is no \
                 canonical name to repoint to. A dead upstream creates no check run at all — \
                 see `squabble chains`' dead-upstream finding."
            ),
            sequencing: Vec::new(),
        };
    }

    // (a) Moved reusable coordinate — "workflow was not found", zero jobs.
    if kind == EdgeKind::ReusableWorkflow {
        if let Some(canon) = &canonical {
            let pin = parsed
                .as_ref()
                .map(|c| c.pin.as_str().to_string())
                .unwrap_or_default();
            let mut sequencing = vec![format!(
                "repoint `{written}` to `{}` — the pin `{pin}` is carried unchanged; \
                 repointing is not re-pinning",
                parsed
                    .as_ref()
                    .map(|c| c.rewritten(canon))
                    .unwrap_or_else(|| format!("{canon}@{pin}"))
            )];
            if let Some(p) = &required_pattern {
                sequencing.push(format!(
                    "then admit `{p}` in the caller's selected-actions allow-list — a repoint \
                     alone can still be refused at startup"
                ));
            }
            sequencing.push(
                "re-run the workflow afterwards; nothing here makes the check green".to_string(),
            );
            return ZeroJobAttribution {
                repo: repo.to_string(),
                workflow: workflow.to_string(),
                coordinate: written.to_string(),
                canonical: Some(canon.clone()),
                required_allow_list_pattern: required_pattern,
                class: StartupFailureClass::MovedCoordinate,
                evidence: format!(
                    "`{written}` resolves to canonical `{canon}` (renamed or transferred). \
                     Reusable coordinates are resolved before any job exists, so the stale name \
                     is a zero-job `startup_failure` (\"workflow was not found\") that no PR \
                     check list shows."
                ),
                sequencing,
            };
        }
    }

    // (b) The coordinate is not admitted by the caller's selected allow-list.
    if let Some(pattern) = required_pattern {
        let empty = allow_list.is_some_and(AllowList::is_empty);
        let mut sequencing = vec![format!(
            "admit `{pattern}` to the caller's `selected` actions allow-list{}; no workflow \
             edit can make this run start",
            if empty {
                " (the list is EMPTY, so every external `uses:` is refused)"
            } else {
                ""
            }
        )];
        if resolution.is_moved() {
            sequencing.insert(
                0,
                format!(
                    "the pattern must name the canonical target `{effective}`, not the \
                     moved coordinate `{written_repo}`"
                ),
            );
        }
        if empty {
            sequencing.push(
                "the estate posture of record is `allowed_actions=all` with \
                 `sha_pinning_required=true` KEPT — adopt that, or enumerate every needed \
                 `owner/repo@*` pattern"
                    .to_string(),
            );
        }
        sequencing.push(
            "re-run the workflow afterwards; admission only lets the check start".to_string(),
        );
        // The refusal is a fact about the allow-list; the name is stated only as
        // strongly as it was established, so an unverified canonical name is not
        // dressed up as a resolved one.
        let named = match resolution {
            CanonicalResolution::Moved { .. } => format!("resolves to canonical `{effective}`"),
            CanonicalResolution::Unverified { .. } => format!(
                "is named `{effective}` (its canonical name could not be checked, so the name \
                 as written is the only one this report can vouch for)"
            ),
            _ => format!("resolves to `{effective}`"),
        };
        return ZeroJobAttribution {
            repo: repo.to_string(),
            workflow: workflow.to_string(),
            coordinate: written.to_string(),
            canonical,
            required_allow_list_pattern: Some(pattern),
            class: StartupFailureClass::AllowListAdmissionRequired,
            evidence: format!(
                "`{written}` {named}, but the caller's `selected` actions allow-list has no \
                 pattern covering it. GitHub refuses the run at startup with zero jobs, so \
                 nothing reports on the PR."
            ),
            sequencing,
        };
    }

    // Fail-closed: a zero-job failure we cannot attribute from these facts.
    let evidence = match resolution {
        CanonicalResolution::Moved { .. } if kind == EdgeKind::Action => format!(
            "`{written}` names a moved action repo; a moved action fails its own job rather \
             than preventing workflow startup, so it is not a zero-job class."
        ),
        CanonicalResolution::Unverified { reason } => format!(
            "`{written}` is admitted by the live allow-list (or none applies), and its \
             canonical name could not be checked ({reason}). The cause is outside this \
             taxonomy — diagnose from the run's own workflow before changing anything."
        ),
        _ => format!(
            "`{written}` resolves canonically and the live allow-list admits it (or no \
             `selected` posture applies). The cause is outside this taxonomy — diagnose from \
             the run's own workflow before changing anything."
        ),
    };
    ZeroJobAttribution {
        repo: repo.to_string(),
        workflow: workflow.to_string(),
        coordinate: written.to_string(),
        canonical,
        required_allow_list_pattern: None,
        class: StartupFailureClass::Unattributed,
        evidence,
        sequencing: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Runs and reports
// ---------------------------------------------------------------------------

/// One Actions run that concluded `STARTUP_FAILURE` — a run with no jobs, which
/// therefore cannot appear as a red check on any PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZeroJobRun {
    pub id: u64,
    /// Workflow file relative to `.github/workflows/`.
    pub workflow_file: String,
    /// The workflow's display name, when the API reports one.
    #[serde(default)]
    pub workflow_name: Option<String>,
    /// Jobs the run created, when the host counted them. `None` means the count
    /// could not be read — absence of evidence, not evidence of zero.
    #[serde(default)]
    pub jobs: Option<u64>,
}

/// One zero-job run and every coordinate attribution made for its workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZeroJobRunAttribution {
    pub run_id: u64,
    pub workflow: String,
    #[serde(default)]
    pub jobs: Option<u64>,
    pub attributions: Vec<ZeroJobAttribution>,
}

/// The report for one repository: its zero-job runs, their attributions, and
/// every caveat the probes hit. A report with any run is a finding — there is
/// no representation of "zero-job runs, but green".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZeroJobReport {
    pub repo: String,
    #[serde(default)]
    pub runs: Vec<ZeroJobRunAttribution>,
    #[serde(default)]
    pub notes: Vec<String>,
}

impl ZeroJobReport {
    /// True when at least one zero-job run was observed. Always a finding,
    /// whatever the attribution quality: an unrecorded run is still a run that
    /// never reported.
    pub fn has_findings(&self) -> bool {
        !self.runs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chains::Pin;

    const SHA: &str = "8f2ee50841e216cd8c192eeb68953118190f105c";
    const CALLER: &str = "hyperpolymath/hypatia";
    const WF: &str = "hypatia-scan.yml";

    fn moved(to: &str) -> CanonicalResolution {
        CanonicalResolution::Moved {
            canonical: to.to_string(),
        }
    }

    fn allow(patterns: &[&str]) -> AllowList {
        AllowList {
            patterns: patterns.iter().map(|p| p.to_string()).collect(),
            github_owned_allowed: true,
        }
    }

    #[test]
    fn a_moved_reusable_is_a_first_class_class_with_a_same_pin_repoint() {
        let written = format!(
            "hyperpolymath/standards-old/.github/workflows/governance-reusable.yml@{SHA}"
        );
        let a = attribute_zero_job(
            CALLER,
            WF,
            &written,
            EdgeKind::ReusableWorkflow,
            &moved("hyperpolymath/standards"),
            None,
        );
        assert_eq!(a.class, StartupFailureClass::MovedCoordinate);
        assert_eq!(a.canonical.as_deref(), Some("hyperpolymath/standards"));
        assert_eq!(a.repo, CALLER);
        assert_eq!(a.workflow, WF);
        assert_eq!(a.coordinate, written);
        assert!(a.required_allow_list_pattern.is_none());
        assert!(a.evidence.contains("workflow was not found"));
        match a.proposed_move() {
            Some(Move::RepointToCanonical {
                written: w,
                canonical,
                pin,
            }) => {
                assert_eq!(w, written);
                assert_eq!(canonical, "hyperpolymath/standards");
                // Same pin, untouched.
                assert_eq!(pin, SHA);
                assert_eq!(Pin::parse(&pin), Pin::Sha(SHA.to_string()));
            }
            other => panic!("expected repoint, got {other:?}"),
        }
    }

    #[test]
    fn repointing_preserves_the_subpath_and_the_pin_exactly() {
        let raw = format!("old/stds/.github/workflows/gov.yml@{SHA}");
        let c = Coordinate::parse(&raw).expect("parse");
        assert_eq!(c.owner_repo, "old/stds");
        assert_eq!(
            c.path.as_deref(),
            Some(".github/workflows/gov.yml")
        );
        assert_eq!(
            c.rewritten("new/stds"),
            format!("new/stds/.github/workflows/gov.yml@{SHA}")
        );
        // A tag pin is carried just as literally — never resolved on the way.
        let tag = Coordinate::parse("old/stds/.github/workflows/gov.yml@v2").unwrap();
        assert_eq!(tag.rewritten("new/stds"), "new/stds/.github/workflows/gov.yml@v2");
    }

    #[test]
    fn local_actions_and_docker_images_are_not_coordinates() {
        assert!(Coordinate::parse("./local/action").is_none());
        assert!(Coordinate::parse("$/sibling/action@v1").is_none());
        assert!(Coordinate::parse("docker://alpine:3").is_none());
        assert!(Coordinate::parse("owner/repo").is_none());
    }

    #[test]
    fn a_non_admitted_canonical_coordinate_requires_admission() {
        let written = "hyperpolymath/standards/.github/workflows/gov.yml@main";
        let a = attribute_zero_job(
            CALLER,
            WF,
            written,
            EdgeKind::ReusableWorkflow,
            &CanonicalResolution::Canonical,
            Some(&allow(&["actions/*"])),
        );
        assert_eq!(a.class, StartupFailureClass::AllowListAdmissionRequired);
        assert_eq!(
            a.required_allow_list_pattern.as_deref(),
            Some("hyperpolymath/standards@*")
        );
        assert!(a
            .sequencing
            .iter()
            .any(|s| s.contains("no workflow edit can make this run start")));
        assert_eq!(
            a.proposed_move(),
            Some(Move::AllowListAdmissionRequired {
                written: written.to_string(),
                canonical: "hyperpolymath/standards".to_string(),
                pattern: "hyperpolymath/standards@*".to_string(),
            })
        );
    }

    #[test]
    fn an_empty_allow_list_refuses_every_external_and_says_so() {
        let a = attribute_zero_job(
            CALLER,
            WF,
            "hyperpolymath/standards/.github/workflows/gov.yml@v1",
            EdgeKind::ReusableWorkflow,
            &CanonicalResolution::Canonical,
            Some(&allow(&[])),
        );
        assert_eq!(a.class, StartupFailureClass::AllowListAdmissionRequired);
        assert!(a
            .sequencing
            .first()
            .is_some_and(|s| s.contains("EMPTY")));
        assert!(a.required_allow_list_pattern.is_some());
    }

    #[test]
    fn a_moved_reusable_that_is_also_not_admitted_reports_both_in_order() {
        let written = "old/stds/.github/workflows/gov.yml@v1";
        let a = attribute_zero_job(
            CALLER,
            WF,
            written,
            EdgeKind::ReusableWorkflow,
            &moved("new/stds"),
            Some(&allow(&[])),
        );
        // Repoint first: the workflow cannot even be found under the old name.
        assert_eq!(a.class, StartupFailureClass::MovedCoordinate);
        assert_eq!(
            a.required_allow_list_pattern.as_deref(),
            Some("new/stds@*")
        );
        assert!(a.sequencing[0].contains("repoint"));
        assert!(a.sequencing[1].contains("new/stds@*"));
        assert!(matches!(
            a.proposed_move(),
            Some(Move::RepointToCanonical { .. })
        ));
    }

    #[test]
    fn a_moved_action_is_not_a_workflow_startup_class_and_proposes_nothing() {
        // A moved action repo fails its own job; attributing it as a zero-job
        // workflow startup would be an invented diagnosis.
        let a = attribute_zero_job(
            CALLER,
            WF,
            "old/action@v1",
            EdgeKind::Action,
            &moved("new/action"),
            Some(&allow(&["new/action@*"])),
        );
        assert_eq!(a.class, StartupFailureClass::Unattributed);
        assert!(a.proposed_move().is_none());
        assert!(a.evidence.contains("moved action"));
    }

    #[test]
    fn an_unadmitted_moved_action_is_an_admission_finding() {
        let a = attribute_zero_job(
            CALLER,
            WF,
            "old/action@v1",
            EdgeKind::Action,
            &moved("new/action"),
            Some(&allow(&["actions/*"])),
        );
        assert_eq!(a.class, StartupFailureClass::AllowListAdmissionRequired);
        assert_eq!(a.required_allow_list_pattern.as_deref(), Some("new/action@*"));
    }

    #[test]
    fn a_dead_coordinate_is_unattributed_with_no_move() {
        let a = attribute_zero_job(
            CALLER,
            WF,
            "gone/action@v1",
            EdgeKind::ReusableWorkflow,
            &CanonicalResolution::NotFound,
            Some(&allow(&[])),
        );
        assert_eq!(a.class, StartupFailureClass::Unattributed);
        assert!(a.proposed_move().is_none());
        assert!(a.evidence.contains("dead"));
    }

    #[test]
    fn no_allow_list_posture_means_no_admission_is_invented() {
        let a = attribute_zero_job(
            CALLER,
            WF,
            "hyperpolymath/standards/.github/workflows/gov.yml@v1",
            EdgeKind::ReusableWorkflow,
            &CanonicalResolution::Canonical,
            None,
        );
        assert_eq!(a.class, StartupFailureClass::Unattributed);
        assert!(a.proposed_move().is_none());
    }

    #[test]
    fn an_unverified_name_still_allows_an_admission_finding() {
        // The allow-list fact stands on its own: the coordinate is not admitted,
        // so admission is required whether or not the name has moved.
        let a = attribute_zero_job(
            CALLER,
            WF,
            "someone/thing/.github/workflows/x.yml@v1",
            EdgeKind::ReusableWorkflow,
            &CanonicalResolution::Unverified {
                reason: "no REST transport".into(),
            },
            Some(&allow(&[])),
        );
        assert_eq!(a.class, StartupFailureClass::AllowListAdmissionRequired);
    }

    #[test]
    fn covered_and_canonical_is_unattributed_not_a_fabricated_cause() {
        let a = attribute_zero_job(
            CALLER,
            WF,
            "hyperpolymath/standards/.github/workflows/gov.yml@v1",
            EdgeKind::ReusableWorkflow,
            &CanonicalResolution::Canonical,
            Some(&allow(&["hyperpolymath/standards@*"])),
        );
        assert_eq!(a.class, StartupFailureClass::Unattributed);
        assert!(a.evidence.contains("outside this taxonomy"));
    }

    #[test]
    fn allow_list_matching_is_the_estate_preflight_semantics() {
        let a = allow(&["owner/repo@*", "wide/*", "prefix-*"]);
        assert!(a.covers("owner/repo"));
        assert!(a.covers("wide/anything"));
        assert!(a.covers("prefix-ix/thing"));
        assert!(!a.covers("other/repo"));
        // GitHub-owned refs pass only when the flag says so.
        let no_gh = AllowList {
            patterns: vec![],
            github_owned_allowed: false,
        };
        assert!(!no_gh.covers("actions/checkout"));
        assert!(a.covers("actions/checkout"));
        assert_eq!(no_gh.missing_pattern("actions/checkout").as_deref(), Some("actions/checkout@*"));
    }

    #[test]
    fn the_two_classes_map_to_distinct_gate_causes_and_unattributed_maps_to_none() {
        let moved = attribute_zero_job(
            CALLER,
            WF,
            "old/stds/.github/workflows/gov.yml@v1",
            EdgeKind::ReusableWorkflow,
            &moved_resolution(),
            None,
        );
        let admission = attribute_zero_job(
            CALLER,
            WF,
            "hyperpolymath/standards/.github/workflows/gov.yml@v1",
            EdgeKind::ReusableWorkflow,
            &CanonicalResolution::Canonical,
            Some(&allow(&[])),
        );
        let unattributed = attribute_zero_job(
            CALLER,
            WF,
            "hyperpolymath/standards/.github/workflows/gov.yml@v1",
            EdgeKind::ReusableWorkflow,
            &CanonicalResolution::Canonical,
            None,
        );
        assert_eq!(
            moved.missing_cause(),
            Some(MissingCause::MovedUpstreamCoordinate)
        );
        assert_eq!(
            admission.missing_cause(),
            Some(MissingCause::AllowListAdmissionRequired)
        );
        assert_ne!(moved.missing_cause(), admission.missing_cause());
        assert_eq!(unattributed.missing_cause(), None);
    }

    fn moved_resolution() -> CanonicalResolution {
        CanonicalResolution::Moved {
            canonical: "new/stds".into(),
        }
    }

    #[test]
    fn an_unattributed_attribution_never_yields_a_move() {
        let a = ZeroJobAttribution {
            repo: CALLER.into(),
            workflow: WF.into(),
            coordinate: "x/y@v1".into(),
            canonical: None,
            required_allow_list_pattern: None,
            class: StartupFailureClass::Unattributed,
            evidence: "unknown".into(),
            sequencing: Vec::new(),
        };
        assert!(a.proposed_move().is_none());
    }

    #[test]
    fn report_round_trips_and_never_reads_green() {
        let a = attribute_zero_job(
            CALLER,
            WF,
            &format!("old/stds/.github/workflows/gov.yml@{SHA}"),
            EdgeKind::ReusableWorkflow,
            &moved("new/stds"),
            None,
        );
        let report = ZeroJobReport {
            repo: CALLER.into(),
            runs: vec![ZeroJobRunAttribution {
                run_id: 40_451_601_571,
                workflow: WF.into(),
                jobs: Some(0),
                attributions: vec![a],
            }],
            notes: vec!["note".into()],
        };
        assert!(report.has_findings());
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"class\":\"moved-coordinate\""));
        let back: ZeroJobReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
        assert!(ZeroJobReport {
            repo: CALLER.into(),
            runs: Vec::new(),
            notes: Vec::new(),
        }
        .runs
        .is_empty());
    }
}
