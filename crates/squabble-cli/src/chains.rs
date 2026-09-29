// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble chains` — cross-repo CI dependency chains, read-only.
//!
//! Sources, freely mixed:
//!
//! * a local checkout path — identity from `path=owner/repo`, else its
//!   `origin` remote;
//! * `gh:owner/repo` — read through GitHub GraphQL (`squabble-forge`), batched.
//!
//! `--follow N` then probes upstreams referenced by what was read, N levels
//! deep, through GraphQL. Action upstreams are probed for *existence only*
//! (their own CI is not their consumers' CI); reusable-workflow upstreams
//! contribute the edges of exactly the files that are called.
//!
//! Nothing is written anywhere. Exit `0` = no blocking finding, `4` = at
//! least one blocking finding (a cycle or a dead upstream), `2` = usage or
//! tool failure, including a run where no repository could be read at all.

use squabble_core::chains::{
    analyse, ChainsReport, Finding, RepoId, RepoSnapshot, ScanStatus, Severity, SourceCost,
};
use squabble_fight::chains::{graph_from_snapshots, snapshot_local, upstream_roles, Role};
use squabble_forge::{fetch_workflows, GhTransport, GraphQlTransport, DEFAULT_BATCH};
use std::path::Path;
use std::process::ExitCode;

pub(crate) const USAGE: &str =
    "usage: squabble chains <path[=owner/repo] | gh:owner/repo>... [--follow N] [--batch N] [--json]";

/// Exit code when the report carries at least one blocking finding.
pub(crate) const BLOCKING_EXIT: u8 = 4;

enum Source {
    Local { path: String, slug: Option<String> },
    GraphQl(RepoId),
}

struct Args {
    sources: Vec<Source>,
    follow: u32,
    batch: usize,
    json: bool,
}

fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut a = Args {
        sources: Vec::new(),
        follow: 0,
        batch: DEFAULT_BATCH,
        json: false,
    };
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => a.json = true,
            "--follow" => {
                a.follow = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--follow needs a non-negative integer")?
            }
            "--batch" => {
                a.batch = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|&n: &usize| n > 0)
                    .ok_or("--batch needs a positive integer")?
            }
            s if s.starts_with("--") => return Err(format!("unknown flag `{s}`\n{USAGE}")),
            s => {
                if let Some(slug) = s.strip_prefix("gh:") {
                    a.sources.push(Source::GraphQl(RepoId::new(slug)));
                } else {
                    let (path, slug) = match s.split_once('=') {
                        Some((p, sl)) => (p.to_string(), Some(sl.to_string())),
                        None => (s.to_string(), None),
                    };
                    a.sources.push(Source::Local { path, slug });
                }
            }
        }
    }
    if a.sources.is_empty() {
        return Err(USAGE.to_string());
    }
    Ok(a)
}

pub fn run(rest: &[String]) -> ExitCode {
    let args = match parse_args(rest) {
        Ok(a) => a,
        Err(m) => {
            eprintln!("{m}");
            return ExitCode::from(2);
        }
    };
    let report = collect(&args, &GhTransport);
    if args.json {
        match serde_json::to_string_pretty(&report) {
            Ok(j) => println!("{j}"),
            Err(e) => {
                eprintln!("squabble chains: could not serialise report: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        print!("{}", narrate(&report));
    }
    exit_for(&report)
}

/// Fail-closed exit: a run that read nothing is a tool failure (`2`), never a
/// clean `0`; otherwise `4` on any blocking finding.
fn exit_for(report: &ChainsReport) -> ExitCode {
    let scanned = report
        .scans
        .values()
        .any(|s| matches!(s, ScanStatus::Scanned { .. }));
    if !scanned {
        eprintln!("squabble chains: no repository could be read — nothing was analysed");
        return ExitCode::from(2);
    }
    if report.has_blocking() {
        ExitCode::from(BLOCKING_EXIT)
    } else {
        ExitCode::SUCCESS
    }
}

/// Gather snapshots from every source, follow upstreams, and analyse.
fn collect(args: &Args, transport: &dyn GraphQlTransport) -> ChainsReport {
    let mut subjects: Vec<RepoSnapshot> = Vec::new();
    let mut remote: Vec<RepoId> = Vec::new();
    for s in &args.sources {
        match s {
            Source::Local { path, slug } => {
                subjects.push(snapshot_local(Path::new(path), slug.as_deref()))
            }
            Source::GraphQl(r) => remote.push(r.clone()),
        }
    }
    let mut cost: Option<SourceCost> = None;
    if !remote.is_empty() {
        let (snaps, c) = fetch_workflows(transport, &remote, args.batch);
        subjects.extend(snaps);
        cost = Some(c);
    }

    let mut upstreams: Vec<(RepoSnapshot, Role)> = Vec::new();
    for _ in 0..args.follow {
        let pairs: Vec<(&RepoSnapshot, Role)> = subjects
            .iter()
            .map(|s| (s, Role::Subject))
            .chain(upstreams.iter().map(|(s, r)| (s, r.clone())))
            .collect();
        let g = graph_from_snapshots(pairs);
        let next = upstream_roles(&g);
        if next.is_empty() {
            break;
        }
        let ids: Vec<RepoId> = next.iter().map(|(r, _)| r.clone()).collect();
        let (snaps, c) = fetch_workflows(transport, &ids, args.batch);
        let total = cost.get_or_insert_with(SourceCost::default);
        total.queries += c.queries;
        total.points_used += c.points_used;
        if c.points_remaining.is_some() {
            total.points_remaining = c.points_remaining;
            total.resets_at = c.resets_at;
        }
        upstreams.extend(
            snaps
                .into_iter()
                .zip(next.into_iter().map(|(_, role)| role)),
        );
    }

    let pairs = subjects
        .iter()
        .map(|s| (s, Role::Subject))
        .chain(upstreams.iter().map(|(s, r)| (s, r.clone())));
    analyse(&graph_from_snapshots(pairs), cost)
}

/// Human-readable report. Blocking first; every repo's scan status is shown
/// so an unavailable read is never mistaken for a clean one.
fn narrate(r: &ChainsReport) -> String {
    let mut o = String::new();
    o.push_str(&format!("chains: {}\n", r.summary));
    o.push_str("\nscans:\n");
    for (repo, s) in &r.scans {
        let line = match s {
            ScanStatus::Scanned {
                revision,
                workflow_files,
            } => format!(
                "scanned @{} ({workflow_files} workflow files)",
                revision
                    .as_deref()
                    .map(|x| &x[..x.len().min(12)])
                    .unwrap_or("?")
            ),
            ScanStatus::NotFound => "NOT FOUND".to_string(),
            ScanStatus::Unavailable { reason } => format!("unavailable — {reason}"),
        };
        o.push_str(&format!("  {repo}: {line}\n"));
    }
    if !r.findings.is_empty() {
        o.push_str("\nfindings:\n");
        for f in &r.findings {
            let tag = match f.severity() {
                Severity::Blocking => "BLOCKING",
                Severity::Warning => "warning ",
            };
            o.push_str(&format!("  [{tag}] {}\n", f.describe()));
            for detail in details(f) {
                o.push_str(&format!("             {detail}\n"));
            }
        }
    }
    match &r.landing_order {
        Some(order) if !order.is_empty() => {
            // Narrate only probed repos; frontier leaves are in --json.
            let names: Vec<&str> = order
                .iter()
                .filter(|x| r.scans.contains_key(*x))
                .map(|x| x.as_str())
                .collect();
            if !names.is_empty() {
                o.push_str(&format!(
                    "\nlanding order of probed repos (upstreams first): {}\n",
                    names.join(" → ")
                ));
            }
        }
        Some(_) => {}
        None => o.push_str("\nlanding order: none — a cycle makes one impossible\n"),
    }
    if r.critical_chain.len() > 1 {
        let names: Vec<&str> = r.critical_chain.iter().map(|x| x.as_str()).collect();
        o.push_str(&format!("longest chain: {}\n", names.join(" → ")));
    }
    if !r.frontier.is_empty() {
        o.push_str(&format!(
            "\nfrontier ({} referenced, not probed — use --follow to probe): {}\n",
            r.frontier.len(),
            r.frontier
                .iter()
                .map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !r.notes.is_empty() {
        o.push_str("\nnotes:\n");
        for n in &r.notes {
            o.push_str(&format!("  - {n}\n"));
        }
    }
    if let Some(c) = &r.cost {
        o.push_str(&format!(
            "\nGraphQL: {} queries, {} points{}\n",
            c.queries,
            c.points_used,
            c.points_remaining
                .map(|p| format!(", {p} remaining"))
                .unwrap_or_default()
        ));
    }
    o
}

fn details(f: &Finding) -> Vec<String> {
    let at = |e: &squabble_core::chains::Edge| {
        format!("{}:{} @{}", e.from, e.file, short(e.pin.as_str()))
    };
    match f {
        Finding::Cycle { .. } => Vec::new(),
        Finding::DeadUpstream { consumers, .. } => consumers.iter().map(at).collect(),
        Finding::Diamond { routes, .. } => routes
            .iter()
            .map(|r| {
                let via = if r.via.is_empty() {
                    "direct".to_string()
                } else {
                    format!(
                        "via {}",
                        r.via
                            .iter()
                            .map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(" → ")
                    )
                };
                format!("@{} {via} (from {})", short(r.pin.as_str()), r.file)
            })
            .collect(),
        Finding::PinSkew { pins, .. } => pins
            .iter()
            .map(|p| {
                let hint = if p.version_hints.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", p.version_hints.join(", "))
                };
                let who: std::collections::BTreeSet<&str> =
                    p.consumers.iter().map(|e| e.from.as_str()).collect();
                format!(
                    "@{}{hint}: {}",
                    short(p.pin.as_str()),
                    who.into_iter().collect::<Vec<_>>().join(", ")
                )
            })
            .collect(),
        Finding::MutablePins { uses, .. } => uses
            .iter()
            .map(|e| format!("{} → {}@{}", e.file, e.to, e.pin.as_str()))
            .collect(),
    }
}

fn short(p: &str) -> &str {
    if p.len() == 40 {
        p.get(..12).unwrap_or(p)
    } else {
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// A fake GraphQL endpoint serving canned repos by slug.
    /// (slug, Some(files) | None = NOT_FOUND), files as (name, text).
    type CannedRepo = (&'static str, Option<Vec<(&'static str, &'static str)>>);
    struct Canned(Vec<CannedRepo>);
    impl GraphQlTransport for Canned {
        fn execute(&self, body: &Value) -> Result<Value, String> {
            let vars = body["variables"].as_object().unwrap();
            let mut data = serde_json::Map::new();
            let mut errors = Vec::new();
            for i in 0..vars.len() / 2 {
                let slug = format!(
                    "{}/{}",
                    vars[&format!("o{i}")].as_str().unwrap(),
                    vars[&format!("n{i}")].as_str().unwrap()
                );
                match self.0.iter().find(|(s, _)| s.eq_ignore_ascii_case(&slug)) {
                    Some((_, Some(files))) => {
                        let entries: Vec<Value> = files
                            .iter()
                            .map(|(n, t)| json!({"name": n, "type": "blob", "object": {"text": t, "isBinary": false, "isTruncated": false}}))
                            .collect();
                        data.insert(format!("r{i}"), json!({
                            "nameWithOwner": slug,
                            "defaultBranchRef": {"target": {"oid": "1111111111111111111111111111111111111111",
                                "file": {"object": {"entries": entries}}}}
                        }));
                    }
                    _ => {
                        data.insert(format!("r{i}"), Value::Null);
                        errors.push(json!({"type": "NOT_FOUND", "path": [format!("r{i}")], "message": "gone"}));
                    }
                }
            }
            data.insert(
                "rateLimit".into(),
                json!({"cost": 1, "remaining": 4000, "resetAt": "T"}),
            );
            Ok(json!({"data": data, "errors": errors}))
        }
    }

    const APP: &str = "jobs:\n  gov:\n    uses: me/standards/.github/workflows/gov.yml@1111111111111111111111111111111111111111\n  b:\n    steps:\n      - uses: me/dead-action@v1\n      - uses: actions/checkout@v4\n";
    const GOV: &str = "jobs:\n  x:\n    steps:\n      - uses: actions/checkout@v5\n";

    fn canned() -> Canned {
        Canned(vec![
            ("me/app", Some(vec![("ci.yml", APP)])),
            (
                "me/standards",
                Some(vec![
                    ("gov.yml", GOV),
                    ("own.yml", "jobs:\n  y:\n    steps:\n      - uses: x/y@v1\n"),
                ]),
            ),
            (
                "actions/checkout",
                Some(vec![(
                    "test.yml",
                    "jobs:\n  t:\n    steps:\n      - uses: actions/checkout@v3\n",
                )]),
            ),
        ])
    }

    fn args(follow: u32) -> Args {
        Args {
            sources: vec![Source::GraphQl(RepoId::new("me/app"))],
            follow,
            batch: 2,
            json: false,
        }
    }

    #[test]
    fn without_follow_upstreams_stay_on_the_frontier() {
        let r = collect(&args(0), &canned());
        assert!(!r.has_blocking());
        assert_eq!(r.frontier.len(), 3);
    }

    #[test]
    fn follow_finds_dead_upstream_and_diamond_without_importing_action_ci() {
        let r = collect(&args(1), &canned());
        assert!(r.has_blocking(), "{}", narrate(&r));
        assert!(r.findings.iter().any(|f| matches!(f, Finding::DeadUpstream { target, .. } if target == &RepoId::new("me/dead-action"))));
        assert!(r.findings.iter().any(|f| matches!(f, Finding::Diamond { target, .. } if target == &RepoId::new("actions/checkout"))));
        // actions/checkout was probed but its own CI (checkout@v3) never enters.
        assert!(!narrate(&r).contains("@v3"));
        // me/standards' uncalled own.yml never enters either.
        assert!(!r.frontier.contains(&RepoId::new("x/y")));
        let c = r.cost.unwrap();
        assert_eq!(c.queries, 3); // 1 subject + 2 batches of 3 upstreams
    }

    #[test]
    fn narration_names_every_scan_status() {
        let r = collect(&args(1), &canned());
        let text = narrate(&r);
        assert!(text.contains("me/dead-action: NOT FOUND"));
        assert!(text.contains("[BLOCKING] dead upstream"));
        assert!(text.contains("landing order"));
    }

    #[test]
    fn a_run_that_read_nothing_is_a_failure_not_clean() {
        struct Down;
        impl GraphQlTransport for Down {
            fn execute(&self, _: &Value) -> Result<Value, String> {
                Err("no gh".into())
            }
        }
        let r = collect(&args(0), &Down);
        assert_eq!(exit_for(&r), ExitCode::from(2));
        let ok = collect(&args(0), &canned());
        assert_eq!(exit_for(&ok), ExitCode::SUCCESS);
        let blocking = collect(&args(1), &canned());
        assert_eq!(exit_for(&blocking), ExitCode::from(BLOCKING_EXIT));
    }

    #[test]
    fn arg_parsing() {
        let a = parse_args(&[
            "./x=me/x".into(),
            "gh:me/y".into(),
            "--follow".into(),
            "2".into(),
            "--json".into(),
        ])
        .unwrap();
        assert_eq!(a.sources.len(), 2);
        assert_eq!(a.follow, 2);
        assert!(a.json);
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&["--batch".into(), "0".into(), "x".into()]).is_err());
        assert!(parse_args(&["--nope".into()]).is_err());
    }
}
