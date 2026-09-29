// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble-forge` — forge access for the squabbler.
//!
//! GraphQL first for batched reads; REST (still in `squabble-cli::fetch`) where
//! GraphQL has no equivalent (Actions permissions, code-scanning analyses).
//! See `docs/proposals/squabble-modes-and-app-layer.adoc` § Forge access.
//!
//! Transport is the `gh` CLI (`gh api graphql --input -`): the same auth and
//! the same binary `fetch.rs` already relies on, and no HTTP client here. The
//! [`GraphQlTransport`] trait keeps it swappable (octocrab for the App, a
//! recorded-response fake for tests).
//!
//! Every query document is built from `graphql/*.graphql` and validated in
//! tests against `graphql/github-schema.graphql`, a committed snapshot of
//! GitHub's public schema. If GitHub changes the schema, refresh the snapshot
//! and the tests say exactly which query broke.
//!
//! Fail-closed throughout: a repo the API could not read is
//! [`ScanStatus::Unavailable`] with the reason; only an explicit `NOT_FOUND`
//! becomes [`ScanStatus::NotFound`]; an exhausted rate budget marks the
//! remaining repos unavailable rather than guessing.

use serde_json::{json, Value};
use squabble_core::chains::{RepoId, RepoSnapshot, ScanStatus, SourceCost, WorkflowFile};
use std::io::Write;
use std::process::{Command, Stdio};

/// The per-repo fragment, aliased once per repo in [`chains_query`].
pub const WORKFLOW_TREE: &str = include_str!("../graphql/workflow_tree.graphql");

/// Default repos per query. Small enough that one bad repo's payload cannot
/// crowd out the rest; large enough that an estate of hundreds is a handful of
/// round trips.
pub const DEFAULT_BATCH: usize = 20;

/// Something that can POST a GraphQL body and return the JSON response.
pub trait GraphQlTransport {
    /// Return the parsed response body. GraphQL reports per-field errors
    /// *inside* a 200 response, so a body with `errors` is still `Ok`; `Err`
    /// is only for "no usable JSON came back at all".
    fn execute(&self, body: &Value) -> Result<Value, String>;
}

/// `gh api graphql --input -`, body on stdin.
#[derive(Debug, Default, Clone, Copy)]
pub struct GhTransport;

impl GraphQlTransport for GhTransport {
    fn execute(&self, body: &Value) -> Result<Value, String> {
        let mut child = Command::new("gh")
            .args(["api", "graphql", "--input", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to run `gh api graphql`: {e}"))?;
        {
            let stdin = child
                .stdin
                .as_mut()
                .ok_or_else(|| "could not open stdin for `gh`".to_string())?;
            stdin
                .write_all(body.to_string().as_bytes())
                .map_err(|e| format!("could not write query to `gh`: {e}"))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("`gh api graphql` did not finish: {e}"))?;
        // `gh` exits non-zero when the body carries `errors`, but still prints
        // the body — which may hold partial data we must not throw away.
        match serde_json::from_slice::<Value>(&out.stdout) {
            Ok(v) => Ok(v),
            Err(_) => Err(format!(
                "`gh api graphql` exited {} with no JSON body: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    }
}

fn split_slug(repo: &RepoId) -> Option<(&str, &str)> {
    let (o, n) = repo.as_str().split_once('/')?;
    (!o.is_empty() && !n.is_empty() && !n.contains('/')).then_some((o, n))
}

/// Build one batched query for `batch`: the rate-limit probe plus the
/// [`WORKFLOW_TREE`] fragment aliased `r0…rN`, with owner/name passed as
/// variables (never interpolated into the document).
///
/// Every repo in `batch` must have a valid `owner/name` slug; callers filter
/// with [`fetch_workflows`], which reports invalid slugs as unavailable.
pub fn chains_query(batch: &[RepoId]) -> (String, Value) {
    let mut params = Vec::new();
    let mut fields = Vec::new();
    let mut vars = serde_json::Map::new();
    for (i, repo) in batch.iter().enumerate() {
        let (o, n) = split_slug(repo).unwrap_or(("", ""));
        params.push(format!("$o{i}: String!, $n{i}: String!"));
        fields.push(format!(
            "  r{i}: repository(owner: $o{i}, name: $n{i}) {{ ...WorkflowTree }}"
        ));
        vars.insert(format!("o{i}"), json!(o));
        vars.insert(format!("n{i}"), json!(n));
    }
    let doc = format!(
        "query ChainsWorkflows({}) {{\n  rateLimit {{ cost remaining resetAt }}\n{}\n}}\n{}",
        params.join(", "),
        fields.join("\n"),
        WORKFLOW_TREE
    );
    (doc, Value::Object(vars))
}

/// One query's rate-limit sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateSample {
    pub cost: u64,
    pub remaining: u64,
    pub reset_at: Option<String>,
}

/// Turn one batched response into snapshots, in `batch` order.
pub fn parse_chains_response(
    batch: &[RepoId],
    resp: &Value,
) -> (Vec<RepoSnapshot>, Option<RateSample>) {
    let rate = resp.pointer("/data/rateLimit").and_then(|r| {
        Some(RateSample {
            cost: r.get("cost")?.as_u64()?,
            remaining: r.get("remaining")?.as_u64()?,
            reset_at: r.get("resetAt").and_then(Value::as_str).map(str::to_string),
        })
    });

    let errors: Vec<&Value> = resp
        .get("errors")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let error_for = |alias: &str| {
        errors.iter().copied().find(|e| {
            e.get("path")
                .and_then(Value::as_array)
                .and_then(|p| p.first())
                .and_then(Value::as_str)
                == Some(alias)
        })
    };
    let top_level_reason = || {
        let msgs: Vec<&str> = errors
            .iter()
            .filter_map(|e| e.get("message").and_then(Value::as_str))
            .collect();
        if msgs.is_empty() {
            "response carried no data".to_string()
        } else {
            msgs.join("; ")
        }
    };

    let data = resp.get("data").filter(|d| d.is_object());
    let snaps = batch
        .iter()
        .enumerate()
        .map(|(i, repo)| {
            let alias = format!("r{i}");
            let Some(data) = data else {
                return unavailable(repo, top_level_reason());
            };
            match data.get(&alias) {
                Some(node) if node.is_object() => parse_repo(repo, node),
                _ => match error_for(&alias) {
                    Some(e) if e.get("type").and_then(Value::as_str) == Some("NOT_FOUND") => {
                        RepoSnapshot {
                            repo: repo.clone(),
                            status: ScanStatus::NotFound,
                            files: Vec::new(),
                            notes: Vec::new(),
                        }
                    }
                    Some(e) => unavailable(
                        repo,
                        e.get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("GraphQL error without a message")
                            .to_string(),
                    ),
                    None => unavailable(repo, "null repository with no matching error".into()),
                },
            }
        })
        .collect();
    (snaps, rate)
}

fn unavailable(repo: &RepoId, reason: String) -> RepoSnapshot {
    RepoSnapshot {
        repo: repo.clone(),
        status: ScanStatus::Unavailable { reason },
        files: Vec::new(),
        notes: Vec::new(),
    }
}

fn parse_repo(repo: &RepoId, node: &Value) -> RepoSnapshot {
    let mut notes = Vec::new();
    if let Some(canon) = node.get("nameWithOwner").and_then(Value::as_str) {
        if RepoId::new(canon) != *repo {
            notes.push(format!(
                "resolves to `{canon}` (renamed or transferred); references to the old name rely on a redirect"
            ));
        }
    }
    let commit = node.pointer("/defaultBranchRef/target");
    let revision = commit
        .and_then(|c| c.get("oid"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    if node.get("defaultBranchRef").is_none_or(Value::is_null) {
        notes.push("no default branch (empty repository?)".into());
    }

    let mut files = Vec::new();
    let entries = commit
        .and_then(|c| c.pointer("/file/object/entries"))
        .and_then(Value::as_array);
    for entry in entries.into_iter().flatten() {
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let is_blob = entry.get("type").and_then(Value::as_str) == Some("blob");
        if !is_blob || !(name.ends_with(".yml") || name.ends_with(".yaml")) {
            continue;
        }
        let blob = entry.get("object");
        let text = blob.and_then(|b| b.get("text")).and_then(Value::as_str);
        let truncated = blob
            .and_then(|b| b.get("isTruncated"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match text {
            Some(t) => {
                if truncated {
                    notes.push(format!(
                        "{name}: text truncated by the API; references past the cut are missing"
                    ));
                }
                files.push(WorkflowFile {
                    path: name.to_string(),
                    text: t.to_string(),
                });
            }
            None => notes.push(format!(
                "{name}: no text returned (binary or unreadable); skipped"
            )),
        }
    }

    RepoSnapshot {
        repo: repo.clone(),
        status: ScanStatus::Scanned {
            revision,
            workflow_files: files.len(),
        },
        files,
        notes,
    }
}

/// Read the workflow directories of `repos` in batches. Returns one snapshot
/// per requested repo, in order, plus the accumulated query cost.
///
/// Budget: after each query, if the remaining points would not cover another
/// query of the same cost, the rest are marked unavailable with the reset
/// time — never silently dropped, never guessed.
pub fn fetch_workflows(
    transport: &dyn GraphQlTransport,
    repos: &[RepoId],
    batch_size: usize,
) -> (Vec<RepoSnapshot>, SourceCost) {
    let batch_size = batch_size.max(1);
    let mut out: Vec<Option<RepoSnapshot>> = vec![None; repos.len()];
    let mut cost = SourceCost::default();

    let (valid, invalid): (Vec<usize>, Vec<usize>) =
        (0..repos.len()).partition(|&i| split_slug(&repos[i]).is_some());
    for i in invalid {
        out[i] = Some(unavailable(&repos[i], "not an `owner/name` slug".into()));
    }

    let mut exhausted: Option<String> = None;
    for chunk in valid.chunks(batch_size) {
        let batch: Vec<RepoId> = chunk.iter().map(|&i| repos[i].clone()).collect();
        if let Some(reason) = &exhausted {
            for &i in chunk {
                out[i] = Some(unavailable(&repos[i], reason.clone()));
            }
            continue;
        }
        let (doc, vars) = chains_query(&batch);
        let body = json!({ "query": doc, "variables": vars });
        cost.queries += 1;
        match transport.execute(&body) {
            Ok(resp) => {
                let (snaps, rate) = parse_chains_response(&batch, &resp);
                for (&i, s) in chunk.iter().zip(snaps) {
                    out[i] = Some(s);
                }
                if let Some(r) = rate {
                    cost.points_used += r.cost;
                    cost.points_remaining = Some(r.remaining);
                    cost.resets_at = r.reset_at.clone();
                    if r.remaining < r.cost.max(1) {
                        exhausted = Some(format!(
                            "GraphQL rate budget exhausted ({} left){}",
                            r.remaining,
                            r.reset_at
                                .as_deref()
                                .map(|t| format!(", resets at {t}"))
                                .unwrap_or_default()
                        ));
                    }
                }
            }
            Err(reason) => {
                for &i in chunk {
                    out[i] = Some(unavailable(&repos[i], reason.clone()));
                }
            }
        }
    }

    let snaps = out
        .into_iter()
        .zip(repos)
        .map(|(s, r)| s.unwrap_or_else(|| unavailable(r, "not fetched".into())))
        .collect();
    (snaps, cost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const SCHEMA: &str = include_str!("../graphql/github-schema.graphql");

    fn repos(slugs: &[&str]) -> Vec<RepoId> {
        slugs.iter().map(|s| RepoId::new(*s)).collect()
    }

    // --- schema validation -------------------------------------------------

    fn validate(doc: &str) -> Result<(), String> {
        use apollo_compiler::{ExecutableDocument, Schema};
        let schema = Schema::parse_and_validate(SCHEMA, "github-schema.graphql")
            .map_err(|e| format!("schema: {}", e.errors))?;
        ExecutableDocument::parse_and_validate(&schema, doc, "query.graphql")
            .map(|_| ())
            .map_err(|e| e.errors.to_string())
    }

    #[test]
    fn generated_query_validates_against_the_schema_snapshot() {
        for n in [1, 2, DEFAULT_BATCH] {
            let batch: Vec<RepoId> = (0..n).map(|i| RepoId::new(format!("o{i}/r{i}"))).collect();
            let (doc, vars) = chains_query(&batch);
            validate(&doc).unwrap_or_else(|e| panic!("batch {n}: {e}\n{doc}"));
            assert_eq!(vars.as_object().unwrap().len(), 2 * n);
        }
    }

    #[test]
    fn validator_actually_rejects_a_field_the_schema_lacks() {
        // Guard against a vacuous validator: a made-up field must fail.
        let bad = "query Q { rateLimit { cost notARealField } }";
        assert!(validate(bad).is_err());
    }

    #[test]
    fn owner_and_name_travel_as_variables_not_document_text() {
        let (doc, vars) = chains_query(&repos(&["evil\"){x}/repo"]));
        assert!(!doc.contains("evil"));
        assert_eq!(vars["o0"], "evil\"){x}");
    }

    // --- response parsing (synthetic responses shaped per the schema) ------

    fn tree(entries: Value) -> Value {
        json!({
            "nameWithOwner": "me/app",
            "defaultBranchRef": { "target": {
                "oid": "ABCDEF0123456789ABCDEF0123456789ABCDEF01",
                "file": { "object": { "entries": entries } }
            }}
        })
    }

    #[test]
    fn parses_files_revision_and_rate() {
        let resp = json!({ "data": {
            "rateLimit": { "cost": 1, "remaining": 4990, "resetAt": "2026-09-29T23:00:00Z" },
            "r0": tree(json!([
                { "name": "ci.yml", "type": "blob",
                  "object": { "text": "jobs: {}\n", "isBinary": false, "isTruncated": false } },
                { "name": "README.adoc", "type": "blob",
                  "object": { "text": "x", "isBinary": false, "isTruncated": false } },
                { "name": "nested", "type": "tree", "object": {} }
            ]))
        }});
        let (snaps, rate) = parse_chains_response(&repos(&["me/app"]), &resp);
        let s = &snaps[0];
        assert_eq!(s.files.len(), 1);
        assert_eq!(
            s.status,
            ScanStatus::Scanned {
                revision: Some("abcdef0123456789abcdef0123456789abcdef01".into()),
                workflow_files: 1
            }
        );
        assert_eq!(rate.unwrap().remaining, 4990);
    }

    #[test]
    fn truncated_and_binary_blobs_are_said_aloud() {
        let resp = json!({ "data": { "r0": tree(json!([
            { "name": "big.yml", "type": "blob",
              "object": { "text": "jobs:\n", "isBinary": false, "isTruncated": true } },
            { "name": "odd.yml", "type": "blob",
              "object": { "text": null, "isBinary": true, "isTruncated": false } }
        ]))}});
        let (snaps, _) = parse_chains_response(&repos(&["me/app"]), &resp);
        let notes = snaps[0].notes.join("\n");
        assert!(notes.contains("big.yml: text truncated"));
        assert!(notes.contains("odd.yml: no text returned"));
        assert_eq!(snaps[0].files.len(), 1);
    }

    #[test]
    fn not_found_is_distinguished_from_other_errors() {
        let resp = json!({
            "data": { "r0": null, "r1": null, "r2": tree(json!([])) },
            "errors": [
                { "type": "NOT_FOUND", "path": ["r0"], "message": "Could not resolve to a Repository" },
                { "type": "FORBIDDEN", "path": ["r1"], "message": "Resource not accessible" }
            ]
        });
        let (s, _) = parse_chains_response(&repos(&["a/gone", "b/private", "me/app"]), &resp);
        assert_eq!(s[0].status, ScanStatus::NotFound);
        assert!(
            matches!(&s[1].status, ScanStatus::Unavailable { reason } if reason.contains("not accessible"))
        );
        assert!(matches!(s[2].status, ScanStatus::Scanned { .. }));
    }

    #[test]
    fn a_response_without_data_marks_everything_unavailable() {
        let resp = json!({ "errors": [{ "message": "Bad credentials" }] });
        let (s, rate) = parse_chains_response(&repos(&["a/b", "c/d"]), &resp);
        assert!(rate.is_none());
        assert!(s.iter().all(|x| matches!(
            &x.status,
            ScanStatus::Unavailable { reason } if reason.contains("Bad credentials")
        )));
    }

    #[test]
    fn missing_workflow_dir_and_empty_repo_are_scanned_with_zero_files() {
        let resp = json!({ "data": {
            "r0": { "nameWithOwner": "me/nowf",
                    "defaultBranchRef": { "target": { "oid": "0000000000000000000000000000000000000000", "file": null } } },
            "r1": { "nameWithOwner": "me/empty", "defaultBranchRef": null }
        }});
        let (s, _) = parse_chains_response(&repos(&["me/nowf", "me/empty"]), &resp);
        assert!(matches!(
            s[0].status,
            ScanStatus::Scanned {
                workflow_files: 0,
                ..
            }
        ));
        assert!(matches!(
            s[1].status,
            ScanStatus::Scanned {
                revision: None,
                workflow_files: 0
            }
        ));
        assert!(s[1].notes.iter().any(|n| n.contains("no default branch")));
    }

    #[test]
    fn a_rename_is_noted_but_identity_stays_as_requested() {
        let mut node = tree(json!([]));
        node["nameWithOwner"] = json!("me/new-name");
        let resp = json!({ "data": { "r0": node } });
        let (s, _) = parse_chains_response(&repos(&["me/old-name"]), &resp);
        assert_eq!(s[0].repo, RepoId::new("me/old-name"));
        assert!(s[0].notes[0].contains("renamed"));
    }

    // --- batching and budget ------------------------------------------------

    struct Fake {
        remaining: RefCell<u64>,
        calls: RefCell<u32>,
    }
    impl GraphQlTransport for Fake {
        fn execute(&self, body: &Value) -> Result<Value, String> {
            *self.calls.borrow_mut() += 1;
            let vars = body["variables"].as_object().unwrap();
            let mut data = serde_json::Map::new();
            for i in 0..vars.len() / 2 {
                let slug = format!(
                    "{}/{}",
                    vars[&format!("o{i}")].as_str().unwrap(),
                    vars[&format!("n{i}")].as_str().unwrap()
                );
                let mut t = tree(json!([]));
                t["nameWithOwner"] = json!(slug);
                data.insert(format!("r{i}"), t);
            }
            let mut rem = self.remaining.borrow_mut();
            *rem = rem.saturating_sub(1);
            data.insert(
                "rateLimit".into(),
                json!({ "cost": 1, "remaining": *rem, "resetAt": "T" }),
            );
            Ok(json!({ "data": data }))
        }
    }

    #[test]
    fn batches_preserve_order_and_count_queries() {
        let fake = Fake {
            remaining: RefCell::new(100),
            calls: RefCell::new(0),
        };
        let rs = repos(&["a/1", "a/2", "a/3", "a/4", "a/5"]);
        let (snaps, cost) = fetch_workflows(&fake, &rs, 2);
        assert_eq!(*fake.calls.borrow(), 3);
        assert_eq!(cost.queries, 3);
        assert_eq!(cost.points_used, 3);
        assert_eq!(snaps.iter().map(|s| s.repo.clone()).collect::<Vec<_>>(), rs);
        assert!(snaps.iter().all(|s| s.notes.is_empty()));
    }

    #[test]
    fn an_exhausted_budget_stops_and_says_why() {
        let fake = Fake {
            remaining: RefCell::new(1),
            calls: RefCell::new(0),
        };
        let rs = repos(&["a/1", "a/2", "a/3"]);
        let (snaps, cost) = fetch_workflows(&fake, &rs, 1);
        assert_eq!(
            *fake.calls.borrow(),
            1,
            "must stop after the budget runs out"
        );
        assert_eq!(cost.points_remaining, Some(0));
        assert!(matches!(snaps[0].status, ScanStatus::Scanned { .. }));
        for s in &snaps[1..] {
            assert!(
                matches!(&s.status, ScanStatus::Unavailable { reason } if reason.contains("exhausted") && reason.contains("resets at T"))
            );
        }
    }

    #[test]
    fn invalid_slugs_and_transport_failures_are_unavailable() {
        struct Down;
        impl GraphQlTransport for Down {
            fn execute(&self, _: &Value) -> Result<Value, String> {
                Err("no network".into())
            }
        }
        let (snaps, _) = fetch_workflows(&Down, &repos(&["not-a-slug", "a/b"]), 5);
        assert!(
            matches!(&snaps[0].status, ScanStatus::Unavailable { reason } if reason.contains("slug"))
        );
        assert!(
            matches!(&snaps[1].status, ScanStatus::Unavailable { reason } if reason == "no network")
        );
    }
}
