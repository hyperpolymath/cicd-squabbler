// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble inbox-sweep` — clear notification threads whose pull request or
//! issue is already merged or closed, so the inbox holds only live items.
//!
//! Lists every thread still in the inbox (`GET /notifications?all=true`,
//! paginated), resolves each subject's state in GraphQL batches, and decides
//! per thread with `squabble_core::inbox::decide`. Only merged or closed
//! subjects are cleared; anything unread, unknown or of another kind stays.
//!
//! Dry run by default: prints what it *would* clear. `--apply` writes, per
//! thread, `DELETE /notifications/threads/{id}/subscription` (unsubscribe)
//! and then `DELETE /notifications/threads/{id}` (mark done). A thread whose
//! unsubscribe fails is not marked done. Writes stop while the REST budget
//! is at or below `--reserve`; every thread that was due to be cleared but
//! was not is listed by id — the tail is a set, never just a count.
//!
//! `all=true` also lists threads already marked done — REST exposes no done
//! state — so without memory every run would re-clear the same threads.
//! `--state <path>` records `(thread id, updated_at)` for each thread the
//! sweep cleared; a thread whose `updated_at` is unchanged is skipped, and one
//! with new activity (which GitHub returns to the inbox) is decided afresh.
//!
//! This needs the owner's user token (`notifications` or `repo` scope); an
//! App token cannot read notifications.
//!
//! Exit `0` = every due thread cleared (or, dry run, listed); `6` = some due
//! threads were left (budget, `--limit`, or a failed write); `2` = usage or
//! tool failure.

use crate::board::INCOMPLETE_EXIT;
use serde_json::Value;
use squabble_core::chains::RepoId;
use squabble_core::inbox::{decide, subject_of, tally, SubjectRef, Thread, Verdict};
use squabble_forge::inbox::{fetch_states, STATE_BATCH};
use squabble_forge::{GhTransport, GraphQlTransport};
use std::collections::{BTreeMap, BTreeSet};
use std::process::{Command, ExitCode};

pub(crate) const USAGE: &str = "usage: squabble inbox-sweep [--owners a,b] [--repo owner/name] [--apply] [--limit N] [--reserve N] [--state path]";

/// Thread id → `updated_at` at the moment the sweep cleared it.
pub(crate) type Cleared = BTreeMap<String, String>;

/// Owners swept when `--owners` is not given.
const DEFAULT_OWNERS: &[&str] = &["hyperpolymath", "metadatastician"];

/// REST requests kept back for everything else on this token.
const DEFAULT_RESERVE: u64 = 1000;

struct Args {
    owners: Vec<String>,
    repo: Option<String>,
    apply: bool,
    limit: Option<usize>,
    reserve: u64,
    state: Option<std::path::PathBuf>,
}

/// Parse the subcommand's flags.
fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut a = Args {
        owners: DEFAULT_OWNERS.iter().map(|s| s.to_string()).collect(),
        repo: None,
        apply: false,
        limit: None,
        reserve: DEFAULT_RESERVE,
        state: None,
    };
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))
        };
        match flag.as_str() {
            "--owners" => {
                a.owners = value()?
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            }
            "--repo" => {
                let r = value()?;
                if r.split('/').count() != 2 || r.starts_with('/') || r.ends_with('/') {
                    return Err(format!("--repo wants owner/name, got `{r}`"));
                }
                a.repo = Some(r.clone());
            }
            "--apply" => a.apply = true,
            "--limit" => {
                a.limit = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--limit wants a number".to_string())?,
                )
            }
            "--reserve" => {
                a.reserve = value()?
                    .parse()
                    .map_err(|_| "--reserve wants a number".to_string())?
            }
            "--state" => a.state = Some(value()?.into()),
            other => return Err(format!("unknown flag `{other}`\n{USAGE}")),
        }
    }
    if a.owners.is_empty() {
        return Err("--owners is empty".into());
    }
    Ok(a)
}

/// The notification endpoints the sweep uses. A write returns the REST
/// budget left after it, when the response said.
pub(crate) trait NotificationApi {
    /// Every thread still in the inbox, read or unread.
    fn list(&self) -> Result<Vec<Thread>, String>;
    /// Unsubscribe from a thread.
    fn unsubscribe(&self, id: &str) -> Result<Option<u64>, String>;
    /// Mark a thread done (removes it from the inbox).
    fn mark_done(&self, id: &str) -> Result<Option<u64>, String>;
}

/// `gh api` with the owner's token.
struct GhNotifications;

/// Run `gh api -i <args>` and return the status line, headers and body.
fn gh_with_headers(args: &[&str]) -> Result<(u16, BTreeMap<String, String>, String), String> {
    let out = Command::new("gh")
        .arg("api")
        .arg("-i")
        .args(args)
        .output()
        .map_err(|e| format!("failed to run `gh api`: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    let (head, body) = text.split_once("\n\n").unwrap_or((text.as_str(), ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| {
            format!(
                "`gh api {}` gave no HTTP status: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )
        })?;
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Ok((status, headers, body.to_string()))
}

/// Issue one DELETE and insist on `204 No Content`.
fn delete_expect_204(path: &str) -> Result<Option<u64>, String> {
    let (status, headers, body) = gh_with_headers(&["-X", "DELETE", path])?;
    if status != 204 {
        return Err(format!("DELETE {path} → HTTP {status}: {}", body.trim()));
    }
    Ok(headers
        .get("x-ratelimit-remaining")
        .and_then(|v| v.parse().ok()))
}

/// Turn one REST notification object into a [`Thread`].
fn thread_from_json(v: &Value) -> Option<Thread> {
    let s = |p: &str| v.pointer(p).and_then(Value::as_str).map(str::to_string);
    Some(Thread {
        id: s("/id")?,
        reason: s("/reason").unwrap_or_default(),
        subject_type: s("/subject/type").unwrap_or_default(),
        repo: RepoId::new(s("/repository/full_name")?),
        subject_url: s("/subject/url"),
        unread: v.get("unread").and_then(Value::as_bool).unwrap_or(false),
        updated_at: s("/updated_at").unwrap_or_default(),
    })
}

impl NotificationApi for GhNotifications {
    fn list(&self) -> Result<Vec<Thread>, String> {
        let out = Command::new("gh")
            .args([
                "api",
                "--paginate",
                "--slurp",
                "notifications?all=true&per_page=50",
            ])
            .output()
            .map_err(|e| format!("failed to run `gh api`: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "listing notifications failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let pages: Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("notification listing was not JSON: {e}"))?;
        let mut threads = Vec::new();
        for page in pages
            .as_array()
            .ok_or("notification listing was not an array of pages")?
        {
            for n in page
                .as_array()
                .ok_or("a notification page was not an array")?
            {
                threads.push(
                    thread_from_json(n).ok_or_else(|| format!("malformed notification: {n}"))?,
                );
            }
        }
        Ok(threads)
    }

    fn unsubscribe(&self, id: &str) -> Result<Option<u64>, String> {
        delete_expect_204(&format!("notifications/threads/{id}/subscription"))
    }

    fn mark_done(&self, id: &str) -> Result<Option<u64>, String> {
        delete_expect_204(&format!("notifications/threads/{id}"))
    }
}

/// What a sweep found and did.
#[derive(Debug, Default)]
pub(crate) struct Sweep {
    pub listed: usize,
    pub out_of_scope: usize,
    pub verdicts: BTreeMap<Verdict, usize>,
    /// Threads due to be cleared.
    pub due: BTreeSet<String>,
    pub cleared: BTreeSet<String>,
    /// Due threads not cleared, with why.
    pub left: BTreeMap<String, String>,
    pub graphql_queries: usize,
    /// In-scope threads skipped because an earlier sweep cleared them and
    /// they have had no activity since.
    pub already_cleared: usize,
    /// The state to persist: earlier entries still listed, plus this run's
    /// clears. Entries for threads no longer listed are dropped.
    pub state: Cleared,
}

/// List, decide and (with `apply`) clear. Pure apart from the two APIs.
/// `prior` is the state an earlier sweep left; threads it records with the
/// same `updated_at` are skipped without a read or a write.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sweep(
    api: &dyn NotificationApi,
    graphql: &dyn GraphQlTransport,
    owners: &[String],
    repo: Option<&str>,
    apply: bool,
    limit: Option<usize>,
    reserve: u64,
    prior: &Cleared,
) -> Result<Sweep, String> {
    let all = api.list()?;
    let mut out = Sweep {
        listed: all.len(),
        ..Sweep::default()
    };
    // Keep only entries whose thread is still listed, so the file stays bounded.
    out.state = all
        .iter()
        .filter_map(|t| prior.get(&t.id).map(|u| (t.id.clone(), u.clone())))
        .collect();
    let in_scope: Vec<Thread> = all
        .into_iter()
        .filter(|t| {
            let owner = t.repo.as_str().split('/').next().unwrap_or("");
            owners.iter().any(|o| o == owner) && repo.is_none_or(|r| t.repo.as_str() == r)
        })
        .collect();
    out.out_of_scope = out.listed - in_scope.len();
    let in_scope: Vec<Thread> = in_scope
        .into_iter()
        .filter(|t| {
            let skip = prior.get(&t.id) == Some(&t.updated_at);
            out.already_cleared += usize::from(skip);
            !skip
        })
        .collect();

    let subjects: Vec<SubjectRef> = in_scope.iter().filter_map(subject_of).collect();
    let (states, queries) = fetch_states(graphql, &subjects, STATE_BATCH);
    out.graphql_queries = queries;

    let decided: Vec<(&Thread, Verdict)> = in_scope
        .iter()
        .map(|t| (t, decide(t, subject_of(t).and_then(|s| states.get(&s)))))
        .collect();
    out.verdicts = tally(&decided.iter().map(|(_, v)| *v).collect::<Vec<_>>());
    let due: Vec<&Thread> = decided
        .iter()
        .filter(|(_, v)| v.clears())
        .map(|(t, _)| *t)
        .collect();
    out.due = due.iter().map(|t| t.id.clone()).collect();

    if !apply {
        return Ok(out);
    }
    let mut budget: Option<u64> = None;
    for t in due {
        if limit.is_some_and(|l| out.cleared.len() >= l) {
            out.left.insert(t.id.clone(), "--limit reached".into());
            continue;
        }
        if let Some(b) = budget.filter(|b| *b <= reserve) {
            out.left
                .insert(t.id.clone(), format!("REST budget at reserve ({b} left)"));
            continue;
        }
        match api.unsubscribe(&t.id) {
            Ok(b) => budget = b.or(budget),
            Err(e) => {
                out.left
                    .insert(t.id.clone(), format!("unsubscribe failed: {e}"));
                continue;
            }
        }
        match api.mark_done(&t.id) {
            Ok(b) => {
                budget = b.or(budget);
                out.cleared.insert(t.id.clone());
                out.state.insert(t.id.clone(), t.updated_at.clone());
            }
            Err(e) => {
                out.left
                    .insert(t.id.clone(), format!("unsubscribed, mark-done failed: {e}"));
            }
        }
    }
    Ok(out)
}

/// Read a state file; a missing file is an empty state, anything else
/// unreadable is an error (a silently empty state would re-clear everything).
fn load_state(path: &std::path::Path) -> Result<Cleared, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("state file {} is not valid: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Cleared::new()),
        Err(e) => Err(format!("cannot read state file {}: {e}", path.display())),
    }
}

/// Write the state file atomically: a sibling temp file, then rename.
fn save_state(path: &std::path::Path, state: &Cleared) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let body = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| format!("cannot write state file {}: {e}", path.display()))
}

/// Entry point for `squabble inbox-sweep <flags>`.
pub(crate) fn run(rest: &[String]) -> ExitCode {
    let args = match parse_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("squabble inbox-sweep: {e}");
            return ExitCode::from(2);
        }
    };
    let prior = match args.state.as_deref().map(load_state).transpose() {
        Ok(p) => p.unwrap_or_default(),
        Err(e) => {
            eprintln!("squabble inbox-sweep: {e}");
            return ExitCode::from(2);
        }
    };
    let s = match sweep(
        &GhNotifications,
        &GhTransport,
        &args.owners,
        args.repo.as_deref(),
        args.apply,
        args.limit,
        args.reserve,
        &prior,
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("squabble inbox-sweep: {e}");
            return ExitCode::from(2);
        }
    };
    println!(
        "inbox: {} threads listed, {} out of scope, {} state queries",
        s.listed, s.out_of_scope, s.graphql_queries
    );
    if s.already_cleared > 0 {
        println!(
            "  {:>5}  skip: cleared by an earlier sweep, no activity since",
            s.already_cleared
        );
    }
    for (v, n) in &s.verdicts {
        println!("  {n:>5}  {}", v.label());
    }
    if !args.apply {
        println!(
            "dry run: {} thread(s) would be cleared; pass --apply to write",
            s.due.len()
        );
        return ExitCode::SUCCESS;
    }
    println!("cleared {} of {} due", s.cleared.len(), s.due.len());
    if let Some(path) = &args.state {
        if let Err(e) = save_state(path, &s.state) {
            eprintln!("squabble inbox-sweep: {e}");
            return ExitCode::from(2);
        }
    }
    // The tail is the set difference, computed rather than assumed.
    let unaccounted: Vec<&String> = s
        .due
        .iter()
        .filter(|id| !s.cleared.contains(*id) && !s.left.contains_key(*id))
        .collect();
    for (id, why) in &s.left {
        println!("  left {id}: {why}");
    }
    for id in &unaccounted {
        println!("  left {id}: not attempted (unaccounted)");
    }
    if s.left.is_empty() && unaccounted.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(INCOMPLETE_EXIT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    fn t(id: &str, repo: &str, kind: &str, n: u64) -> Thread {
        Thread {
            id: id.into(),
            reason: "author".into(),
            subject_type: kind.into(),
            repo: RepoId::new(repo),
            subject_url: Some(format!("https://api.github.com/repos/{repo}/pulls/{n}")),
            unread: false,
            updated_at: "2026-10-01T00:00:00Z".into(),
        }
    }

    struct FakeApi {
        threads: Vec<Thread>,
        writes: RefCell<Vec<String>>,
        fail_unsub: Option<String>,
        budget: RefCell<u64>,
    }
    impl NotificationApi for FakeApi {
        fn list(&self) -> Result<Vec<Thread>, String> {
            Ok(self.threads.clone())
        }
        fn unsubscribe(&self, id: &str) -> Result<Option<u64>, String> {
            if self.fail_unsub.as_deref() == Some(id) {
                return Err("HTTP 500".into());
            }
            self.writes.borrow_mut().push(format!("unsub {id}"));
            *self.budget.borrow_mut() -= 1;
            Ok(Some(*self.budget.borrow()))
        }
        fn mark_done(&self, id: &str) -> Result<Option<u64>, String> {
            self.writes.borrow_mut().push(format!("done {id}"));
            *self.budget.borrow_mut() -= 1;
            Ok(Some(*self.budget.borrow()))
        }
    }

    /// Answers every alias from a number → state table.
    struct States(BTreeMap<u64, &'static str>);
    impl GraphQlTransport for States {
        fn execute(&self, body: &Value) -> Result<Value, String> {
            let vars = body["variables"].as_object().unwrap();
            let mut data = serde_json::Map::new();
            for i in 0.. {
                let Some(k) = vars.get(&format!("k{i}")).and_then(Value::as_u64) else {
                    break;
                };
                let node = self
                    .0
                    .get(&k)
                    .map(|s| json!({ "__typename": "PullRequest", "prState": s }));
                data.insert(format!("s{i}"), json!({ "issueOrPullRequest": node }));
            }
            data.insert("rateLimit".into(), json!({ "cost": 1, "remaining": 4000 }));
            Ok(json!({ "data": data }))
        }
    }

    fn fixture(fail_unsub: Option<&str>, budget: u64) -> (FakeApi, States) {
        let api = FakeApi {
            threads: vec![
                t("1", "hyperpolymath/a", "PullRequest", 1),
                t("2", "hyperpolymath/a", "PullRequest", 2),
                t("3", "hyperpolymath/b", "PullRequest", 3),
                t("4", "hyperpolymath/b", "PullRequest", 4),
                t("5", "stranger/c", "PullRequest", 5),
                t("6", "hyperpolymath/b", "Release", 6),
            ],
            writes: RefCell::new(vec![]),
            fail_unsub: fail_unsub.map(str::to_string),
            budget: RefCell::new(budget),
        };
        // 1 merged, 2 open, 3 closed, 4 unreadable, 5 merged but out of scope.
        let states = States(BTreeMap::from([
            (1, "MERGED"),
            (2, "OPEN"),
            (3, "CLOSED"),
            (5, "MERGED"),
        ]));
        (api, states)
    }

    fn owners() -> Vec<String> {
        vec!["hyperpolymath".into()]
    }

    #[test]
    fn dry_run_writes_nothing_and_lists_only_resolved_in_scope_threads() {
        let (api, gql) = fixture(None, 5000);
        let s = sweep(
            &api,
            &gql,
            &owners(),
            None,
            false,
            None,
            1000,
            &Cleared::new(),
        )
        .unwrap();
        assert!(api.writes.borrow().is_empty());
        assert_eq!(s.listed, 6);
        assert_eq!(s.out_of_scope, 1);
        assert_eq!(s.due, BTreeSet::from(["1".to_string(), "3".to_string()]));
        assert_eq!(s.verdicts[&Verdict::KeepOpen], 1);
        assert_eq!(s.verdicts[&Verdict::KeepUnknownState], 1);
        assert_eq!(s.verdicts[&Verdict::KeepUnsupportedSubject], 1);
    }

    #[test]
    fn apply_unsubscribes_before_marking_done_and_never_touches_kept_threads() {
        let (api, gql) = fixture(None, 5000);
        let s = sweep(
            &api,
            &gql,
            &owners(),
            None,
            true,
            None,
            1000,
            &Cleared::new(),
        )
        .unwrap();
        assert_eq!(
            *api.writes.borrow(),
            ["unsub 1", "done 1", "unsub 3", "done 3"]
        );
        assert_eq!(s.cleared, s.due);
        assert!(s.left.is_empty());
    }

    #[test]
    fn a_failed_unsubscribe_leaves_the_thread_in_the_inbox() {
        let (api, gql) = fixture(Some("1"), 5000);
        let s = sweep(
            &api,
            &gql,
            &owners(),
            None,
            true,
            None,
            1000,
            &Cleared::new(),
        )
        .unwrap();
        assert!(!api.writes.borrow().iter().any(|w| w == "done 1"));
        assert!(s.left["1"].contains("unsubscribe failed"));
        assert!(s.cleared.contains("3"));
    }

    #[test]
    fn writes_stop_at_the_reserve_and_the_tail_is_named() {
        // Budget 1002, reserve 1000: thread 1 costs two writes, then 1000 ≤ 1000.
        let (api, gql) = fixture(None, 1002);
        let s = sweep(
            &api,
            &gql,
            &owners(),
            None,
            true,
            None,
            1000,
            &Cleared::new(),
        )
        .unwrap();
        assert_eq!(s.cleared, BTreeSet::from(["1".to_string()]));
        assert!(s.left["3"].contains("reserve"));
    }

    #[test]
    fn limit_and_repo_narrow_the_sweep() {
        let (api, gql) = fixture(None, 5000);
        let s = sweep(
            &api,
            &gql,
            &owners(),
            None,
            true,
            Some(1),
            1000,
            &Cleared::new(),
        )
        .unwrap();
        assert_eq!(s.cleared.len(), 1);
        assert!(s.left.values().all(|w| w.contains("--limit")));

        let (api, gql) = fixture(None, 5000);
        let s = sweep(
            &api,
            &gql,
            &owners(),
            Some("hyperpolymath/b"),
            false,
            None,
            1000,
            &Cleared::new(),
        )
        .unwrap();
        assert_eq!(s.due, BTreeSet::from(["3".to_string()]));
    }

    #[test]
    fn a_second_sweep_with_the_saved_state_writes_nothing() {
        let (api, gql) = fixture(None, 5000);
        let first = sweep(
            &api,
            &gql,
            &owners(),
            None,
            true,
            None,
            1000,
            &Cleared::new(),
        )
        .unwrap();
        assert_eq!(first.state.len(), 2);

        let (api, gql) = fixture(None, 5000);
        let second = sweep(&api, &gql, &owners(), None, true, None, 1000, &first.state).unwrap();
        assert!(api.writes.borrow().is_empty());
        assert_eq!(second.already_cleared, 2);
        assert!(second.due.is_empty());
        assert_eq!(second.state, first.state);
    }

    #[test]
    fn new_activity_on_a_cleared_thread_is_decided_again() {
        let (api, gql) = fixture(None, 5000);
        let first = sweep(
            &api,
            &gql,
            &owners(),
            None,
            true,
            None,
            1000,
            &Cleared::new(),
        )
        .unwrap();

        let (mut api, gql) = fixture(None, 5000);
        api.threads[0].updated_at = "2026-10-02T00:00:00Z".into();
        let s = sweep(&api, &gql, &owners(), None, true, None, 1000, &first.state).unwrap();
        assert_eq!(*api.writes.borrow(), ["unsub 1", "done 1"]);
        assert_eq!(s.state["1"], "2026-10-02T00:00:00Z");
    }

    #[test]
    fn state_for_threads_no_longer_listed_is_dropped() {
        let prior = Cleared::from([("gone".to_string(), "x".to_string())]);
        let (api, gql) = fixture(None, 5000);
        let s = sweep(&api, &gql, &owners(), None, false, None, 1000, &prior).unwrap();
        assert!(!s.state.contains_key("gone"));
    }

    #[test]
    fn state_file_round_trips_and_a_corrupt_one_is_refused() {
        let dir = std::env::temp_dir().join(format!("squabble-inbox-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        assert!(load_state(&path).unwrap().is_empty());
        let st = Cleared::from([("1".to_string(), "t".to_string())]);
        save_state(&path, &st).unwrap();
        assert_eq!(load_state(&path).unwrap(), st);
        std::fs::write(&path, b"{not json").unwrap();
        assert!(load_state(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rest_notification_json_parses() {
        let n = json!({
            "id": "26025256674", "reason": "author", "unread": false,
            "updated_at": "2026-10-01T13:55:02Z",
            "subject": { "type": "PullRequest", "url": "https://api.github.com/repos/o/r/pulls/4" },
            "repository": { "full_name": "o/r" }
        });
        let th = thread_from_json(&n).unwrap();
        assert_eq!((th.id.as_str(), th.repo.as_str()), ("26025256674", "o/r"));
        assert!(thread_from_json(&json!({ "id": "1" })).is_none());
    }

    #[test]
    fn flags_parse_and_bad_values_fail() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let a = parse_args(&v(&["--apply", "--limit", "3", "--repo", "o/r"])).unwrap();
        assert!(a.apply && a.limit == Some(3) && a.repo.as_deref() == Some("o/r"));
        assert!(parse_args(&v(&["--repo", "nope"])).is_err());
        assert!(parse_args(&v(&["--limit"])).is_err());
        assert!(parse_args(&v(&["--frobnicate"])).is_err());
    }
}
