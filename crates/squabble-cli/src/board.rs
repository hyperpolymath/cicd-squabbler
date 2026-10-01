// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble board` — the estate "needs me" board.
//!
//! Enumerates every non-archived repository of each owner, reads the merge
//! gate and open PRs of the repos that have any, places each PR in one bucket
//! (`squabble_core::board::classify`) and renders Markdown — or JSON with
//! `--json`.
//!
//! `--publish owner/repo#N` rewrites the *body* of issue N with the board. It
//! never comments, so a run never notifies anyone. That edit is the only write
//! this subcommand can make.
//!
//! Exit `0` = complete board; `6` = board produced but incomplete (a repo was
//! unreadable or had more open PRs than one page) — it is still rendered and
//! still published, with the gap stated at the top; `2` = usage or tool
//! failure, including any owner whose enumeration failed.

use squabble_core::board::{
    classify, epoch_day, render_markdown, rfc3339_from_unix, Board, ISSUE_BODY_LIMIT,
};
use squabble_core::chains::RepoId;
use squabble_forge::board::{enumerate_owner, fetch_board, RepoRead, BOARD_BATCH};
use squabble_forge::{GhTransport, GraphQlTransport};
use std::io::Write;
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const USAGE: &str = "usage: squabble board [--owners a,b] [--stale-days N] [--batch N] [--json] [--out FILE] [--publish owner/repo#N]";

/// Exit code for a board that was produced but has stated gaps.
pub(crate) const INCOMPLETE_EXIT: u8 = 6;

/// Owners read when `--owners` is not given.
const DEFAULT_OWNERS: &[&str] = &["hyperpolymath", "metadatastician"];

struct Args {
    owners: Vec<String>,
    stale_days: i64,
    batch: usize,
    json: bool,
    out: Option<String>,
    publish: Option<(String, u64)>,
}

/// Parse `owner/repo#N`.
fn parse_issue_ref(s: &str) -> Option<(String, u64)> {
    let (slug, n) = s.split_once('#')?;
    let (o, r) = slug.split_once('/')?;
    if o.is_empty() || r.is_empty() || r.contains('/') {
        return None;
    }
    Some((slug.to_string(), n.parse().ok().filter(|&n: &u64| n > 0)?))
}

/// Parse the flags after `squabble board`.
fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut a = Args {
        owners: DEFAULT_OWNERS.iter().map(|s| s.to_string()).collect(),
        stale_days: 30,
        batch: BOARD_BATCH,
        json: false,
        out: None,
        publish: None,
    };
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => a.json = true,
            "--owners" => {
                a.owners = it
                    .next()
                    .map(|v| {
                        v.split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .filter(|v| !v.is_empty())
                    .ok_or("--owners needs a comma-separated list")?
            }
            "--stale-days" => {
                a.stale_days = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|&n: &i64| n > 0)
                    .ok_or("--stale-days needs a positive integer")?
            }
            "--batch" => {
                a.batch = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|&n: &usize| n > 0)
                    .ok_or("--batch needs a positive integer")?
            }
            "--out" => a.out = Some(it.next().ok_or("--out needs a file path")?.clone()),
            "--publish" => {
                a.publish = Some(
                    it.next()
                        .and_then(|v| parse_issue_ref(v))
                        .ok_or("--publish needs owner/repo#N")?,
                )
            }
            s => return Err(format!("unexpected argument `{s}`\n{USAGE}")),
        }
    }
    Ok(a)
}

/// Build the board for `owners` through `transport`. `Err` when any owner
/// cannot be enumerated: a board missing a whole owner would look complete.
fn build(
    transport: &dyn GraphQlTransport,
    owners: &[String],
    batch: usize,
    now_secs: u64,
    stale_days: i64,
) -> Result<Board, String> {
    let generated_at = rfc3339_from_unix(now_secs);
    let today = epoch_day(&generated_at).ok_or("clock produced an unparseable date")?;
    let mut board = Board {
        generated_at,
        owners: owners.to_vec(),
        ..Board::default()
    };
    let mut with_prs: Vec<RepoId> = Vec::new();
    for owner in owners {
        let listing = enumerate_owner(transport, owner)?;
        board
            .repos_enumerated
            .insert(owner.clone(), listing.repos.len());
        board
            .open_prs_reported
            .insert(owner.clone(), listing.open_prs());
        with_prs.extend(
            listing
                .repos
                .into_iter()
                .filter(|(_, n)| *n > 0)
                .map(|(r, _)| r),
        );
    }
    let (reads, _queries) = fetch_board(transport, &with_prs, batch);
    for (repo, read) in with_prs.iter().zip(reads) {
        match read {
            RepoRead::Unavailable(why) => board.unavailable.push((repo.clone(), why)),
            RepoRead::Read {
                gate,
                prs,
                truncated,
            } => {
                if truncated {
                    board.truncated.push(repo.clone());
                }
                for pr in prs {
                    let reason = classify(&pr, &gate, today, stale_days);
                    board.placed.push((pr, reason));
                }
            }
        }
    }
    Ok(board)
}

/// Replace the body of `slug#number` with `body` via `gh api`, then confirm
/// the stored body is the one sent.
fn publish(slug: &str, number: u64, body: &str) -> Result<(), String> {
    let payload = serde_json::json!({ "body": body }).to_string();
    let mut child = Command::new("gh")
        .args([
            "api",
            "-X",
            "PATCH",
            &format!("repos/{slug}/issues/{number}"),
            "--input",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to run `gh api`: {e}"))?;
    child
        .stdin
        .as_mut()
        .ok_or("could not open stdin for `gh`")?
        .write_all(payload.as_bytes())
        .map_err(|e| format!("could not write to `gh`: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("`gh api` did not finish: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "PATCH {slug}#{number} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // rc=0 is not evidence: check what GitHub actually stored.
    let stored: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("PATCH {slug}#{number}: response was not JSON: {e}"))?;
    match stored.get("body").and_then(serde_json::Value::as_str) {
        Some(b) if b == body => Ok(()),
        Some(b) => Err(format!(
            "PATCH {slug}#{number}: stored body differs from the one sent ({} vs {} bytes)",
            b.len(),
            body.len()
        )),
        None => Err(format!("PATCH {slug}#{number}: response carried no body")),
    }
}

/// Entry point for `squabble board <flags>`.
pub(crate) fn run(rest: &[String]) -> ExitCode {
    let args = match parse_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("squabble board: {e}");
            return ExitCode::from(2);
        }
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let board = match build(&GhTransport, &args.owners, args.batch, now, args.stale_days) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("squabble board: {e}");
            return ExitCode::from(2);
        }
    };
    let md = render_markdown(&board, ISSUE_BODY_LIMIT);
    let text = if args.json {
        match serde_json::to_string_pretty(&board) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("squabble board: could not serialise: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        md.clone()
    };
    match &args.out {
        Some(path) => {
            if let Err(e) = std::fs::write(path, &text) {
                eprintln!("squabble board: cannot write `{path}`: {e}");
                return ExitCode::from(2);
            }
        }
        None => println!("{text}"),
    }
    let placed = board.placed_per_owner();
    for o in &board.owners {
        eprintln!(
            "squabble board: {o}: {} repos, {} open PRs reported, {} placed",
            board.repos_enumerated.get(o).copied().unwrap_or(0),
            board.open_prs_reported.get(o).copied().unwrap_or(0),
            placed.get(o).copied().unwrap_or(0)
        );
    }
    if let Some((slug, n)) = &args.publish {
        if let Err(e) = publish(slug, *n, &md) {
            eprintln!("squabble board: {e}");
            return ExitCode::from(2);
        }
        eprintln!(
            "squabble board: published to {slug}#{n} ({} bytes)",
            md.len()
        );
    }
    if board.unavailable.is_empty() && board.truncated.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "squabble board: INCOMPLETE — {} unreadable, {} truncated",
            board.unavailable.len(),
            board.truncated.len()
        );
        ExitCode::from(INCOMPLETE_EXIT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use squabble_core::board::{Bucket, Reason};
    use std::cell::RefCell;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn publish_ref_parses_and_rejects() {
        assert_eq!(
            parse_issue_ref("hyperpolymath/standards#12"),
            Some(("hyperpolymath/standards".into(), 12))
        );
        for bad in ["standards#12", "a/b#0", "a/b#x", "a/b/c#1", "/b#1", "a/b"] {
            assert_eq!(parse_issue_ref(bad), None, "{bad}");
        }
    }

    #[test]
    fn flags_parse_and_unknown_flags_fail() {
        let a = parse_args(&args(&["--owners", "x, y", "--stale-days", "7", "--json"])).unwrap();
        assert_eq!(a.owners, vec!["x", "y"]);
        assert_eq!(a.stale_days, 7);
        assert!(a.json);
        assert!(parse_args(&args(&["--nope"])).is_err());
        assert!(parse_args(&args(&["--publish", "bad"])).is_err());
    }

    /// Answers listing queries and board queries from fixed fixtures.
    struct Fake {
        calls: RefCell<usize>,
    }

    impl GraphQlTransport for Fake {
        fn execute(&self, body: &Value) -> Result<Value, String> {
            *self.calls.borrow_mut() += 1;
            let rate = json!({ "cost": 1, "remaining": 4000, "resetAt": null });
            if body["variables"].get("login").is_some() {
                return Ok(
                    json!({ "data": { "rateLimit": rate, "repositoryOwner": { "repositories": {
                        "totalCount": 3,
                        "pageInfo": { "hasNextPage": false, "endCursor": null },
                        "nodes": [
                            { "nameWithOwner": "me/gated", "pullRequests": { "totalCount": 2 } },
                            { "nameWithOwner": "me/bare", "pullRequests": { "totalCount": 1 } },
                            { "nameWithOwner": "me/quiet", "pullRequests": { "totalCount": 0 } }
                        ]
                    }}}}),
                );
            }
            let pr = |n: u64, state: &str, armed: bool| {
                json!({
                    "number": n, "title": "t", "url": format!("u{n}"), "isDraft": false,
                    "mergeable": "MERGEABLE", "mergeStateStatus": state, "reviewDecision": null,
                    "updatedAt": "2026-10-01T00:00:00Z", "headRefName": "h", "author": { "login": "a" },
                    "autoMergeRequest": if armed { json!({ "enabledAt": "x" }) } else { Value::Null },
                    "commits": { "nodes": [ { "commit": { "statusCheckRollup": { "state": "PENDING" } } } ] }
                })
            };
            let mut data = serde_json::Map::new();
            data.insert("rateLimit".into(), rate);
            for (i, name) in ["o0", "o1"].iter().enumerate() {
                if body["variables"].get(*name).is_none() {
                    continue;
                }
                let repo = body["variables"][format!("n{i}")].as_str().unwrap();
                let node = if repo == "gated" {
                    json!({ "nameWithOwner": "me/gated",
                        "defaultBranchRef": { "name": "main", "branchProtectionRule": null,
                            "rules": { "nodes": [ { "type": "REQUIRED_STATUS_CHECKS",
                                "parameters": { "requiredStatusChecks": [ { "context": "ci" } ] } } ] } },
                        "pullRequests": { "totalCount": 2, "nodes": [ pr(1, "CLEAN", false), pr(2, "BLOCKED", true) ] } })
                } else {
                    json!({ "nameWithOwner": "me/bare",
                        "defaultBranchRef": { "name": "main", "branchProtectionRule": null, "rules": { "nodes": [] } },
                        "pullRequests": { "totalCount": 1, "nodes": [ pr(3, "BLOCKED", false) ] } })
                };
                data.insert(format!("r{i}"), node);
            }
            Ok(json!({ "data": data }))
        }
    }

    #[test]
    fn planted_controls_land_in_their_buckets_and_counts_reconcile() {
        // 2026-10-01T12:00:00Z
        let board = build(
            &Fake {
                calls: RefCell::new(0),
            },
            &["me".into()],
            10,
            1_790_856_000,
            30,
        )
        .unwrap();
        let reason_of = |n: u64| board.placed.iter().find(|(p, _)| p.number == n).unwrap().1;
        assert_eq!(reason_of(1), Reason::CleanMergeByHand);
        assert_eq!(reason_of(2), Reason::ArmedWaiting);
        assert_eq!(reason_of(3), Reason::NoRequiredGate);
        assert_eq!(reason_of(3).bucket(), Bucket::NeedsYou);
        // Enumerated vs placed: the set difference is empty.
        assert_eq!(board.open_prs_reported["me"], 3);
        assert_eq!(board.placed_per_owner()["me"], 3);
        assert!(board.unavailable.is_empty() && board.truncated.is_empty());
        assert_eq!(board.repos_enumerated["me"], 3);
    }

    #[test]
    fn repos_without_open_prs_cost_no_detail_query() {
        let fake = Fake {
            calls: RefCell::new(0),
        };
        build(&fake, &["me".into()], 10, 1_790_856_000, 30).unwrap();
        // One listing page + one board batch (gated, bare); `quiet` not read.
        assert_eq!(*fake.calls.borrow(), 2);
    }
}
