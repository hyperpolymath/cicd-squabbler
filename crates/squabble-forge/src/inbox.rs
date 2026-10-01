// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Forge reads for `squabble inbox-sweep`: the state of every issue or pull
//! request a notification thread points at, batched through GraphQL.
//!
//! Each alias carries its own `number`, so the per-subject selection is
//! generated inline rather than from a shared fragment; it is still
//! schema-validated in tests. A subject that could not be read comes back
//! [`SubjectState::Unknown`] with the reason, and the sweep keeps its thread.

use crate::{split_slug, GraphQlTransport};
use serde_json::{json, Value};
use squabble_core::inbox::{SubjectRef, SubjectState};
use std::collections::BTreeMap;

/// Subjects per state query.
pub const STATE_BATCH: usize = 50;

/// Build one batched state query for `batch`, aliased `s0…sN`.
pub fn state_query(batch: &[SubjectRef]) -> (String, Value) {
    let mut params = Vec::new();
    let mut fields = Vec::new();
    let mut vars = serde_json::Map::new();
    for (i, s) in batch.iter().enumerate() {
        let (o, n) = split_slug(&s.repo).unwrap_or(("", ""));
        params.push(format!("$o{i}: String!, $n{i}: String!, $k{i}: Int!"));
        fields.push(format!(
            "  s{i}: repository(owner: $o{i}, name: $n{i}) {{ issueOrPullRequest(number: $k{i}) {{ __typename ... on PullRequest {{ prState: state }} ... on Issue {{ issueState: state }} }} }}"
        ));
        vars.insert(format!("o{i}"), json!(o));
        vars.insert(format!("n{i}"), json!(n));
        vars.insert(format!("k{i}"), json!(s.number));
    }
    let doc = format!(
        "query SubjectStates({}) {{\n  rateLimit {{ cost remaining resetAt }}\n{}\n}}\n",
        params.join(", "),
        fields.join("\n")
    );
    (doc, Value::Object(vars))
}

/// Read one alias of a state response.
fn state_of(resp: &Value, alias: &str) -> SubjectState {
    let node = resp.pointer(&format!("/data/{alias}/issueOrPullRequest"));
    let state = node.and_then(|n| n.get("prState").or_else(|| n.get("issueState")));
    match state.and_then(Value::as_str) {
        Some("OPEN") => SubjectState::Open,
        Some("MERGED") => SubjectState::Merged,
        Some("CLOSED") => SubjectState::Closed,
        Some(other) => SubjectState::Unknown(format!("unrecognised state `{other}`")),
        None => {
            let msg = resp
                .get("errors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|e| e.pointer("/path/0").and_then(Value::as_str) == Some(alias))
                .and_then(|e| e.get("message").and_then(Value::as_str))
                .map(str::to_string);
            SubjectState::Unknown(msg.unwrap_or_else(|| {
                if resp.get("data").is_some_and(Value::is_object) {
                    "subject not found".into()
                } else {
                    "response carried no data".into()
                }
            }))
        }
    }
}

/// Look up the state of every subject. Duplicates are read once. Returns the
/// states and the number of queries spent.
pub fn fetch_states(
    transport: &dyn GraphQlTransport,
    subjects: &[SubjectRef],
    batch_size: usize,
) -> (BTreeMap<SubjectRef, SubjectState>, usize) {
    let mut unique: Vec<SubjectRef> = subjects.to_vec();
    unique.sort();
    unique.dedup();
    let mut out = BTreeMap::new();
    let (valid, invalid): (Vec<SubjectRef>, Vec<SubjectRef>) = unique
        .into_iter()
        .partition(|s| split_slug(&s.repo).is_some());
    for s in invalid {
        out.insert(s, SubjectState::Unknown("not an `owner/name` slug".into()));
    }
    let mut queries = 0;
    let mut exhausted: Option<String> = None;
    for chunk in valid.chunks(batch_size.max(1)) {
        if let Some(r) = &exhausted {
            for s in chunk {
                out.insert(s.clone(), SubjectState::Unknown(r.clone()));
            }
            continue;
        }
        let (doc, vars) = state_query(chunk);
        queries += 1;
        match transport.execute(&json!({ "query": doc, "variables": vars })) {
            Ok(resp) => {
                for (i, s) in chunk.iter().enumerate() {
                    out.insert(s.clone(), state_of(&resp, &format!("s{i}")));
                }
                let rem = resp
                    .pointer("/data/rateLimit/remaining")
                    .and_then(Value::as_u64);
                let cost = resp.pointer("/data/rateLimit/cost").and_then(Value::as_u64);
                if let (Some(rem), Some(cost)) = (rem, cost) {
                    if rem < cost.max(1) {
                        exhausted = Some(format!("GraphQL rate budget exhausted ({rem} left)"));
                    }
                }
            }
            Err(e) => {
                for s in chunk {
                    out.insert(s.clone(), SubjectState::Unknown(e.clone()));
                }
            }
        }
    }
    (out, queries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use squabble_core::chains::RepoId;

    const SCHEMA: &str = include_str!("../graphql/github-schema.graphql");

    fn validate(doc: &str) -> Result<(), String> {
        use apollo_compiler::{ExecutableDocument, Schema};
        let schema = Schema::parse_and_validate(SCHEMA, "github-schema.graphql")
            .map_err(|e| format!("schema: {}", e.errors))?;
        ExecutableDocument::parse_and_validate(&schema, doc, "query.graphql")
            .map(|_| ())
            .map_err(|e| e.errors.to_string())
    }

    fn subj(r: &str, n: u64) -> SubjectRef {
        SubjectRef {
            repo: RepoId::new(r),
            number: n,
        }
    }

    #[test]
    fn state_query_validates_at_every_batch_size() {
        for n in [1, 3, STATE_BATCH] {
            let batch: Vec<SubjectRef> = (0..n)
                .map(|i| subj(&format!("o/r{i}"), i as u64 + 1))
                .collect();
            let (doc, vars) = state_query(&batch);
            validate(&doc).unwrap_or_else(|e| panic!("batch {n}: {e}"));
            assert_eq!(vars.as_object().unwrap().len(), 3 * n);
        }
    }

    #[test]
    fn validator_rejects_a_wrong_field_in_the_generated_query() {
        let (doc, _) = state_query(&[subj("o/r", 1)]);
        assert!(validate(&doc.replace("prState: state", "prState: stateX")).is_err());
    }

    #[test]
    fn states_parse_and_failures_stay_unknown() {
        let resp = json!({
            "data": {
                "s0": { "issueOrPullRequest": { "__typename": "PullRequest", "prState": "MERGED" } },
                "s1": { "issueOrPullRequest": { "__typename": "Issue", "issueState": "CLOSED" } },
                "s2": { "issueOrPullRequest": { "__typename": "PullRequest", "prState": "OPEN" } },
                "s3": null,
                "s4": { "issueOrPullRequest": null }
            },
            "errors": [ { "path": ["s3"], "message": "Could not resolve to a Repository" } ]
        });
        assert_eq!(state_of(&resp, "s0"), SubjectState::Merged);
        assert_eq!(state_of(&resp, "s1"), SubjectState::Closed);
        assert_eq!(state_of(&resp, "s2"), SubjectState::Open);
        assert_eq!(
            state_of(&resp, "s3"),
            SubjectState::Unknown("Could not resolve to a Repository".into())
        );
        assert_eq!(
            state_of(&resp, "s4"),
            SubjectState::Unknown("subject not found".into())
        );
        assert_eq!(
            state_of(&json!({ "message": "502" }), "s0"),
            SubjectState::Unknown("response carried no data".into())
        );
    }

    #[test]
    fn duplicates_are_read_once_and_transport_errors_are_unknown() {
        struct Down;
        impl GraphQlTransport for Down {
            fn execute(&self, _: &Value) -> Result<Value, String> {
                Err("gh: HTTP 502".into())
            }
        }
        let (m, q) = fetch_states(&Down, &[subj("o/r", 1), subj("o/r", 1), subj("o/r", 2)], 50);
        assert_eq!(q, 1);
        assert_eq!(m.len(), 2);
        assert!(m
            .values()
            .all(|s| matches!(s, SubjectState::Unknown(r) if r.contains("502"))));
    }
}
