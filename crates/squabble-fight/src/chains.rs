// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Host side of `chains`: turn raw workflow files into [`Edge`]s, and read a
//! local checkout into a [`RepoSnapshot`].
//!
//! The analysis itself is pure and lives in `squabble_core::chains`. This
//! module does the two estate-aware jobs that core must not:
//!
//! * **extraction** — a tolerant, line-based scan for `uses:` references,
//!   sharing the `uses:` helpers in [`crate::workflows`] so the fight planner
//!   and the chains graph cannot disagree about what a workflow references;
//! * **the local source** — reading `.github/workflows/` from a checkout,
//!   with the repo's identity taken from an explicit slug or its
//!   `origin` remote, and its revision from `.git/HEAD`.
//!
//! The GraphQL source lives in `squabble-forge`; both produce the same
//! [`RepoSnapshot`] shape, so [`graph_from_snapshots`] does not care which
//! source a snapshot came from.

use crate::workflows::{strip_sequence_marker, uses_target};
use squabble_core::chains::{
    ChainGraph, Edge, EdgeKind, Pin, RepoId, RepoSnapshot, ScanStatus, WorkflowFile,
};
use std::collections::BTreeSet;
use std::path::Path;

/// Extract every external `uses:` reference in one workflow file.
///
/// Local actions (`./…`), `docker://` images and malformed targets are
/// skipped. A trailing `# vX.Y.Z` comment is kept as a version hint.
pub fn extract_edges(repo: &RepoId, file: &WorkflowFile) -> Vec<Edge> {
    let mut out = Vec::new();
    for line in file.text.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            continue;
        }
        let u = strip_sequence_marker(t);
        let Some(target) = uses_target(u) else {
            continue;
        };
        if let Some(edge) = parse_target(repo, &file.path, target, comment_of(u)) {
            out.push(edge);
        }
    }
    out
}

/// The text after an unquoted `#` on a `uses:` line, if any.
fn comment_of(u: &str) -> Option<&str> {
    let hash = u.find(" #")?;
    Some(u[hash + 2..].trim())
}

fn parse_target(repo: &RepoId, file: &str, target: &str, comment: Option<&str>) -> Option<Edge> {
    if target.starts_with("./") || target.starts_with("$/") || target.starts_with("docker://") {
        return None;
    }
    let at = target.rfind('@')?;
    let (path, reference) = (&target[..at], &target[at + 1..]);
    if reference.is_empty() {
        return None;
    }
    let mut segs = path.splitn(3, '/');
    let owner = segs.next().filter(|s| !s.is_empty())?;
    let name = segs.next().filter(|s| !s.is_empty())?;
    let sub = segs.next().filter(|s| !s.is_empty()).map(str::to_string);

    let kind = match &sub {
        Some(s) if s.starts_with(".github/workflows/") => EdgeKind::ReusableWorkflow,
        _ => EdgeKind::Action,
    };
    let pin = Pin::parse(reference);
    let version_hint = match (&pin, comment) {
        (Pin::Sha(_), Some(c)) => c
            .split_whitespace()
            .next()
            .filter(|w| w.starts_with('v') || w.starts_with(|ch: char| ch.is_ascii_digit()))
            .map(str::to_string),
        _ => None,
    };

    Some(Edge {
        from: repo.clone(),
        file: file.to_string(),
        to: RepoId::new(format!("{owner}/{name}")),
        kind,
        path: sub,
        pin,
        version_hint,
    })
}

/// Which files of a snapshot contribute edges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// A repo the operator asked about: every workflow file counts.
    Subject,
    /// An upstream probed to follow a chain: only the named reusable workflow
    /// files count (an action repo's own CI is not part of its consumers' CI).
    /// An empty set means "existence probe only — no edges".
    Upstream(BTreeSet<String>),
}

/// Fold snapshots into a graph. Each snapshot's scan status is always
/// recorded; its edges are added according to its [`Role`].
pub fn graph_from_snapshots<'a>(
    snapshots: impl IntoIterator<Item = (&'a RepoSnapshot, Role)>,
) -> ChainGraph {
    let mut g = ChainGraph::new();
    for (snap, role) in snapshots {
        g.record_scan(snap.repo.clone(), snap.status.clone());
        for n in &snap.notes {
            g.notes.push(format!("{}: {n}", snap.repo));
        }
        if !matches!(snap.status, ScanStatus::Scanned { .. }) {
            continue;
        }
        for f in &snap.files {
            let counts = match &role {
                Role::Subject => true,
                Role::Upstream(files) => files.contains(&f.path),
            };
            if counts {
                g.add_edges(extract_edges(&snap.repo, f));
            }
        }
    }
    g
}

/// For each upstream referenced by `graph`'s edges, the reusable workflow
/// files that are actually called on it. Upstreams referenced only as
/// actions map to an empty set (existence probe only).
pub fn upstream_roles(graph: &ChainGraph) -> Vec<(RepoId, Role)> {
    let mut by: std::collections::BTreeMap<RepoId, BTreeSet<String>> = Default::default();
    for e in &graph.edges {
        if graph.scans.contains_key(&e.to) {
            continue;
        }
        let files = by.entry(e.to.clone()).or_default();
        if let (EdgeKind::ReusableWorkflow, Some(p)) = (e.kind, e.path.as_deref()) {
            files.insert(p.trim_start_matches(".github/workflows/").to_string());
        }
    }
    by.into_iter()
        .map(|(r, f)| (r, Role::Upstream(f)))
        .collect()
}

// ---------------------------------------------------------------------------
// Local source
// ---------------------------------------------------------------------------

/// Read a local checkout. `slug` overrides identity; otherwise it is taken
/// from the `origin` remote in `.git/config`. Never errors: an unreadable
/// root becomes [`ScanStatus::Unavailable`] with the reason.
pub fn snapshot_local(root: &Path, slug: Option<&str>) -> RepoSnapshot {
    let repo = match slug.map(str::to_string).or_else(|| origin_slug(root)) {
        Some(s) => RepoId::new(s),
        None => {
            let name = root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            return RepoSnapshot {
                repo: RepoId::new(format!("local/{name}")),
                status: ScanStatus::Unavailable {
                    reason: format!(
                        "no slug given and no GitHub `origin` remote under {}",
                        root.display()
                    ),
                },
                files: Vec::new(),
                notes: Vec::new(),
            };
        }
    };
    if !root.is_dir() {
        return RepoSnapshot {
            repo,
            status: ScanStatus::Unavailable {
                reason: format!("{} is not a directory", root.display()),
            },
            files: Vec::new(),
            notes: Vec::new(),
        };
    }

    let dir = root.join(".github/workflows");
    let mut files = Vec::new();
    let mut notes = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("yml" | "yaml")))
            .collect();
        paths.sort();
        for p in paths {
            let name = p
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("")
                .to_string();
            match std::fs::read_to_string(&p) {
                Ok(text) => files.push(WorkflowFile { path: name, text }),
                Err(e) => notes.push(format!("could not read {name}: {e}")),
            }
        }
    }
    RepoSnapshot {
        repo,
        status: ScanStatus::Scanned {
            revision: head_revision(root),
            workflow_files: files.len(),
        },
        files,
        notes,
    }
}

/// `owner/repo` from `[remote "origin"] url = …github.com[:/]owner/repo(.git)`.
fn origin_slug(root: &Path) -> Option<String> {
    let cfg = std::fs::read_to_string(root.join(".git/config")).ok()?;
    let mut in_origin = false;
    for line in cfg.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_origin = t == "[remote \"origin\"]";
            continue;
        }
        if in_origin {
            if let Some(url) = t
                .strip_prefix("url")
                .map(|r| r.trim_start_matches([' ', '=']))
            {
                return github_slug(url.trim());
            }
        }
    }
    None
}

fn github_slug(url: &str) -> Option<String> {
    let rest = url
        .split_once("github.com/")
        .or_else(|| url.split_once("github.com:"))?
        .1;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut segs = rest.split('/');
    let (o, r) = (segs.next()?, segs.next()?);
    (!o.is_empty() && !r.is_empty()).then(|| format!("{o}/{r}"))
}

/// The commit `.git/HEAD` points at, from loose refs or `packed-refs`.
fn head_revision(root: &Path) -> Option<String> {
    let git = root.join(".git");
    let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(r) = head.strip_prefix("ref: ") else {
        return is_sha(head).then(|| head.to_ascii_lowercase());
    };
    if let Ok(s) = std::fs::read_to_string(git.join(r)) {
        let s = s.trim();
        if is_sha(s) {
            return Some(s.to_ascii_lowercase());
        }
    }
    let packed = std::fs::read_to_string(git.join("packed-refs")).ok()?;
    packed.lines().find_map(|l| {
        let (sha, name) = l.split_once(' ')?;
        (name == r && is_sha(sha)).then(|| sha.to_ascii_lowercase())
    })
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use squabble_core::chains::{analyse, Finding};

    const SHA: &str = "8f2ee50841e216cd8c192eeb68953118190f105c";

    fn wf(path: &str, text: &str) -> WorkflowFile {
        WorkflowFile {
            path: path.into(),
            text: text.into(),
        }
    }

    const CALLER: &str = r#"
name: gov
on: [pull_request]
jobs:
  gov:
    uses: hyperpolymath/standards/.github/workflows/governance-reusable.yml@8f2ee50841e216cd8c192eeb68953118190f105c
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v4.2.2
      - uses: github/codeql-action/init@v4.38.0
      - uses: ./local-helper
      - uses: docker://alpine:3
      # - uses: commented/out@v1
      - name: step
        uses: "quoted/action@main"
"#;

    #[test]
    fn extracts_reusables_actions_pins_and_hints() {
        let me = RepoId::new("me/app");
        let e = extract_edges(&me, &wf("gov.yml", CALLER));
        assert_eq!(e.len(), 4, "{e:#?}");

        assert_eq!(e[0].kind, EdgeKind::ReusableWorkflow);
        assert_eq!(e[0].to, RepoId::new("hyperpolymath/standards"));
        assert_eq!(
            e[0].path.as_deref(),
            Some(".github/workflows/governance-reusable.yml")
        );
        assert_eq!(e[0].pin, Pin::Sha(SHA.into()));

        assert_eq!(e[1].to, RepoId::new("actions/checkout"));
        assert_eq!(e[1].version_hint.as_deref(), Some("v4.2.2"));

        assert_eq!(e[2].kind, EdgeKind::Action);
        assert_eq!(e[2].path.as_deref(), Some("init"));
        assert_eq!(e[2].pin, Pin::Mutable("v4.38.0".into()));

        assert_eq!(e[3].to, RepoId::new("quoted/action"));
        assert_eq!(e[3].pin, Pin::Mutable("main".into()));
    }

    #[test]
    fn github_slugs_parse_from_https_and_ssh() {
        assert_eq!(
            github_slug("https://github.com/hyperpolymath/cicd-squabbler.git").as_deref(),
            Some("hyperpolymath/cicd-squabbler")
        );
        assert_eq!(
            github_slug("git@github.com:hyperpolymath/standards.git").as_deref(),
            Some("hyperpolymath/standards")
        );
        assert_eq!(github_slug("https://gitlab.com/x/y"), None);
    }

    fn snap(repo: &str, files: Vec<WorkflowFile>) -> RepoSnapshot {
        RepoSnapshot {
            repo: RepoId::new(repo),
            status: ScanStatus::Scanned {
                revision: None,
                workflow_files: files.len(),
            },
            files,
            notes: Vec::new(),
        }
    }

    #[test]
    fn upstream_role_only_counts_called_reusable_files() {
        let app = snap("me/app", vec![wf("gov.yml", CALLER)]);
        let g0 = graph_from_snapshots([(&app, Role::Subject)]);
        let roles = upstream_roles(&g0);
        let standards_role = roles
            .iter()
            .find(|(r, _)| r == &RepoId::new("hyperpolymath/standards"))
            .map(|(_, role)| role.clone())
            .unwrap();
        assert_eq!(
            standards_role,
            Role::Upstream(["governance-reusable.yml".to_string()].into())
        );
        // actions/checkout is only an action: existence probe, no files.
        assert!(roles
            .iter()
            .any(|(r, role)| r == &RepoId::new("actions/checkout")
                && role == &Role::Upstream(BTreeSet::new())));

        let standards = snap(
            "hyperpolymath/standards",
            vec![
                wf(
                    "governance-reusable.yml",
                    "jobs:\n  x:\n    steps:\n      - uses: actions/checkout@v4\n",
                ),
                wf(
                    "own-ci.yml",
                    "jobs:\n  y:\n    steps:\n      - uses: some/other-thing@v1\n",
                ),
            ],
        );
        let g = graph_from_snapshots([(&app, Role::Subject), (&standards, standards_role)]);
        assert!(g
            .edges
            .iter()
            .all(|e| e.to != RepoId::new("some/other-thing")));
        // The reusable's checkout@v4 meets app's checkout@SHA: a diamond.
        let rep = analyse(&g, None);
        assert!(rep
            .findings
            .iter()
            .any(|f| matches!(f, Finding::Diamond { target, .. } if target == &RepoId::new("actions/checkout"))));
    }

    #[test]
    fn failed_snapshots_record_status_and_add_no_edges() {
        let s = RepoSnapshot {
            repo: RepoId::new("me/x"),
            status: ScanStatus::Unavailable {
                reason: "rate limited".into(),
            },
            files: vec![wf("ci.yml", CALLER)],
            notes: vec!["partial".into()],
        };
        let g = graph_from_snapshots([(&s, Role::Subject)]);
        assert!(g.edges.is_empty());
        assert!(g.notes.iter().any(|n| n.contains("partial")));
    }

    #[test]
    fn local_snapshot_of_a_missing_dir_is_unavailable() {
        let s = snapshot_local(Path::new("/definitely/not/here"), Some("me/x"));
        assert!(matches!(s.status, ScanStatus::Unavailable { .. }));
    }

    #[test]
    fn local_snapshot_reads_this_repo() {
        // The crate lives two levels under the workspace root.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let s = snapshot_local(&root, Some("hyperpolymath/cicd-squabbler"));
        match s.status {
            ScanStatus::Scanned { workflow_files, .. } => assert!(workflow_files > 0),
            other => panic!("expected scanned, got {other:?}"),
        }
    }
}
