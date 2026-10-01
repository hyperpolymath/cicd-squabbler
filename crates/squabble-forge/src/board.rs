// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Forge reads for `squabble board`: enumerate an owner's repositories, then
//! read each repo's merge gate and open pull requests in batched queries.
//!
//! Same contract as the crate root: variables never interpolated, every
//! document schema-validated in tests, and fail-closed — a repo that could not
//! be read is reported with its reason, never silently dropped, and a short
//! enumeration (fewer repos listed than `totalCount`) is an error.

use crate::{split_slug, GraphQlTransport, RateSample};
use serde_json::{json, Value};
use squabble_core::board::{Mergeable, PrFacts, RepoGate};
use squabble_core::chains::RepoId;
use std::collections::BTreeSet;

/// Paged repository enumeration for one owner.
pub const ESTATE_REPOS: &str = include_str!("../graphql/estate_repos.graphql");
/// Per-repo gate + open PRs, aliased once per repo by [`board_query`].
pub const BOARD_REPO: &str = include_str!("../graphql/board_repo.graphql");

/// Repos per board query. PR nodes are heavier than workflow trees, so this
/// is smaller than [`crate::DEFAULT_BATCH`].
pub const BOARD_BATCH: usize = 10;

/// GitHub's page size for the PR connection in [`BOARD_REPO`].
pub const PR_PAGE: usize = 100;

/// Every non-archived repo an owner owns, with its open-PR total.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnerListing {
    pub login: String,
    pub repos: Vec<(RepoId, usize)>,
    pub rate: Option<RateSample>,
}

impl OwnerListing {
    /// Sum of the per-repo open-PR totals — the board's denominator.
    pub fn open_prs(&self) -> usize {
        self.repos.iter().map(|(_, n)| n).sum()
    }
}

/// The `rateLimit` block of a response, when it carried one.
fn rate_of(resp: &Value) -> Option<RateSample> {
    let r = resp.pointer("/data/rateLimit")?;
    Some(RateSample {
        cost: r.get("cost")?.as_u64()?,
        remaining: r.get("remaining")?.as_u64()?,
        reset_at: r.get("resetAt").and_then(Value::as_str).map(str::to_string),
    })
}

/// A one-line reason for a response that carried no usable data.
fn error_text(resp: &Value) -> String {
    let msgs: Vec<&str> = resp
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| e.get("message").and_then(Value::as_str))
        .collect();
    if msgs.is_empty() {
        // A REST-shaped error body (`{"message": …}`) arrives from gateway
        // failures; say so rather than "no data".
        resp.get("message").and_then(Value::as_str).map_or_else(
            || format!("response carried no data: {}", short(resp)),
            str::to_string,
        )
    } else {
        msgs.join("; ")
    }
}

/// First 200 characters of a response, for an error message.
fn short(v: &Value) -> String {
    v.to_string().chars().take(200).collect()
}

/// Attempts per listing page. A gateway 502 on one page would otherwise
/// abort the whole board.
const LISTING_ATTEMPTS: u32 = 3;

/// Execute `body`, retrying only a transport failure (no JSON came back)
/// with 1 s, 2 s, … back-off. A GraphQL error is an answer, not retried.
fn execute_with_retry(
    transport: &dyn GraphQlTransport,
    body: &Value,
    attempts: u32,
) -> Result<Value, String> {
    let mut last = String::new();
    for n in 0..attempts.max(1) {
        if n > 0 {
            std::thread::sleep(std::time::Duration::from_secs(1 << (n - 1)));
        }
        match transport.execute(body) {
            Ok(v) => return Ok(v),
            Err(e) => last = e,
        }
    }
    Err(format!("{last} (after {} attempts)", attempts.max(1)))
}

/// List every non-archived repository `login` owns, following cursors to the
/// end. Errs on any page that carries no data, and on a listing whose length
/// disagrees with GitHub's own `totalCount`.
pub fn enumerate_owner(
    transport: &dyn GraphQlTransport,
    login: &str,
) -> Result<OwnerListing, String> {
    let mut out = OwnerListing {
        login: login.to_string(),
        ..OwnerListing::default()
    };
    let mut after: Option<String> = None;
    let mut total: Option<u64> = None;
    loop {
        let body = json!({
            "query": ESTATE_REPOS,
            "variables": { "login": login, "after": after },
        });
        let resp = execute_with_retry(transport, &body, LISTING_ATTEMPTS)?;
        out.rate = rate_of(&resp).or(out.rate);
        let conn = resp
            .pointer("/data/repositoryOwner/repositories")
            .filter(|c| c.is_object())
            .ok_or_else(|| format!("listing `{login}`: {}", error_text(&resp)))?;
        total = conn.get("totalCount").and_then(Value::as_u64).or(total);
        for n in conn
            .get("nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(slug) = n.get("nameWithOwner").and_then(Value::as_str) else {
                return Err(format!("listing `{login}`: a repository node had no name"));
            };
            let open = n
                .pointer("/pullRequests/totalCount")
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("listing `{login}`: `{slug}` had no PR count"))?;
            out.repos.push((RepoId::new(slug), open as usize));
        }
        let next = conn
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        after = conn
            .pointer("/pageInfo/endCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        if !next || after.is_none() {
            break;
        }
    }
    match total {
        Some(t) if t as usize == out.repos.len() => Ok(out),
        Some(t) => Err(format!(
            "listing `{login}`: GitHub reports {t} repositories but {} were listed",
            out.repos.len()
        )),
        None => Err(format!("listing `{login}`: no totalCount in the response")),
    }
}

/// Build one batched board query: rate probe plus [`BOARD_REPO`] aliased
/// `r0…rN`, owner/name passed as variables.
pub fn board_query(batch: &[RepoId]) -> (String, Value) {
    let mut params = Vec::new();
    let mut fields = Vec::new();
    let mut vars = serde_json::Map::new();
    for (i, repo) in batch.iter().enumerate() {
        let (o, n) = split_slug(repo).unwrap_or(("", ""));
        params.push(format!("$o{i}: String!, $n{i}: String!"));
        fields.push(format!(
            "  r{i}: repository(owner: $o{i}, name: $n{i}) {{ ...BoardRepo }}"
        ));
        vars.insert(format!("o{i}"), json!(o));
        vars.insert(format!("n{i}"), json!(n));
    }
    let doc = format!(
        "query EstateBoard({}) {{\n  rateLimit {{ cost remaining resetAt }}\n{}\n}}\n{}",
        params.join(", "),
        fields.join("\n"),
        BOARD_REPO
    );
    (doc, Value::Object(vars))
}

/// What one repo's board read produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoRead {
    Read {
        gate: RepoGate,
        prs: Vec<PrFacts>,
        /// More open PRs exist than one page returned.
        truncated: bool,
    },
    Unavailable(String),
}

/// Turn one batched board response into reads, in `batch` order.
pub fn parse_board_response(batch: &[RepoId], resp: &Value) -> (Vec<RepoRead>, Option<RateSample>) {
    let rate = rate_of(resp);
    let data = resp.get("data").filter(|d| d.is_object());
    let alias_error = |alias: &str| -> Option<String> {
        resp.get("errors")
            .and_then(Value::as_array)?
            .iter()
            .find(|e| e.pointer("/path/0").and_then(Value::as_str) == Some(alias))
            .map(|e| {
                e.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("GraphQL error without a message")
                    .to_string()
            })
    };
    let reads = batch
        .iter()
        .enumerate()
        .map(|(i, repo)| {
            let alias = format!("r{i}");
            let Some(data) = data else {
                return RepoRead::Unavailable(error_text(resp));
            };
            match data.get(&alias) {
                Some(node) if node.is_object() => parse_board_repo(repo, node),
                _ => RepoRead::Unavailable(
                    alias_error(&alias)
                        .unwrap_or_else(|| "null repository with no matching error".into()),
                ),
            }
        })
        .collect();
    (reads, rate)
}

/// Read one repository alias: its merge gate (rulesets ∪ classic protection) and open PRs.
fn parse_board_repo(repo: &RepoId, node: &Value) -> RepoRead {
    let mut contexts: BTreeSet<String> = BTreeSet::new();
    let mut approvals: u32 = 0;
    let branch = node.get("defaultBranchRef").filter(|b| b.is_object());
    for rule in branch
        .and_then(|b| b.pointer("/rules/nodes"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let params = rule.get("parameters");
        match rule.get("type").and_then(Value::as_str) {
            Some("REQUIRED_STATUS_CHECKS") => {
                for c in params
                    .and_then(|p| p.get("requiredStatusChecks"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(ctx) = c.get("context").and_then(Value::as_str) {
                        contexts.insert(ctx.to_string());
                    }
                }
            }
            Some("PULL_REQUEST") => {
                let n = params
                    .and_then(|p| p.get("requiredApprovingReviewCount"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32;
                approvals = approvals.max(n);
            }
            _ => {}
        }
    }
    if let Some(bp) = branch
        .and_then(|b| b.get("branchProtectionRule"))
        .filter(|b| b.is_object())
    {
        if bp.get("requiresStatusChecks").and_then(Value::as_bool) == Some(true) {
            for c in bp
                .get("requiredStatusCheckContexts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                contexts.insert(c.to_string());
            }
        }
        if bp.get("requiresApprovingReviews").and_then(Value::as_bool) == Some(true) {
            let n = bp
                .get("requiredApprovingReviewCount")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            approvals = approvals.max(n);
        }
    }
    let gate = RepoGate {
        repo: repo.clone(),
        required_contexts: contexts.len(),
        required_approvals: approvals,
    };

    let conn = node.get("pullRequests");
    let total = conn
        .and_then(|c| c.get("totalCount"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let mut prs = Vec::new();
    for p in conn
        .and_then(|c| c.get("nodes"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let s = |k: &str| {
            p.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let Some(number) = p.get("number").and_then(Value::as_u64) else {
            return RepoRead::Unavailable("a pull request node had no number".into());
        };
        prs.push(PrFacts {
            repo: repo.clone(),
            number,
            title: s("title"),
            url: s("url"),
            author: p
                .pointer("/author/login")
                .and_then(Value::as_str)
                .unwrap_or("ghost")
                .to_string(),
            head_ref: s("headRefName"),
            is_draft: p.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
            mergeable: match p.get("mergeable").and_then(Value::as_str) {
                Some("MERGEABLE") => Mergeable::Mergeable,
                Some("CONFLICTING") => Mergeable::Conflicting,
                _ => Mergeable::Unknown,
            },
            merge_state: p
                .get("mergeStateStatus")
                .and_then(Value::as_str)
                .unwrap_or("UNKNOWN")
                .to_string(),
            review_decision: p
                .get("reviewDecision")
                .and_then(Value::as_str)
                .map(str::to_string),
            auto_merge_armed: p.get("autoMergeRequest").is_some_and(Value::is_object),
            rollup: p
                .pointer("/commits/nodes/0/commit/statusCheckRollup/state")
                .and_then(Value::as_str)
                .map(str::to_string),
            updated_at: s("updatedAt"),
        });
    }
    RepoRead::Read {
        gate,
        truncated: total > prs.len(),
        prs,
    }
}

/// Read gates and open PRs for `repos` in batches of `batch_size`. Returns
/// one read per repo, in order, and the number of queries spent.
///
/// A batch that fails *as a whole* (a gateway 502, a timeout, a body with no
/// `data`) is split in half and each half retried, down to a single repo,
/// which gets one more attempt. A heavy repo therefore cannot take its
/// neighbours down with it. Per-repo errors inside a good response are not
/// retried: they are about that repo.
///
/// When the remaining GraphQL budget would not cover another query of the
/// same cost, every later repo is [`RepoRead::Unavailable`] with the reset
/// time — never skipped silently.
pub fn fetch_board(
    transport: &dyn GraphQlTransport,
    repos: &[RepoId],
    batch_size: usize,
) -> (Vec<RepoRead>, usize) {
    let mut st = FetchState::default();
    let mut out: Vec<Option<RepoRead>> = vec![None; repos.len()];
    let valid: Vec<usize> = (0..repos.len())
        .filter(|&i| {
            let ok = split_slug(&repos[i]).is_some();
            if !ok {
                out[i] = Some(RepoRead::Unavailable("not an `owner/name` slug".into()));
            }
            ok
        })
        .collect();
    for chunk in valid.chunks(batch_size.max(1)) {
        let batch: Vec<RepoId> = chunk.iter().map(|&i| repos[i].clone()).collect();
        for (&i, read) in chunk.iter().zip(read_chunk(transport, &batch, &mut st, 1)) {
            out[i] = Some(read);
        }
    }
    let reads = out
        .into_iter()
        .map(|r| r.unwrap_or_else(|| RepoRead::Unavailable("not fetched".into())))
        .collect();
    (reads, st.queries)
}

#[derive(Default)]
struct FetchState {
    queries: usize,
    exhausted: Option<String>,
}

/// One batch, split-and-retried on whole-batch failure (see [`fetch_board`]).
fn read_chunk(
    transport: &dyn GraphQlTransport,
    batch: &[RepoId],
    st: &mut FetchState,
    retries: u8,
) -> Vec<RepoRead> {
    if let Some(reason) = &st.exhausted {
        return batch
            .iter()
            .map(|_| RepoRead::Unavailable(reason.clone()))
            .collect();
    }
    let (doc, vars) = board_query(batch);
    st.queries += 1;
    let failure = match transport.execute(&json!({ "query": doc, "variables": vars })) {
        Ok(resp) if resp.get("data").is_some_and(Value::is_object) => {
            let (reads, rate) = parse_board_response(batch, &resp);
            if let Some(r) = rate {
                if r.remaining < r.cost.max(1) {
                    st.exhausted = Some(format!(
                        "GraphQL rate budget exhausted ({} left){}",
                        r.remaining,
                        r.reset_at
                            .as_deref()
                            .map(|t| format!(", resets at {t}"))
                            .unwrap_or_default()
                    ));
                }
            }
            return reads;
        }
        Ok(resp) => error_text(&resp),
        Err(reason) => reason,
    };
    if batch.len() > 1 {
        let (a, b) = batch.split_at(batch.len() / 2);
        let mut reads = read_chunk(transport, a, st, retries);
        reads.extend(read_chunk(transport, b, st, retries));
        reads
    } else if retries > 0 {
        read_chunk(transport, batch, st, retries - 1)
    } else {
        vec![RepoRead::Unavailable(failure)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const SCHEMA: &str = include_str!("../graphql/github-schema.graphql");

    fn validate(doc: &str) -> Result<(), String> {
        use apollo_compiler::{ExecutableDocument, Schema};
        let schema = Schema::parse_and_validate(SCHEMA, "github-schema.graphql")
            .map_err(|e| format!("schema: {}", e.errors))?;
        ExecutableDocument::parse_and_validate(&schema, doc, "query.graphql")
            .map(|_| ())
            .map_err(|e| e.errors.to_string())
    }

    #[test]
    fn estate_repos_validates() {
        validate(ESTATE_REPOS).unwrap();
    }

    #[test]
    fn board_query_validates_at_every_batch_size() {
        for n in [1, 2, BOARD_BATCH] {
            let batch: Vec<RepoId> = (0..n).map(|i| RepoId::new(format!("o{i}/r{i}"))).collect();
            let (doc, vars) = board_query(&batch);
            validate(&doc).unwrap_or_else(|e| panic!("batch {n}: {e}\n{doc}"));
            assert_eq!(vars.as_object().unwrap().len(), 2 * n);
        }
    }

    #[test]
    fn validator_rejects_a_made_up_field_in_the_board_fragment() {
        let bad = BOARD_REPO.replace("mergeStateStatus", "mergeStateStatusX");
        let (doc, _) = board_query(&[RepoId::new("o/r")]);
        let doc = doc.replace(BOARD_REPO, &bad);
        assert!(validate(&doc).is_err());
    }

    /// Replays canned responses in order and records every request body.
    struct Replay {
        responses: RefCell<Vec<Value>>,
        seen: RefCell<Vec<Value>>,
    }

    impl Replay {
        fn new(rs: Vec<Value>) -> Self {
            Self {
                responses: RefCell::new(rs.into_iter().rev().collect()),
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl GraphQlTransport for Replay {
        fn execute(&self, body: &Value) -> Result<Value, String> {
            self.seen.borrow_mut().push(body.clone());
            self.responses
                .borrow_mut()
                .pop()
                .ok_or_else(|| "no more canned responses".into())
        }
    }

    fn page(total: u64, names: &[(&str, u64)], next: Option<&str>) -> Value {
        json!({ "data": {
            "rateLimit": { "cost": 1, "remaining": 4000, "resetAt": "2026-10-01T15:00:00Z" },
            "repositoryOwner": { "repositories": {
                "totalCount": total,
                "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next },
                "nodes": names.iter().map(|(n, c)| json!({
                    "nameWithOwner": n, "pullRequests": { "totalCount": c }
                })).collect::<Vec<_>>()
            }}
        }})
    }

    #[test]
    fn enumeration_follows_cursors_and_sums_prs() {
        let t = Replay::new(vec![
            page(3, &[("me/a", 2), ("me/b", 0)], Some("C1")),
            page(3, &[("me/c", 5)], None),
        ]);
        let l = enumerate_owner(&t, "me").unwrap();
        assert_eq!(l.repos.len(), 3);
        assert_eq!(l.open_prs(), 7);
        assert_eq!(t.seen.borrow()[1]["variables"]["after"], "C1");
    }

    #[test]
    fn a_transient_listing_failure_is_retried() {
        struct Flaky(RefCell<u32>);
        impl GraphQlTransport for Flaky {
            fn execute(&self, _: &Value) -> Result<Value, String> {
                *self.0.borrow_mut() += 1;
                if *self.0.borrow() == 1 {
                    return Err("gh: HTTP 502".into());
                }
                Ok(page(1, &[("me/a", 1)], None))
            }
        }
        let l = enumerate_owner(&Flaky(RefCell::new(0)), "me").unwrap();
        assert_eq!(l.repos.len(), 1);
    }

    #[test]
    fn enumeration_short_of_total_count_is_an_error() {
        let t = Replay::new(vec![page(5, &[("me/a", 1)], None)]);
        let e = enumerate_owner(&t, "me").unwrap_err();
        assert!(e.contains("reports 5 repositories but 1"), "{e}");
    }

    #[test]
    fn enumeration_without_data_is_an_error_not_an_empty_estate() {
        let t = Replay::new(vec![json!({ "errors": [{ "message": "rate limited" }] })]);
        let e = enumerate_owner(&t, "me").unwrap_err();
        assert!(e.contains("rate limited"), "{e}");
    }

    fn pr_node(n: u64) -> Value {
        json!({
            "number": n, "title": "t", "url": format!("https://github.com/me/a/pull/{n}"),
            "isDraft": false, "mergeable": "MERGEABLE", "mergeStateStatus": "BLOCKED",
            "reviewDecision": null, "updatedAt": "2026-10-01T10:00:00Z",
            "headRefName": "h", "author": { "login": "bot" },
            "autoMergeRequest": null,
            "commits": { "nodes": [ { "commit": { "statusCheckRollup": { "state": "FAILURE" } } } ] }
        })
    }

    #[test]
    fn gate_unions_rulesets_and_classic_protection_without_double_counting() {
        let node = json!({
            "nameWithOwner": "me/a",
            "defaultBranchRef": {
                "name": "main",
                "rules": { "nodes": [
                    { "type": "REQUIRED_STATUS_CHECKS", "parameters": {
                        "requiredStatusChecks": [ { "context": "build" }, { "context": "test" } ] } },
                    { "type": "PULL_REQUEST", "parameters": { "requiredApprovingReviewCount": 1 } },
                    { "type": "DELETION", "parameters": null }
                ]},
                "branchProtectionRule": {
                    "requiresStatusChecks": true,
                    "requiredStatusCheckContexts": ["test", "lint"],
                    "requiresApprovingReviews": true,
                    "requiredApprovingReviewCount": 2
                }
            },
            "pullRequests": { "totalCount": 1, "nodes": [ pr_node(7) ] }
        });
        let RepoRead::Read {
            gate,
            prs,
            truncated,
        } = parse_board_repo(&RepoId::new("me/a"), &node)
        else {
            panic!("expected a read")
        };
        assert_eq!(gate.required_contexts, 3); // build, test, lint
        assert_eq!(gate.required_approvals, 2);
        assert!(!truncated);
        assert_eq!(prs[0].rollup.as_deref(), Some("FAILURE"));
        assert!(!prs[0].auto_merge_armed);
    }

    #[test]
    fn no_rules_and_no_protection_is_a_zero_gate() {
        let node = json!({
            "nameWithOwner": "me/a",
            "defaultBranchRef": { "name": "main", "rules": { "nodes": [] }, "branchProtectionRule": null },
            "pullRequests": { "totalCount": 150, "nodes": [ pr_node(1) ] }
        });
        let RepoRead::Read {
            gate, truncated, ..
        } = parse_board_repo(&RepoId::new("me/a"), &node)
        else {
            panic!("expected a read")
        };
        assert_eq!(gate.required_contexts, 0);
        assert!(truncated, "150 open but 1 returned must be flagged");
    }

    #[test]
    fn a_null_alias_is_unavailable_with_the_error_message() {
        let resp = json!({
            "data": { "rateLimit": { "cost": 1, "remaining": 10, "resetAt": null }, "r0": null },
            "errors": [ { "path": ["r0"], "type": "NOT_FOUND", "message": "Could not resolve" } ]
        });
        let (reads, _) = parse_board_response(&[RepoId::new("me/gone")], &resp);
        assert_eq!(reads[0], RepoRead::Unavailable("Could not resolve".into()));
    }

    /// Fails any query naming `heavy`; fails the first `flaky_first` calls
    /// outright; otherwise answers every alias with an empty repo.
    struct Gateway {
        calls: RefCell<usize>,
        flaky_first: usize,
    }

    impl GraphQlTransport for Gateway {
        fn execute(&self, body: &Value) -> Result<Value, String> {
            *self.calls.borrow_mut() += 1;
            if *self.calls.borrow() <= self.flaky_first {
                return Err("gh: HTTP 502".into());
            }
            let vars = body["variables"].as_object().unwrap();
            if vars.values().any(|v| v == "heavy") {
                return Ok(json!({ "message": "timeout" }));
            }
            let mut data = serde_json::Map::new();
            data.insert(
                "rateLimit".into(),
                json!({ "cost": 1, "remaining": 4000, "resetAt": null }),
            );
            for i in 0..vars.len() / 2 {
                data.insert(
                    format!("r{i}"),
                    json!({ "nameWithOwner": "x", "defaultBranchRef": null,
                    "pullRequests": { "totalCount": 0, "nodes": [] } }),
                );
            }
            Ok(json!({ "data": data }))
        }
    }

    #[test]
    fn one_heavy_repo_does_not_sink_its_batch() {
        let t = Gateway {
            calls: RefCell::new(0),
            flaky_first: 0,
        };
        let repos: Vec<RepoId> = ["me/a", "me/heavy", "me/b", "me/c"]
            .iter()
            .map(|s| RepoId::new(*s))
            .collect();
        let (reads, _) = fetch_board(&t, &repos, 4);
        for i in [0, 2, 3] {
            assert!(
                matches!(reads[i], RepoRead::Read { .. }),
                "repo {i}: {:?}",
                reads[i]
            );
        }
        assert_eq!(reads[1], RepoRead::Unavailable("timeout".into()));
    }

    #[test]
    fn a_transient_gateway_failure_is_recovered() {
        let t = Gateway {
            calls: RefCell::new(0),
            flaky_first: 1,
        };
        let (reads, q) = fetch_board(&t, &[RepoId::new("me/a")], 4);
        assert!(matches!(reads[0], RepoRead::Read { .. }), "{:?}", reads[0]);
        assert_eq!(q, 2);
    }

    #[test]
    fn an_exhausted_budget_marks_the_rest_unavailable() {
        let low = json!({ "data": {
            "rateLimit": { "cost": 5, "remaining": 2, "resetAt": "2026-10-01T15:00:00Z" },
            "r0": { "nameWithOwner": "me/a", "defaultBranchRef": null,
                    "pullRequests": { "totalCount": 0, "nodes": [] } }
        }});
        let t = Replay::new(vec![low]);
        let repos = vec![RepoId::new("me/a"), RepoId::new("me/b")];
        let (reads, q) = fetch_board(&t, &repos, 1);
        assert_eq!(q, 1);
        assert!(matches!(reads[0], RepoRead::Read { .. }));
        assert!(matches!(&reads[1], RepoRead::Unavailable(r) if r.contains("exhausted")));
    }
}
