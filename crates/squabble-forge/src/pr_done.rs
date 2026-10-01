// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! The GraphQL half of `verify-satisfied`: one pull request's mergeability,
//! reviews, threads and head contexts, fully paginated.
//!
//! The REST-only half — required contexts (rulesets ∪ classic protection) and
//! the ruleset rule types — is filled in by the CLI, which already reads both.

use crate::GraphQlTransport;
use serde_json::{json, Value};
use squabble_core::done::{Mergeability, Observed, PrFacts, PrState, Thread};
use squabble_core::gate::CheckRun;

/// The query document; see `graphql/pr_done.graphql`.
pub const PR_DONE: &str = include_str!("../graphql/pr_done.graphql");

/// A safety bound on pages per connection. At 100 per page this is 2 000
/// contexts or threads; past it the read fails rather than truncating.
pub const MAX_PAGES: usize = 20;

/// What the GraphQL read yields. `facts.required_contexts` and
/// `facts.rule_types` are left empty for the caller to fill from REST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRead {
    pub base_ref: String,
    pub head_oid: String,
    pub facts: PrFacts,
}

fn s<'a>(v: &'a Value, ptr: &str) -> Option<&'a str> {
    v.pointer(ptr).and_then(Value::as_str)
}

fn login(v: &Value, ptr: &str) -> String {
    s(v, ptr)
        .map(|l| l.strip_suffix("[bot]").unwrap_or(l).to_string())
        .unwrap_or_else(|| "ghost".into())
}

fn observed(node: &Value) -> Option<Observed> {
    match s(node, "/__typename")? {
        "CheckRun" => Some(Observed {
            name: s(node, "/name")?.to_string(),
            producer: login(node, "/checkSuite/app/slug"),
            run: CheckRun::from_github(s(node, "/status"), s(node, "/conclusion")),
        }),
        "StatusContext" => Some(Observed {
            name: s(node, "/context")?.to_string(),
            producer: login(node, "/creator/login"),
            run: CheckRun::from_github(None, s(node, "/state")),
        }),
        _ => None,
    }
}

fn thread(node: &Value) -> Option<Thread> {
    if node.get("isResolved")?.as_bool()? {
        return None;
    }
    let first = node
        .pointer("/comments/nodes/0")
        .cloned()
        .unwrap_or(json!({}));
    Some(Thread {
        author: login(&first, "/author/login"),
        path: s(node, "/path").map(str::to_string),
        line: node.get("line").and_then(Value::as_u64),
        outdated: node
            .get("isOutdated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        url: s(&first, "/url").unwrap_or_default().to_string(),
    })
}

/// A connection's page: its nodes and, if more follow, the cursor.
fn page(conn: &Value) -> Result<(&[Value], Option<String>), String> {
    let nodes = conn
        .get("nodes")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or("connection without nodes")?;
    let more = conn
        .pointer("/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .ok_or("connection without pageInfo")?;
    let next = if more {
        Some(
            s(conn, "/pageInfo/endCursor")
                .ok_or("hasNextPage without endCursor")?
                .to_string(),
        )
    } else {
        None
    };
    Ok((nodes, next))
}

/// Read one PR. Fail-closed: GraphQL `errors`, a missing PR, or a connection
/// that will not finish within [`MAX_PAGES`] is an `Err`, never a partial read.
pub fn fetch_pr_done(
    t: &dyn GraphQlTransport,
    owner: &str,
    name: &str,
    number: u64,
) -> Result<PrRead, String> {
    let mut ctx_after: Option<String> = None;
    let mut thr_after: Option<String> = None;
    let (mut ctx_done, mut thr_done) = (false, false);
    let mut observed_all = Vec::new();
    let mut threads = Vec::new();
    let mut head: Option<Value> = None;

    for _ in 0..MAX_PAGES {
        let body = json!({
            "query": PR_DONE,
            "variables": {
                "owner": owner, "name": name, "number": number,
                "ctxAfter": ctx_after, "thrAfter": thr_after,
            }
        });
        let resp = t.execute(&body)?;
        if let Some(errs) = resp.get("errors").and_then(Value::as_array) {
            let msgs: Vec<&str> = errs
                .iter()
                .filter_map(|e| e.get("message").and_then(Value::as_str))
                .collect();
            return Err(format!("GraphQL errors: {}", msgs.join("; ")));
        }
        let pr = resp
            .pointer("/data/repository/pullRequest")
            .filter(|p| p.is_object())
            .ok_or_else(|| format!("{owner}/{name}#{number}: no such pull request"))?;

        if !thr_done {
            let (nodes, next) = page(pr.get("reviewThreads").ok_or("no reviewThreads")?)?;
            threads.extend(nodes.iter().filter_map(thread));
            thr_done = next.is_none();
            thr_after = next.or(thr_after);
        }
        if !ctx_done {
            // No rollup at all (a head nothing has reported on) is an empty set.
            match pr.pointer("/commits/nodes/0/commit/statusCheckRollup/contexts") {
                Some(conn) if conn.is_object() => {
                    let (nodes, next) = page(conn)?;
                    observed_all.extend(nodes.iter().filter_map(observed));
                    ctx_done = next.is_none();
                    ctx_after = next.or(ctx_after);
                }
                _ => ctx_done = true,
            }
        }
        head.get_or_insert_with(|| pr.clone());
        if ctx_done && thr_done {
            let pr = head.expect("set above");
            return Ok(PrRead {
                base_ref: s(&pr, "/baseRefName").unwrap_or_default().to_string(),
                head_oid: s(&pr, "/headRefOid").unwrap_or_default().to_string(),
                facts: PrFacts {
                    state: match s(&pr, "/state") {
                        Some("MERGED") => PrState::Merged,
                        Some("CLOSED") => PrState::Closed,
                        _ => PrState::Open,
                    },
                    is_draft: pr.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
                    mergeable: match s(&pr, "/mergeable") {
                        Some("MERGEABLE") => Mergeability::Mergeable,
                        Some("CONFLICTING") => Mergeability::Conflicting,
                        _ => Mergeability::Unknown,
                    },
                    merge_state: s(&pr, "/mergeStateStatus").unwrap_or("UNKNOWN").to_string(),
                    review_decision: s(&pr, "/reviewDecision").map(str::to_string),
                    auto_merge: s(&pr, "/autoMergeRequest/mergeMethod").map(str::to_string),
                    required_contexts: Vec::new(),
                    observed: observed_all,
                    unresolved_threads: threads,
                    changes_requested_by: pr
                        .pointer("/latestOpinionatedReviews/nodes")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter(|r| s(r, "/state") == Some("CHANGES_REQUESTED"))
                                .map(|r| login(r, "/author/login"))
                                .collect()
                        })
                        .unwrap_or_default(),
                    rule_types: Vec::new(),
                    body: s(&pr, "/body").unwrap_or_default().to_string(),
                },
            });
        }
    }
    Err(format!(
        "{owner}/{name}#{number}: more than {MAX_PAGES} pages of contexts or threads — \
         refusing to judge a partial read"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Replays canned pages and records the cursors each request carried.
    struct Pages {
        pages: RefCell<Vec<Value>>,
        seen: RefCell<Vec<(Value, Value)>>,
    }
    impl GraphQlTransport for Pages {
        fn execute(&self, body: &Value) -> Result<Value, String> {
            self.seen.borrow_mut().push((
                body["variables"]["ctxAfter"].clone(),
                body["variables"]["thrAfter"].clone(),
            ));
            let mut p = self.pages.borrow_mut();
            if p.is_empty() {
                return Err("no more pages".into());
            }
            Ok(p.remove(0))
        }
    }

    fn resp(ctx: Value, ctx_next: Option<&str>, thr: Value, thr_next: Option<&str>) -> Value {
        json!({ "data": { "repository": { "pullRequest": {
            "state": "OPEN", "isDraft": false, "mergeable": "MERGEABLE",
            "mergeStateStatus": "BLOCKED", "reviewDecision": null,
            "baseRefName": "main", "headRefOid": "abc", "body": "ack `x` #1",
            "autoMergeRequest": { "mergeMethod": "SQUASH" },
            "latestOpinionatedReviews": { "nodes": [
                { "state": "CHANGES_REQUESTED", "author": { "login": "coderabbitai" } },
                { "state": "APPROVED", "author": { "login": "owner" } }
            ]},
            "reviewThreads": {
                "pageInfo": { "hasNextPage": thr_next.is_some(), "endCursor": thr_next },
                "nodes": thr
            },
            "commits": { "nodes": [ { "commit": { "statusCheckRollup": { "contexts": {
                "pageInfo": { "hasNextPage": ctx_next.is_some(), "endCursor": ctx_next },
                "nodes": ctx
            }}}}]}
        }}}})
    }

    fn run(name: &str) -> Value {
        json!({ "__typename": "CheckRun", "name": name, "status": "COMPLETED",
                "conclusion": "SUCCESS", "checkSuite": { "app": { "slug": "github-actions" } } })
    }

    #[test]
    fn the_query_validates_against_the_schema_snapshot() {
        crate::tests::validate(PR_DONE).unwrap();
    }

    /// The defect this module exists to avoid: a second page of contexts must
    /// be read, and a connection that already finished must not be re-counted.
    #[test]
    fn contexts_past_the_first_page_are_read_and_finished_threads_are_not_repeated() {
        let t1 = json!([{ "isResolved": false, "isOutdated": true, "path": "a.rs", "line": 3,
                          "comments": { "nodes": [ { "author": { "login": "coderabbitai" }, "url": "u1" } ] } },
                        { "isResolved": true, "isOutdated": false, "path": "b.rs", "line": 1,
                          "comments": { "nodes": [] } }]);
        let bot = json!({ "__typename": "StatusContext", "context": "CodeRabbit",
                          "state": "PENDING", "creator": { "login": "coderabbitai[bot]" } });
        let p = Pages {
            pages: RefCell::new(vec![
                resp(json!([run("build")]), Some("C1"), t1.clone(), None),
                resp(json!([bot]), None, t1, None),
            ]),
            seen: RefCell::new(vec![]),
        };
        let r = fetch_pr_done(&p, "o", "r", 1).unwrap();
        let names: Vec<_> = r.facts.observed.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["build", "CodeRabbit"]);
        assert_eq!(r.facts.observed[1].producer, "coderabbitai");
        assert_eq!(r.facts.observed[1].run, CheckRun::Pending);
        assert_eq!(
            r.facts.unresolved_threads.len(),
            1,
            "resolved skipped, no repeat"
        );
        assert!(r.facts.unresolved_threads[0].outdated);
        assert_eq!(r.facts.changes_requested_by, ["coderabbitai"]);
        assert_eq!(r.facts.body, "ack `x` #1");
        assert_eq!(r.facts.auto_merge.as_deref(), Some("SQUASH"));
        assert_eq!(
            p.seen.borrow()[1].0,
            json!("C1"),
            "second request carried the cursor"
        );
    }

    #[test]
    fn graphql_errors_fail_closed() {
        let p = Pages {
            pages: RefCell::new(vec![json!({ "errors": [ { "message": "rate limited" } ] })]),
            seen: RefCell::new(vec![]),
        };
        assert!(fetch_pr_done(&p, "o", "r", 1)
            .unwrap_err()
            .contains("rate limited"));
    }

    #[test]
    fn a_connection_that_never_ends_is_refused_not_truncated() {
        let pages = (0..MAX_PAGES)
            .map(|i| {
                resp(
                    json!([run(&format!("c{i}"))]),
                    Some("more"),
                    json!([]),
                    None,
                )
            })
            .collect();
        let p = Pages {
            pages: RefCell::new(pages),
            seen: RefCell::new(vec![]),
        };
        assert!(fetch_pr_done(&p, "o", "r", 1)
            .unwrap_err()
            .contains("partial read"));
    }
}
