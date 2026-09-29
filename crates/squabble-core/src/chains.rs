// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `chains` — pure analysis of cross-repo CI dependency chains.
//!
//! A required check in one repo is often *produced* by code in another: a
//! reusable workflow, or an action pinned by SHA or tag. Doctrine #14
//! ("solutions at source — trace and respect every up- and down-stream") needs
//! that graph made explicit. This module takes an already-extracted graph
//! (edges plus what was scanned) and reports on it. It performs **no IO and no
//! parsing** — hosts extract edges (see `squabble-fight::chains`) from local
//! checkouts or the GitHub GraphQL API and hand them in.
//!
//! # What it reports
//!
//! * [`Finding::Cycle`] — repos whose CI depends on each other in a loop, so
//!   no landing order exists. *Blocking.*
//! * [`Finding::DeadUpstream`] — a target repo was probed and does not exist
//!   (deleted or renamed away). A dead `uses:` produces no check run at all,
//!   so the board reads green while the job never ran. *Blocking.*
//! * [`Finding::Diamond`] — one consumer's CI receives the same upstream at two
//!   different pins, at least one of them through a reusable workflow. *Warning.*
//! * [`Finding::PinSkew`] — one upstream is pinned at several refs across the
//!   scanned consumers. *Warning.*
//! * [`Finding::MutablePins`] — a consumer pins upstreams by tag or branch, not
//!   by full SHA. *Warning.*
//!
//! Plus two structural answers: a deterministic [`ChainsReport::landing_order`]
//! (upstreams first; `None` when a cycle makes one impossible) and the
//! [`ChainsReport::critical_chain`], the longest dependency chain.
//!
//! # Fail-closed, no overclaim
//!
//! A repo that was referenced but never probed is listed on the
//! [`ChainsReport::frontier`], not assumed healthy. A repo whose scan failed
//! keeps its reason. A diamond discovered through an upstream read at a
//! revision other than the one pinned says so ([`RevisionBasis`]).
//!
//! # Tropical note
//!
//! [`ChainGraph::critical_path`] is a longest path over a DAG: evaluation in
//! the max-plus semiring. Today every hop weighs 1, so it measures chain
//! depth. Once `retro` records CI durations, the same function with measured
//! weights gives time-to-green along the slowest chain.
//!
//! # Why the SPARK theorem is untouched
//!
//! Nothing here reads or constructs a [`crate::gate::CheckRun`] or
//! [`crate::gate::GateState`]. A chains report is not a gate verdict and can
//! never read as green.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// A repository `owner/repo`. GitHub resolves owner and repo names
/// case-insensitively, so identity here is case-insensitive too: equality,
/// ordering and hashing use the ASCII-lowercased form. The original spelling
/// is kept for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepoId(String);

impl RepoId {
    pub fn new(slug: impl Into<String>) -> Self {
        Self(slug.into())
    }

    /// The spelling as first seen, for display.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn key(&self) -> String {
        self.0.to_ascii_lowercase()
    }
}

impl PartialEq for RepoId {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}
impl Eq for RepoId {}
impl Hash for RepoId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}
impl PartialOrd for RepoId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RepoId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().cmp(&other.key())
    }
}
impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Edges
// ---------------------------------------------------------------------------

/// How a consumer refers to an upstream revision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "pin", content = "ref", rename_all = "kebab-case")]
pub enum Pin {
    /// A full 40-hex commit SHA (stored lowercased). Immutable.
    Sha(String),
    /// A tag or a branch. Offline the two cannot be told apart; both can move.
    Mutable(String),
}

impl Pin {
    /// Classify a raw `@ref`. Only a full-length hex SHA is immutable; a short
    /// SHA is treated as mutable, matching GitHub's `sha_pinning_required`.
    pub fn parse(reference: &str) -> Self {
        if reference.len() == 40 && reference.chars().all(|c| c.is_ascii_hexdigit()) {
            Pin::Sha(reference.to_ascii_lowercase())
        } else {
            Pin::Mutable(reference.to_string())
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Pin::Sha(s) | Pin::Mutable(s) => s,
        }
    }

    pub fn is_immutable(&self) -> bool {
        matches!(self, Pin::Sha(_))
    }
}

/// What kind of `uses:` an edge is. Only a reusable workflow carries the
/// upstream's *own* `uses:` edges into the consumer's CI run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EdgeKind {
    /// `uses: owner/repo/.github/workflows/x.yml@ref` at job level.
    ReusableWorkflow,
    /// `uses: owner/repo[/subpath]@ref` as a step.
    Action,
}

/// One `uses:` reference from a consumer's workflow file to an upstream repo.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Edge {
    /// The consumer repo.
    pub from: RepoId,
    /// The consumer's workflow file, relative to `.github/workflows/`.
    pub file: String,
    /// The upstream repo.
    pub to: RepoId,
    pub kind: EdgeKind,
    /// Path inside the upstream: the reusable workflow's path, or an action
    /// subpath. `None` for a root action.
    #[serde(default)]
    pub path: Option<String>,
    pub pin: Pin,
    /// A trailing `# vX.Y.Z` comment on a SHA pin, kept as a human hint only.
    /// Never used for identity: the SHA is the truth.
    #[serde(default)]
    pub version_hint: Option<String>,
}

impl Edge {
    fn is_self(&self) -> bool {
        self.from == self.to
    }

    /// For a reusable-workflow edge, the upstream workflow's file name (what
    /// the upstream's own edges record as `file`).
    fn reusable_file(&self) -> Option<&str> {
        match self.kind {
            EdgeKind::ReusableWorkflow => {
                let p = self.path.as_deref()?;
                Some(p.rsplit('/').next().unwrap_or(p))
            }
            EdgeKind::Action => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Scans
// ---------------------------------------------------------------------------

/// What happened when a host tried to read one repo's workflows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum ScanStatus {
    /// Read successfully. `revision` is the commit the files came from, when
    /// the source knows it (GraphQL always does; a local tree may not).
    Scanned {
        #[serde(default)]
        revision: Option<String>,
        workflow_files: usize,
    },
    /// Probed, and the repo does not exist or is not visible to the token.
    /// The two cannot be told apart through the API, and both mean a `uses:`
    /// pointing there cannot resolve for this reader.
    NotFound,
    /// The read failed for another reason (rate limit, network, parse).
    /// Nothing is concluded about the repo.
    Unavailable { reason: String },
}

/// One workflow file's text, as fetched. Hosts extract [`Edge`]s from these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowFile {
    /// Path relative to `.github/workflows/`.
    pub path: String,
    pub text: String,
}

/// Everything a source returned for one repo: its status and raw files.
/// This is the seam between sources (local tree, GraphQL) and extraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoSnapshot {
    pub repo: RepoId,
    pub status: ScanStatus,
    #[serde(default)]
    pub files: Vec<WorkflowFile>,
    /// Caveats the source hit while reading (truncated or binary blobs, …).
    #[serde(default)]
    pub notes: Vec<String>,
}

// ---------------------------------------------------------------------------
// The graph
// ---------------------------------------------------------------------------

/// The extracted graph: every probed repo with its status, and every edge.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainGraph {
    pub scans: BTreeMap<RepoId, ScanStatus>,
    pub edges: Vec<Edge>,
    /// Source caveats carried through to the report verbatim.
    #[serde(default)]
    pub notes: Vec<String>,
}

impl ChainGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a probe result. A later result for the same repo replaces the
    /// earlier one only if the earlier one was not a successful scan.
    pub fn record_scan(&mut self, repo: RepoId, status: ScanStatus) {
        match self.scans.get(&repo) {
            Some(ScanStatus::Scanned { .. }) => {}
            _ => {
                self.scans.insert(repo, status);
            }
        }
    }

    pub fn add_edges(&mut self, edges: impl IntoIterator<Item = Edge>) {
        self.edges.extend(edges);
    }

    fn is_scanned(&self, repo: &RepoId) -> bool {
        matches!(self.scans.get(repo), Some(ScanStatus::Scanned { .. }))
    }

    fn scanned_revision(&self, repo: &RepoId) -> Option<&str> {
        match self.scans.get(repo) {
            Some(ScanStatus::Scanned { revision, .. }) => revision.as_deref(),
            _ => None,
        }
    }

    /// Distinct repo-level dependency pairs `(consumer, upstream)`, self
    /// references excluded (a repo calling its own reusable workflow is a
    /// two-step landing, not a cycle).
    fn repo_edges(&self) -> BTreeSet<(RepoId, RepoId)> {
        self.edges
            .iter()
            .filter(|e| !e.is_self())
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect()
    }

    fn nodes(&self) -> BTreeSet<RepoId> {
        let mut n: BTreeSet<RepoId> = self.scans.keys().cloned().collect();
        for e in &self.edges {
            n.insert(e.from.clone());
            n.insert(e.to.clone());
        }
        n
    }

    /// Strongly connected components with more than one repo (Tarjan).
    /// Deterministic: components and their members are sorted.
    pub fn cycles(&self) -> Vec<Vec<RepoId>> {
        let nodes: Vec<RepoId> = self.nodes().into_iter().collect();
        let index_of: HashMap<&RepoId, usize> =
            nodes.iter().enumerate().map(|(i, r)| (r, i)).collect();
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
        for (a, b) in self.repo_edges() {
            adj[index_of[&a]].push(index_of[&b]);
        }

        struct Tarjan<'a> {
            adj: &'a [Vec<usize>],
            index: Vec<Option<usize>>,
            low: Vec<usize>,
            on_stack: Vec<bool>,
            stack: Vec<usize>,
            next: usize,
            out: Vec<Vec<usize>>,
        }
        impl Tarjan<'_> {
            fn visit(&mut self, v: usize) {
                self.index[v] = Some(self.next);
                self.low[v] = self.next;
                self.next += 1;
                self.stack.push(v);
                self.on_stack[v] = true;
                for i in 0..self.adj[v].len() {
                    let w = self.adj[v][i];
                    match self.index[w] {
                        None => {
                            self.visit(w);
                            self.low[v] = self.low[v].min(self.low[w]);
                        }
                        Some(iw) if self.on_stack[w] => {
                            self.low[v] = self.low[v].min(iw);
                        }
                        _ => {}
                    }
                }
                if Some(self.low[v]) == self.index[v] {
                    let mut comp = Vec::new();
                    while let Some(w) = self.stack.pop() {
                        self.on_stack[w] = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    if comp.len() > 1 {
                        self.out.push(comp);
                    }
                }
            }
        }

        let n = nodes.len();
        let mut t = Tarjan {
            adj: &adj,
            index: vec![None; n],
            low: vec![0; n],
            on_stack: vec![false; n],
            stack: Vec::new(),
            next: 0,
            out: Vec::new(),
        };
        for v in 0..n {
            if t.index[v].is_none() {
                t.visit(v);
            }
        }
        let mut comps: Vec<Vec<RepoId>> = t
            .out
            .into_iter()
            .map(|c| {
                let mut c: Vec<RepoId> = c.into_iter().map(|i| nodes[i].clone()).collect();
                c.sort();
                c
            })
            .collect();
        comps.sort();
        comps
    }

    /// A landing order over every repo in the graph: each upstream before any
    /// of its consumers. Ties are broken by name, so the order is
    /// reproducible. `None` when a cycle makes an order impossible.
    pub fn landing_order(&self) -> Option<Vec<RepoId>> {
        let nodes = self.nodes();
        // consumer -> upstreams it waits on
        let mut waits: BTreeMap<RepoId, BTreeSet<RepoId>> =
            nodes.iter().map(|n| (n.clone(), BTreeSet::new())).collect();
        for (from, to) in self.repo_edges() {
            waits.entry(from).or_default().insert(to);
        }
        let mut order = Vec::with_capacity(nodes.len());
        let mut done: BTreeSet<RepoId> = BTreeSet::new();
        while done.len() < nodes.len() {
            let ready: Vec<RepoId> = waits
                .iter()
                .filter(|(n, ups)| !done.contains(*n) && ups.iter().all(|u| done.contains(u)))
                .map(|(n, _)| n.clone())
                .collect();
            if ready.is_empty() {
                return None;
            }
            for r in ready {
                done.insert(r.clone());
                order.push(r);
            }
        }
        Some(order)
    }

    /// The heaviest upstream→consumer chain, where a chain's weight is the sum
    /// of `weight` over its repos: a longest path, i.e. max-plus evaluation.
    /// Returned upstream-first. `None` when a cycle makes it unbounded, or the
    /// graph is empty. Ties are broken by name.
    pub fn critical_path(&self, weight: impl Fn(&RepoId) -> u64) -> Option<(u64, Vec<RepoId>)> {
        let order = self.landing_order()?;
        let edges = self.repo_edges();
        // best[n] = (weight of heaviest chain ending at n, predecessor)
        let mut best: BTreeMap<RepoId, (u64, Option<RepoId>)> = BTreeMap::new();
        for n in &order {
            let own = weight(n);
            let mut cand: (u64, Option<RepoId>) = (own, None);
            for (from, to) in &edges {
                if from == n {
                    if let Some((w, _)) = best.get(to) {
                        let total = w.saturating_add(own);
                        if total > cand.0 {
                            cand = (total, Some(to.clone()));
                        }
                    }
                }
            }
            best.insert(n.clone(), cand);
        }
        let (end, (total, _)) = best
            .iter()
            .max_by(|a, b| a.1 .0.cmp(&b.1 .0).then_with(|| b.0.cmp(a.0)))?;
        let total = *total;
        let mut path = vec![end.clone()];
        let mut cur = end.clone();
        while let Some((_, Some(prev))) = best.get(&cur) {
            path.push(prev.clone());
            cur = prev.clone();
        }
        path.reverse();
        Some((total, path))
    }

    /// Every pin through which `target` enters `consumer`'s CI: the consumer's
    /// direct edges, plus edges carried in through reusable workflows it calls
    /// (followed file-precisely, depth-first, with a visited guard).
    fn routes_into(&self, consumer: &RepoId) -> Vec<Route> {
        let mut out = Vec::new();
        for e in self.edges.iter().filter(|e| &e.from == consumer) {
            out.push(Route {
                target: e.to.clone(),
                pin: e.pin.clone(),
                via: Vec::new(),
                basis: RevisionBasis::Direct,
                file: e.file.clone(),
            });
        }
        let mut visited: BTreeSet<(RepoId, String)> = BTreeSet::new();
        for e in self.edges.iter().filter(|e| &e.from == consumer) {
            self.follow(e, Vec::new(), &mut visited, &mut out);
        }
        out
    }

    fn follow(
        &self,
        call: &Edge,
        mut via: Vec<RepoId>,
        visited: &mut BTreeSet<(RepoId, String)>,
        out: &mut Vec<Route>,
    ) {
        let Some(file) = call.reusable_file() else {
            return;
        };
        if call.is_self() || !visited.insert((call.to.clone(), file.to_string())) {
            return;
        }
        let basis = match (&call.pin, self.scanned_revision(&call.to)) {
            (Pin::Sha(p), Some(rev)) if p.eq_ignore_ascii_case(rev) => {
                RevisionBasis::PinnedRevision
            }
            _ => RevisionBasis::ScannedRevision,
        };
        via.push(call.to.clone());
        for inner in self
            .edges
            .iter()
            .filter(|e| e.from == call.to && e.file == file)
        {
            out.push(Route {
                target: inner.to.clone(),
                pin: inner.pin.clone(),
                via: via.clone(),
                basis,
                file: call.file.clone(),
            });
            self.follow(inner, via.clone(), visited, out);
        }
    }
}

// ---------------------------------------------------------------------------
// Findings
// ---------------------------------------------------------------------------

/// How sure a transitive route is about the upstream revision it read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RevisionBasis {
    /// The consumer's own edge; no upstream revision involved.
    Direct,
    /// The upstream was read at exactly the SHA the consumer pins.
    PinnedRevision,
    /// The upstream was read at some other revision (e.g. its default branch).
    /// The finding may not match what the pinned revision actually does.
    ScannedRevision,
}

/// One way an upstream reaches a consumer's CI.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Route {
    pub target: RepoId,
    pub pin: Pin,
    /// Reusable-workflow repos passed through, outermost first. Empty = direct.
    pub via: Vec<RepoId>,
    pub basis: RevisionBasis,
    /// The consumer's own workflow file where the route starts.
    pub file: String,
}

/// One pin of a skewed upstream and who uses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinUse {
    pub pin: Pin,
    #[serde(default)]
    pub version_hints: Vec<String>,
    pub consumers: Vec<Edge>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    Warning,
    Blocking,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "finding", rename_all = "kebab-case")]
pub enum Finding {
    Cycle {
        repos: Vec<RepoId>,
    },
    DeadUpstream {
        target: RepoId,
        consumers: Vec<Edge>,
    },
    Diamond {
        consumer: RepoId,
        target: RepoId,
        routes: Vec<Route>,
    },
    PinSkew {
        target: RepoId,
        pins: Vec<PinUse>,
    },
    MutablePins {
        consumer: RepoId,
        uses: Vec<Edge>,
    },
}

impl Finding {
    pub fn severity(&self) -> Severity {
        match self {
            Finding::Cycle { .. } | Finding::DeadUpstream { .. } => Severity::Blocking,
            Finding::Diamond { .. } | Finding::PinSkew { .. } | Finding::MutablePins { .. } => {
                Severity::Warning
            }
        }
    }

    /// One-line human summary.
    pub fn describe(&self) -> String {
        match self {
            Finding::Cycle { repos } => format!(
                "cycle: {} depend on each other; no landing order exists",
                join(repos)
            ),
            Finding::DeadUpstream { target, consumers } => format!(
                "dead upstream `{target}`: {} reference(s) cannot resolve, so their jobs never run",
                consumers.len()
            ),
            Finding::Diamond {
                consumer,
                target,
                routes,
            } => {
                let pins: BTreeSet<&str> = routes.iter().map(|r| r.pin.as_str()).collect();
                let hedged = routes
                    .iter()
                    .any(|r| r.basis == RevisionBasis::ScannedRevision);
                format!(
                    "diamond: `{consumer}` receives `{target}` at {} pins ({}){}",
                    pins.len(),
                    pins.into_iter().collect::<Vec<_>>().join(", "),
                    if hedged {
                        " — partly read at a revision other than the one pinned"
                    } else {
                        ""
                    }
                )
            }
            Finding::PinSkew { target, pins } => format!(
                "pin skew: `{target}` pinned at {} different refs across consumers",
                pins.len()
            ),
            Finding::MutablePins { consumer, uses } => format!(
                "`{consumer}` pins {} upstream ref(s) by tag/branch instead of SHA",
                uses.len()
            ),
        }
    }
}

fn join(repos: &[RepoId]) -> String {
    repos
        .iter()
        .map(|r| r.as_str())
        .collect::<Vec<_>>()
        .join(" ↔ ")
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// Query-cost evidence from the source, when it has any (GraphQL does).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCost {
    pub queries: u32,
    pub points_used: u64,
    #[serde(default)]
    pub points_remaining: Option<u64>,
    #[serde(default)]
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainsReport {
    pub summary: String,
    pub scans: BTreeMap<RepoId, ScanStatus>,
    pub edge_count: usize,
    /// Blocking findings first, then warnings; stable within each.
    pub findings: Vec<Finding>,
    /// Upstreams referenced but never probed. Nothing is concluded about them.
    pub frontier: Vec<RepoId>,
    /// Upstreams-first order over every repo seen; `None` if cyclic.
    pub landing_order: Option<Vec<RepoId>>,
    /// The longest upstream→consumer chain (hop-weighted for now).
    #[serde(default)]
    pub critical_chain: Vec<RepoId>,
    /// Source caveats, verbatim (no-overclaim: partial reads are said aloud).
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub cost: Option<SourceCost>,
}

impl ChainsReport {
    pub fn has_blocking(&self) -> bool {
        self.findings
            .iter()
            .any(|f| f.severity() == Severity::Blocking)
    }
}

/// Analyse a graph. Pure and deterministic.
///
/// Mutable-pin and skew findings are reported only for edges whose consumer
/// was actually scanned, so a frontier repo is never blamed on hearsay.
pub fn analyse(graph: &ChainGraph, cost: Option<SourceCost>) -> ChainsReport {
    let mut findings = Vec::new();

    for repos in graph.cycles() {
        findings.push(Finding::Cycle { repos });
    }

    // Dead upstreams: probed and not found.
    let mut dead: BTreeMap<RepoId, Vec<Edge>> = BTreeMap::new();
    for e in &graph.edges {
        if matches!(graph.scans.get(&e.to), Some(ScanStatus::NotFound)) {
            dead.entry(e.to.clone()).or_default().push(e.clone());
        }
    }
    for (target, mut consumers) in dead {
        consumers.sort();
        findings.push(Finding::DeadUpstream { target, consumers });
    }

    // Diamonds: >1 distinct pin into one consumer, at least one transitive.
    let consumers: BTreeSet<RepoId> = graph
        .edges
        .iter()
        .map(|e| e.from.clone())
        .filter(|r| graph.is_scanned(r))
        .collect();
    for c in &consumers {
        let mut by_target: BTreeMap<RepoId, Vec<Route>> = BTreeMap::new();
        for r in graph.routes_into(c) {
            by_target.entry(r.target.clone()).or_default().push(r);
        }
        for (target, mut routes) in by_target {
            if &target == c {
                continue;
            }
            let pins: BTreeSet<&Pin> = routes.iter().map(|r| &r.pin).collect();
            let transitive = routes.iter().any(|r| !r.via.is_empty());
            if pins.len() > 1 && transitive {
                routes.sort();
                routes.dedup();
                findings.push(Finding::Diamond {
                    consumer: c.clone(),
                    target,
                    routes,
                });
            }
        }
    }

    // Pin skew across scanned consumers.
    let mut by_target: BTreeMap<RepoId, BTreeMap<Pin, Vec<Edge>>> = BTreeMap::new();
    for e in graph.edges.iter().filter(|e| graph.is_scanned(&e.from)) {
        by_target
            .entry(e.to.clone())
            .or_default()
            .entry(e.pin.clone())
            .or_default()
            .push(e.clone());
    }
    for (target, pins) in by_target {
        if pins.len() > 1 {
            let pins = pins
                .into_iter()
                .map(|(pin, mut consumers)| {
                    consumers.sort();
                    let version_hints: BTreeSet<String> = consumers
                        .iter()
                        .filter_map(|e| e.version_hint.clone())
                        .collect();
                    PinUse {
                        pin,
                        version_hints: version_hints.into_iter().collect(),
                        consumers,
                    }
                })
                .collect();
            findings.push(Finding::PinSkew { target, pins });
        }
    }

    // Mutable pins per scanned consumer.
    let mut mutable: BTreeMap<RepoId, Vec<Edge>> = BTreeMap::new();
    for e in graph
        .edges
        .iter()
        .filter(|e| !e.pin.is_immutable() && !e.is_self() && graph.is_scanned(&e.from))
    {
        mutable.entry(e.from.clone()).or_default().push(e.clone());
    }
    for (consumer, mut uses) in mutable {
        uses.sort();
        findings.push(Finding::MutablePins { consumer, uses });
    }

    findings.sort_by_key(|f| std::cmp::Reverse(f.severity()));

    let frontier: Vec<RepoId> = graph
        .edges
        .iter()
        .map(|e| e.to.clone())
        .filter(|r| !graph.scans.contains_key(r))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let landing_order = graph.landing_order();
    let critical_chain = graph
        .critical_path(|_| 1)
        .map(|(_, p)| p)
        .unwrap_or_default();

    let scanned = graph
        .scans
        .values()
        .filter(|s| matches!(s, ScanStatus::Scanned { .. }))
        .count();
    let blocking = findings
        .iter()
        .filter(|f| f.severity() == Severity::Blocking)
        .count();
    let summary = format!(
        "{scanned}/{} repos scanned, {} edges, {} findings ({blocking} blocking), {} on the frontier",
        graph.scans.len(),
        graph.edges.len(),
        findings.len(),
        frontier.len()
    );

    ChainsReport {
        summary,
        scans: graph.scans.clone(),
        edge_count: graph.edges.len(),
        findings,
        frontier,
        landing_order,
        critical_chain,
        notes: graph.notes.clone(),
        cost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn r(s: &str) -> RepoId {
        RepoId::new(s)
    }

    fn scanned(rev: Option<&str>) -> ScanStatus {
        ScanStatus::Scanned {
            revision: rev.map(str::to_string),
            workflow_files: 1,
        }
    }

    fn edge(
        from: &str,
        file: &str,
        to: &str,
        kind: EdgeKind,
        path: Option<&str>,
        pin: &str,
    ) -> Edge {
        Edge {
            from: r(from),
            file: file.into(),
            to: r(to),
            kind,
            path: path.map(str::to_string),
            pin: Pin::parse(pin),
            version_hint: None,
        }
    }

    fn action(from: &str, to: &str, pin: &str) -> Edge {
        edge(from, "ci.yml", to, EdgeKind::Action, None, pin)
    }

    #[test]
    fn repo_identity_is_case_insensitive_but_keeps_spelling() {
        let a = r("Hyperpolymath/MetaManifold-WebUI");
        let b = r("hyperpolymath/metamanifold-webui");
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "Hyperpolymath/MetaManifold-WebUI");
        let set: BTreeSet<RepoId> = [a, b].into_iter().collect();
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn only_a_full_sha_is_immutable() {
        assert!(Pin::parse(SHA_A).is_immutable());
        assert!(Pin::parse(&SHA_A.to_uppercase()).is_immutable());
        assert!(!Pin::parse("v4").is_immutable());
        assert!(!Pin::parse("aaaaaaa").is_immutable());
        assert!(!Pin::parse("main").is_immutable());
    }

    #[test]
    fn empty_graph_reports_nothing_and_orders_nothing() {
        let rep = analyse(&ChainGraph::new(), None);
        assert!(rep.findings.is_empty());
        assert!(!rep.has_blocking());
        assert_eq!(rep.landing_order, Some(vec![]));
        assert!(rep.critical_chain.is_empty());
    }

    #[test]
    fn landing_order_puts_upstreams_first() {
        let mut g = ChainGraph::new();
        for s in ["me/app", "me/standards", "me/lib"] {
            g.record_scan(r(s), scanned(None));
        }
        g.add_edges([
            action("me/app", "me/lib", SHA_A),
            action("me/lib", "me/standards", SHA_A),
        ]);
        assert_eq!(
            g.landing_order().unwrap(),
            vec![r("me/standards"), r("me/lib"), r("me/app")]
        );
        let (w, path) = g.critical_path(|_| 1).unwrap();
        assert_eq!(w, 3);
        assert_eq!(path, vec![r("me/standards"), r("me/lib"), r("me/app")]);
    }

    #[test]
    fn a_cycle_is_blocking_and_has_no_landing_order() {
        let mut g = ChainGraph::new();
        g.record_scan(r("me/a"), scanned(None));
        g.record_scan(r("me/b"), scanned(None));
        g.add_edges([action("me/a", "me/b", SHA_A), action("me/b", "me/a", SHA_A)]);
        let rep = analyse(&g, None);
        assert!(rep.has_blocking());
        assert_eq!(rep.landing_order, None);
        assert!(rep.critical_chain.is_empty());
        assert!(matches!(
            &rep.findings[0],
            Finding::Cycle { repos } if repos == &vec![r("me/a"), r("me/b")]
        ));
    }

    #[test]
    fn self_reference_is_not_a_cycle_nor_a_mutable_pin() {
        let mut g = ChainGraph::new();
        g.record_scan(r("me/standards"), scanned(None));
        g.add_edges([edge(
            "me/standards",
            "gov.yml",
            "me/standards",
            EdgeKind::ReusableWorkflow,
            Some(".github/workflows/gov-reusable.yml"),
            "main",
        )]);
        let rep = analyse(&g, None);
        assert!(rep.findings.is_empty(), "{:?}", rep.findings);
        assert!(rep.landing_order.is_some());
    }

    #[test]
    fn dead_upstream_is_blocking_and_names_every_consumer() {
        let mut g = ChainGraph::new();
        g.record_scan(r("me/app"), scanned(None));
        g.record_scan(r("me/gone-action"), ScanStatus::NotFound);
        g.add_edges([action("me/app", "me/gone-action", SHA_A)]);
        let rep = analyse(&g, None);
        assert!(rep.has_blocking());
        assert!(matches!(
            &rep.findings[0],
            Finding::DeadUpstream { target, consumers }
                if target == &r("me/gone-action") && consumers.len() == 1
        ));
    }

    #[test]
    fn unavailable_is_not_dead() {
        // A rate-limited probe concludes nothing — fail-closed, no invention.
        let mut g = ChainGraph::new();
        g.record_scan(r("me/app"), scanned(None));
        g.record_scan(
            r("me/lib"),
            ScanStatus::Unavailable {
                reason: "rate limited".into(),
            },
        );
        g.add_edges([action("me/app", "me/lib", SHA_A)]);
        let rep = analyse(&g, None);
        assert!(!rep.has_blocking());
    }

    #[test]
    fn unprobed_upstreams_go_on_the_frontier() {
        let mut g = ChainGraph::new();
        g.record_scan(r("me/app"), scanned(None));
        g.add_edges([action("me/app", "actions/checkout", SHA_A)]);
        let rep = analyse(&g, None);
        assert_eq!(rep.frontier, vec![r("actions/checkout")]);
    }

    #[test]
    fn pin_skew_groups_consumers_by_pin() {
        let mut g = ChainGraph::new();
        g.record_scan(r("me/a"), scanned(None));
        g.record_scan(r("me/b"), scanned(None));
        g.add_edges([
            action("me/a", "actions/checkout", SHA_A),
            action("me/b", "actions/checkout", SHA_B),
        ]);
        let rep = analyse(&g, None);
        let skew = rep
            .findings
            .iter()
            .find_map(|f| match f {
                Finding::PinSkew { target, pins } => Some((target, pins)),
                _ => None,
            })
            .expect("skew");
        assert_eq!(skew.0, &r("actions/checkout"));
        assert_eq!(skew.1.len(), 2);
    }

    #[test]
    fn a_frontier_consumer_is_never_blamed() {
        // Edges from a repo that was not scanned (hearsay) raise no warnings.
        let mut g = ChainGraph::new();
        g.add_edges([action("them/x", "actions/checkout", "v4")]);
        let rep = analyse(&g, None);
        assert!(rep.findings.is_empty());
    }

    #[test]
    fn mutable_pins_are_grouped_per_consumer() {
        let mut g = ChainGraph::new();
        g.record_scan(r("me/a"), scanned(None));
        g.add_edges([
            action("me/a", "actions/checkout", "v4"),
            action("me/a", "github/codeql-action", "v3"),
            action("me/a", "ossf/scorecard-action", SHA_A),
        ]);
        let rep = analyse(&g, None);
        let m = rep
            .findings
            .iter()
            .find_map(|f| match f {
                Finding::MutablePins { consumer, uses } => Some((consumer, uses.len())),
                _ => None,
            })
            .expect("mutable pins");
        assert_eq!(m, (&r("me/a"), 2));
    }

    fn diamond_graph(standards_rev: Option<&str>) -> ChainGraph {
        // me/app pins checkout at A directly, and calls standards' reusable,
        // whose gov-reusable.yml pins checkout at B.
        let mut g = ChainGraph::new();
        g.record_scan(r("me/app"), scanned(None));
        g.record_scan(r("me/standards"), scanned(standards_rev));
        g.add_edges([
            action("me/app", "actions/checkout", SHA_A),
            edge(
                "me/app",
                "gov.yml",
                "me/standards",
                EdgeKind::ReusableWorkflow,
                Some(".github/workflows/gov-reusable.yml"),
                SHA_A,
            ),
            edge(
                "me/standards",
                "gov-reusable.yml",
                "actions/checkout",
                EdgeKind::Action,
                None,
                SHA_B,
            ),
            // A different standards file pinning something else must NOT leak
            // into me/app: only the called reusable is carried in.
            edge(
                "me/standards",
                "unrelated.yml",
                "actions/setup-node",
                EdgeKind::Action,
                None,
                SHA_B,
            ),
        ]);
        g
    }

    #[test]
    fn diamond_through_a_reusable_is_found_file_precisely() {
        let rep = analyse(&diamond_graph(None), None);
        let d = rep
            .findings
            .iter()
            .find_map(|f| match f {
                Finding::Diamond {
                    consumer,
                    target,
                    routes,
                } => Some((consumer, target, routes)),
                _ => None,
            })
            .expect("diamond");
        assert_eq!(d.0, &r("me/app"));
        assert_eq!(d.1, &r("actions/checkout"));
        assert_eq!(d.2.len(), 2);
        assert!(d
            .2
            .iter()
            .any(|r| r.via == vec![RepoId::new("me/standards")]));
        // setup-node lives in an uncalled file: no diamond, no route.
        assert!(!rep.findings.iter().any(|f| matches!(
            f,
            Finding::Diamond { target, .. } if target == &r("actions/setup-node")
        )));
    }

    #[test]
    fn diamond_basis_is_honest_about_the_revision_read() {
        let hedged = analyse(&diamond_graph(Some(SHA_B)), None);
        let exact = analyse(&diamond_graph(Some(SHA_A)), None);
        let basis = |rep: &ChainsReport| {
            rep.findings.iter().find_map(|f| match f {
                Finding::Diamond { routes, .. } => {
                    routes.iter().find(|r| !r.via.is_empty()).map(|r| r.basis)
                }
                _ => None,
            })
        };
        assert_eq!(basis(&hedged), Some(RevisionBasis::ScannedRevision));
        assert_eq!(basis(&exact), Some(RevisionBasis::PinnedRevision));
        assert!(hedged
            .findings
            .iter()
            .any(|f| f.describe().contains("other than the one pinned")));
    }

    #[test]
    fn reusable_recursion_terminates_on_mutual_reusables() {
        let mut g = ChainGraph::new();
        g.record_scan(r("me/a"), scanned(None));
        g.record_scan(r("me/b"), scanned(None));
        g.add_edges([
            edge(
                "me/a",
                "x.yml",
                "me/b",
                EdgeKind::ReusableWorkflow,
                Some(".github/workflows/y.yml"),
                SHA_A,
            ),
            edge(
                "me/b",
                "y.yml",
                "me/a",
                EdgeKind::ReusableWorkflow,
                Some(".github/workflows/x.yml"),
                SHA_A,
            ),
        ]);
        let rep = analyse(&g, None); // must not overflow the stack
        assert!(rep.has_blocking());
    }

    #[test]
    fn blocking_findings_sort_first() {
        let mut g = ChainGraph::new();
        g.record_scan(r("me/a"), scanned(None));
        g.record_scan(r("me/dead"), ScanStatus::NotFound);
        g.add_edges([
            action("me/a", "actions/checkout", "v4"),
            action("me/a", "me/dead", SHA_A),
        ]);
        let rep = analyse(&g, None);
        assert_eq!(rep.findings[0].severity(), Severity::Blocking);
        assert_eq!(rep.findings.last().unwrap().severity(), Severity::Warning);
    }

    #[test]
    fn a_successful_scan_is_not_overwritten_by_a_later_failure() {
        let mut g = ChainGraph::new();
        g.record_scan(r("me/a"), scanned(Some(SHA_A)));
        g.record_scan(r("me/a"), ScanStatus::Unavailable { reason: "x".into() });
        assert!(g.is_scanned(&r("me/a")));
    }

    #[test]
    fn report_round_trips_through_json() {
        let rep = analyse(
            &diamond_graph(Some(SHA_A)),
            Some(SourceCost {
                queries: 1,
                points_used: 1,
                points_remaining: Some(4999),
                resets_at: None,
            }),
        );
        let json = serde_json::to_string(&rep).unwrap();
        let back: ChainsReport = serde_json::from_str(&json).unwrap();
        assert_eq!(rep, back);
    }
}
