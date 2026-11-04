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
/// Canonical-name changes and source notes are retained even when a snapshot
/// was not scanned; only scanned snapshots contribute edges.
pub fn graph_from_snapshots<'a>(
    snapshots: impl IntoIterator<Item = (&'a RepoSnapshot, Role)>,
) -> ChainGraph {
    let mut g = ChainGraph::new();
    for (snap, role) in snapshots {
        let files = snap.workflow_files();
        let status = match (&files, &snap.status) {
            (Err(reason), ScanStatus::Scanned { .. }) => ScanStatus::Unavailable {
                reason: reason.clone(),
            },
            _ => snap.status.clone(),
        };
        g.record_scan(snap.repo.clone(), status);
        if matches!(role, Role::Subject) {
            g.record_subject(snap.repo.clone());
        }
        let canonical = snap.canonical_id();
        if canonical != snap.repo {
            g.record_canonical(snap.repo.clone(), canonical);
        }
        for n in &snap.notes {
            g.notes.push(format!("{}: {n}", snap.repo));
        }
        let Ok(files) = files else {
            continue;
        };
        for f in files {
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
/// from the `origin` remote in `.git/config`. Missing identity or a root that
/// is not a directory becomes [`ScanStatus::Unavailable`]. A genuinely absent
/// workflow directory is a complete empty read. Enumeration, filename, file
/// type or text-read errors make the snapshot unavailable; retained files are
/// partial diagnostics and cannot qualify edges or filename disambiguation.
/// Canonical names are not resolved locally.
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
                canonical: None,
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
            canonical: None,
        };
    }

    let (files, notes) = read_local_workflows(root);
    let status = if notes.is_empty() {
        ScanStatus::Scanned {
            revision: head_revision(root),
            workflow_files: files.len(),
        }
    } else {
        ScanStatus::Unavailable {
            reason: notes.join("; "),
        }
    };
    RepoSnapshot {
        repo,
        status,
        files,
        notes,
        // A checkout cannot tell whether its own name moved; that needs the
        // forge. Leaving this `None` is the honest answer, not "canonical".
        canonical: None,
    }
}

fn read_local_workflows(root: &Path) -> (Vec<WorkflowFile>, Vec<String>) {
    // Inspect each component so a dangling link or a non-directory cannot
    // masquerade as a genuinely absent workflow directory.
    for component in [".github", ".github/workflows"] {
        let path = root.join(component);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (Vec::new(), Vec::new());
            }
            Ok(_) => {
                return (
                    Vec::new(),
                    vec![format!("{component} is not a regular directory")],
                );
            }
            Err(error) => {
                return (
                    Vec::new(),
                    vec![format!("could not inspect {component}: {error}")],
                );
            }
        }
    }

    let dir = root.join(".github/workflows");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) => {
            return (
                Vec::new(),
                vec![format!("could not enumerate .github/workflows: {error}")],
            );
        }
    };
    let mut files = Vec::new();
    let mut notes = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                notes.push(format!("could not read workflow directory entry: {error}"));
                continue;
            }
        };
        let path = entry.path();
        if !matches!(
            path.extension(),
            Some(extension) if extension == "yml" || extension == "yaml"
        ) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            notes.push(format!("workflow filename is not UTF-8: {}", path.display()));
            continue;
        };
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => continue,
            Ok(kind) if kind.is_file() => {}
            Ok(_) => {
                notes.push(format!("{name}: workflow is not a regular file"));
                continue;
            }
            Err(error) => {
                notes.push(format!("could not inspect {name}: {error}"));
                continue;
            }
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => files.push(WorkflowFile {
                path: name.to_string(),
                text,
            }),
            Err(error) => notes.push(format!("could not read {name}: {error}")),
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    (files, notes)
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
    use squabble_core::chains::{analyse, Finding, Severity};

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
            canonical: None,
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
    fn extracted_two_hop_routes_require_both_pins_and_the_called_file() {
        let other_sha = "0000000000000000000000000000000000000000";
        let at_revision = |repo: &str, files: Vec<WorkflowFile>, revision: &str| {
            let mut snapshot = snap(repo, files);
            snapshot.status = ScanStatus::Scanned {
                revision: Some(revision.to_string()),
                workflow_files: snapshot.files.len(),
            };
            snapshot
        };
        for (shared_revision, inner_revision, called_file, expected) in [
            (SHA, SHA, "leaf.yml", Some(Severity::Blocking)),
            (other_sha, SHA, "leaf.yml", Some(Severity::Warning)),
            (SHA, other_sha, "leaf.yml", Some(Severity::Warning)),
            (SHA, SHA, "other.yml", None),
        ] {
            let app = at_revision(
                "me/app",
                vec![wf(
                    "ci.yml",
                    &format!(
                        "jobs:\n  call:\n    uses: me/shared/.github/workflows/reuse.yml@{SHA}\n"
                    ),
                )],
                SHA,
            );
            let shared = at_revision(
                "me/shared",
                vec![wf(
                    "reuse.yml",
                    &format!(
                        "jobs:\n  call:\n    uses: me/inner/.github/workflows/{called_file}@{SHA}\n"
                    ),
                )],
                shared_revision,
            );
            let inner = at_revision(
                "me/inner",
                vec![
                    wf(
                        "leaf.yml",
                        &format!(
                            "jobs:\n  check:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: old/tool@{SHA}\n"
                        ),
                    ),
                    wf(
                        "other.yml",
                        "jobs:\n  check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo no external action\n",
                    ),
                ],
                inner_revision,
            );

            let first = graph_from_snapshots([(&app, Role::Subject)]);
            let shared_role = upstream_roles(&first)
                .into_iter()
                .find(|(repo, _)| repo == &RepoId::new("me/shared"))
                .expect("outer reusable")
                .1;
            let second =
                graph_from_snapshots([(&app, Role::Subject), (&shared, shared_role.clone())]);
            let inner_role = upstream_roles(&second)
                .into_iter()
                .find(|(repo, _)| repo == &RepoId::new("me/inner"))
                .expect("inner reusable")
                .1;
            let mut graph = graph_from_snapshots([
                (&app, Role::Subject),
                (&shared, shared_role),
                (&inner, inner_role),
            ]);
            graph.record_canonical(RepoId::new("old/tool"), RepoId::new("new/tool"));
            let report = analyse(&graph, None);
            let finding = report
                .findings
                .iter()
                .find(|finding| matches!(finding, Finding::MovedUpstream { .. }));
            assert_eq!(
                finding.map(Finding::severity),
                expected,
                "{shared_revision} {inner_revision} {called_file}"
            );
            assert_eq!(report.has_blocking(), expected == Some(Severity::Blocking));
            if let Some(Finding::MovedUpstream {
                consumers,
                unconfirmed_consumers,
                ..
            }) = finding
            {
                assert_eq!(
                    consumers.len(),
                    usize::from(expected == Some(Severity::Blocking))
                );
                assert_eq!(
                    unconfirmed_consumers.len(),
                    usize::from(expected == Some(Severity::Warning))
                );
                let observed = consumers.first().or(unconfirmed_consumers.first()).unwrap();
                assert_eq!(observed.from, RepoId::new("me/inner"));
                assert_eq!(observed.file, "leaf.yml");
            } else {
                assert!(graph
                    .edges
                    .iter()
                    .all(|edge| edge.to != RepoId::new("old/tool")));
            }
        }
    }

    #[test]
    fn snapshot_canonical_names_are_recorded_as_moves() {
        // The GraphQL source compares `nameWithOwner`, so a snapshot can carry
        // the canonical name even though the local source cannot.
        let app = snap(
            "me/app",
            vec![wf(
                "gov.yml",
                "jobs:\n  gov:\n    uses: old/stds/.github/workflows/gov.yml@8f2ee50841e216cd8c192eeb68953118190f105c\n",
            )],
        );
        let mut upstream = snap("old/stds", Vec::new());
        upstream.canonical = Some("new/stds".into());
        let g = graph_from_snapshots([
            (&app, Role::Subject),
            (&upstream, Role::Upstream(BTreeSet::new())),
        ]);
        assert_eq!(
            g.moved_to(&RepoId::new("old/stds")),
            Some(&RepoId::new("new/stds"))
        );
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
            canonical: None,
        };
        let g = graph_from_snapshots([(&s, Role::Subject)]);
        assert!(g.edges.is_empty());
        assert!(g.notes.iter().any(|n| n.contains("partial")));
    }

    struct LocalSource(std::path::PathBuf);

    impl LocalSource {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("squabble-population-{}-{stamp}", std::process::id()));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn workflows(&self) -> std::path::PathBuf {
            let path = self.0.join(".github/workflows");
            std::fs::create_dir_all(&path).unwrap();
            path
        }
    }

    impl Drop for LocalSource {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn complete_local_reads_without_git_keep_their_unknown_revision() {
        let source = LocalSource::new();
        let empty = snapshot_local(&source.0, Some("me/app"));
        assert_eq!(
            empty.status,
            ScanStatus::Scanned {
                revision: None,
                workflow_files: 0,
            }
        );
        assert!(empty.workflow_files().unwrap().is_empty());

        std::fs::create_dir(source.0.join(".github")).unwrap();
        assert!(snapshot_local(&source.0, Some("me/app"))
            .workflow_files()
            .unwrap()
            .is_empty());
        std::fs::write(source.workflows().join("release.yml"), CALLER).unwrap();
        let complete = snapshot_local(&source.0, Some("me/app"));
        assert!(matches!(
            complete.status,
            ScanStatus::Scanned {
                revision: None,
                workflow_files: 1,
            }
        ));
        assert_eq!(complete.workflow_files().unwrap().len(), 1);
        assert!(!graph_from_snapshots([(&complete, Role::Subject)]).edges.is_empty());
    }

    #[test]
    fn non_utf8_workflow_contents_make_the_retained_population_unavailable() {
        let source = LocalSource::new();
        let workflows = source.workflows();
        std::fs::write(workflows.join("release.yml"), CALLER).unwrap();
        std::fs::write(workflows.join("release.yml@nightly.yml"), [0xff, 0xfe]).unwrap();
        let snapshot = snapshot_local(&source.0, Some("me/app"));

        assert!(matches!(snapshot.status, ScanStatus::Unavailable { .. }));
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(snapshot.files[0].path, "release.yml");
        assert!(snapshot.notes.iter().any(|note| note.contains("release.yml@nightly.yml")));
        assert!(snapshot.workflow_files().is_err());
        assert!(graph_from_snapshots([(&snapshot, Role::Subject)]).edges.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn an_unrepresentable_workflow_filename_is_not_silently_omitted() {
        use std::os::unix::ffi::OsStringExt;

        let source = LocalSource::new();
        let name = std::ffi::OsString::from_vec(b"release\xff.yml".to_vec());
        std::fs::write(source.workflows().join(name), CALLER).unwrap();
        let snapshot = snapshot_local(&source.0, Some("me/app"));
        assert!(matches!(snapshot.status, ScanStatus::Unavailable { .. }));
        assert!(snapshot.notes.iter().any(|note| note.contains("filename is not UTF-8")));
        assert!(snapshot.workflow_files().is_err());
    }

    #[test]
    fn a_workflow_directory_path_that_is_a_file_is_not_an_empty_scan() {
        let source = LocalSource::new();
        std::fs::create_dir(source.0.join(".github")).unwrap();
        std::fs::write(source.0.join(".github/workflows"), "not a directory").unwrap();
        let snapshot = snapshot_local(&source.0, Some("me/app"));
        assert!(matches!(snapshot.status, ScanStatus::Unavailable { .. }));
        assert!(snapshot.workflow_files().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_workflow_link_is_unavailable_without_a_permission_fixture() {
        let source = LocalSource::new();
        let workflows = source.workflows();
        std::fs::write(workflows.join("release.yml"), CALLER).unwrap();
        std::os::unix::fs::symlink(
            "missing-target",
            workflows.join("release.yml@nightly.yml"),
        )
        .unwrap();
        let snapshot = snapshot_local(&source.0, Some("me/app"));
        assert!(matches!(snapshot.status, ScanStatus::Unavailable { .. }));
        assert!(snapshot.workflow_files().is_err());
    }

    #[test]
    fn inconsistent_snapshot_counts_do_not_qualify_graph_edges() {
        let mut snapshot = snap("me/app", vec![wf("ci.yml", CALLER)]);
        snapshot.status = ScanStatus::Scanned {
            revision: Some(SHA.into()),
            workflow_files: 2,
        };
        let graph = graph_from_snapshots([(&snapshot, Role::Subject)]);
        assert!(graph.edges.is_empty());
        assert!(matches!(
            graph.scans.get(&RepoId::new("me/app")),
            Some(ScanStatus::Unavailable { .. })
        ));
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
