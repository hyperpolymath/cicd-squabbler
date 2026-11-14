// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! The SQ-LOCK-01 executor core (ADR 0004): repair an `actions.lock` record
//! keyed `owner/repo@<40-hex>` whose `ref:` repeats that SHA.
//!
//! Pure: no network, no filesystem. The caller gathers the evidence (the tags
//! that point at the SHA, the branches that contain it) and writes the result;
//! this module chooses the ref, rewrites only the flagged `ref:` lines, and
//! checks its own work before it returns text. A repair it cannot make is a
//! [`Refusal`], never a guess.
//!
//! The order is the catalogue row's: the highest full-semver tag at the SHA,
//! else the highest semver tag, else the first tag, else a branch that
//! contains the commit. Tags that tie, or no tag and no branch, are refusals.

use crate::rules::{indent_of, is_sha40, self_sha_lock_records, self_sha_record_key, unquote};
use std::collections::BTreeMap;

/// What the caller found out about one record's SHA in its repository.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RefEvidence {
    /// Tags whose commit is the SHA.
    pub tags: Vec<String>,
    /// Branches that contain the SHA.
    pub branches: Vec<String>,
    /// The repository's default branch, when known.
    pub default_branch: Option<String>,
}

/// Why a record is not repaired. Each is a referral, not a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No evidence was gathered for the key.
    NoEvidence,
    /// No tag points at the SHA and no branch contains it.
    NoRef,
    /// Several refs rank equal; the candidates are named.
    Tie(Vec<String>),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NoEvidence => write!(f, "no evidence was gathered for this record"),
            Refusal::NoRef => write!(f, "no tag or branch holds the commit"),
            Refusal::Tie(c) => write!(f, "refs tie: {}", c.join(", ")),
        }
    }
}

/// A tag's numeric parts, `v1.2.3` -> [1, 2, 3], or None when it is not a
/// plain `MAJOR[.MINOR[.PATCH]]` (a pre-release or any other suffix).
fn semver_parts(tag: &str) -> Option<Vec<u64>> {
    let body = tag.strip_prefix('v').unwrap_or(tag);
    let parts: Vec<u64> = body
        .split('.')
        .map(|p| {
            let plain = !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit());
            plain.then(|| p.parse().ok()).flatten()
        })
        .collect::<Option<_>>()?;
    (1..=3).contains(&parts.len()).then_some(parts)
}

/// The highest of `candidates` by their parts, or a tie naming the top ones.
fn highest(candidates: &[(&String, Vec<u64>)]) -> Result<String, Refusal> {
    let key = |p: &[u64]| [p.first(), p.get(1), p.get(2)].map(|v| v.copied().unwrap_or(0));
    let top = candidates.iter().map(|(_, p)| key(p)).max();
    let mut best: Vec<&String> = candidates
        .iter()
        .filter(|(_, p)| Some(key(p)) == top)
        .map(|(t, _)| *t)
        .collect();
    best.sort();
    best.dedup();
    match best.as_slice() {
        [one] => Ok((*one).clone()),
        _ => Err(Refusal::Tie(best.into_iter().cloned().collect())),
    }
}

/// Choose the symbolic ref that replaces a SHA-as-ref.
pub fn choose_ref(e: &RefEvidence) -> Result<String, Refusal> {
    let parsed: Vec<(&String, Vec<u64>)> = e
        .tags
        .iter()
        .filter_map(|t| semver_parts(t).map(|p| (t, p)))
        .collect();
    let full: Vec<_> = parsed
        .iter()
        .filter(|(_, p)| p.len() == 3)
        .cloned()
        .collect();
    if !full.is_empty() {
        return highest(&full);
    }
    if !parsed.is_empty() {
        return highest(&parsed);
    }
    if let Some(first) = e.tags.iter().min() {
        return Ok(first.clone());
    }
    choose_branch(e)
}

fn choose_branch(e: &RefEvidence) -> Result<String, Refusal> {
    let mut branches: Vec<&String> = e.branches.iter().collect();
    branches.sort();
    branches.dedup();
    match (branches.as_slice(), &e.default_branch) {
        ([], _) => Err(Refusal::NoRef),
        (_, Some(d)) if branches.contains(&d) => Ok(d.clone()),
        ([one], _) => Ok((*one).clone()),
        _ => Err(Refusal::Tie(branches.into_iter().cloned().collect())),
    }
}

/// A ref name safe to write between single quotes: no quote, space, colon or
/// control character, not empty, and not itself a 40-hex SHA.
fn ref_is_writable(r: &str) -> bool {
    !r.is_empty()
        && !is_sha40(r)
        && r.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-+".contains(&b))
}

/// Rewrite the direct `ref:` line of each record in `edits` (lowercased
/// `owner/repo@sha` -> new ref). Every other byte is kept, line endings
/// included. Errors when a ref is not writable or a record has no `ref:` line.
pub fn rewrite_refs(lock: &str, edits: &BTreeMap<String, String>) -> Result<String, String> {
    if let Some((k, r)) = edits.iter().find(|(_, r)| !ref_is_writable(r)) {
        return Err(format!("`{r}` is not a writable ref for {k}"));
    }
    let mut out = String::with_capacity(lock.len());
    let mut current: Option<(String, usize)> = None;
    let mut done: Vec<&String> = Vec::new();
    for raw in lock.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\n', '\r']);
        let eol = &raw[line.len()..];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push_str(raw);
            continue;
        }
        let ind = indent_of(line);
        if current.as_ref().is_some_and(|(_, k)| ind <= *k) {
            current = None;
        }
        let Some((key, key_ind)) = current.clone() else {
            current = self_sha_record_key(trimmed).map(|k| (k, ind));
            out.push_str(raw);
            continue;
        };
        match edits.get_key_value(&key) {
            Some((k, new)) if ind > key_ind && trimmed.starts_with("ref:") => {
                out.push_str(&format!("{}ref: '{new}'{eol}", &line[..ind]));
                done.push(k);
            }
            _ => out.push_str(raw),
        }
    }
    match edits.keys().find(|k| !done.contains(k)) {
        Some(k) => Err(format!("record {k} has no direct `ref:` line to rewrite")),
        None => Ok(out),
    }
}

/// The lowercased `owner/repo@<40-hex>` keys of the reusable workflows a
/// workflow file calls at job level (`uses: o/r/.github/workflows/x.yml@sha`).
/// A step's action can never name a `.github/workflows/` path, so that path is
/// the test. A caller's lock record legitimately repeats its SHA as its ref,
/// so it must never be rewritten.
pub fn reusable_callers(workflow_yaml: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in workflow_yaml.lines() {
        let t = line.trim().trim_start_matches('-').trim();
        let Some(value) = t.strip_prefix("uses:") else {
            continue;
        };
        let spec = unquote(value);
        let spec = spec.split_whitespace().next().unwrap_or("");
        let Some((path, sha)) = spec.rsplit_once('@') else {
            continue;
        };
        let mut parts = path.splitn(3, '/');
        let (owner, repo, rest) = (parts.next(), parts.next(), parts.next());
        if let (Some(o), Some(r), Some(rest)) = (owner, repo, rest) {
            let key = format!("{o}/{r}@{sha}").to_ascii_lowercase();
            if rest.starts_with(".github/workflows/") && is_sha40(sha) && !out.contains(&key) {
                out.push(key);
            }
        }
    }
    out
}

/// What [`plan_lock_fix`] decided.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LockFixPlan {
    /// Records to rewrite: key -> the ref they will name.
    pub edits: BTreeMap<String, String>,
    /// Records left as they are, and why.
    pub refused: Vec<(String, Refusal)>,
    /// The repaired lock text, when there is at least one edit.
    pub new_lock: Option<String>,
}

/// Plan the repair of every self-SHA record in `lock`. `callers` are the
/// job-level reusable callers (a record they name is valid as it is).
///
/// Errors only when its own check fails: the result must still parse, differ
/// from the input in exactly the edited `ref:` lines, and no longer flag any
/// edited record. A failed check returns no text.
pub fn plan_lock_fix(
    lock: &str,
    callers: &[String],
    evidence: &BTreeMap<String, RefEvidence>,
) -> Result<LockFixPlan, String> {
    let mut plan = LockFixPlan::default();
    for key in self_sha_lock_records(lock, callers) {
        let chosen = evidence
            .get(&key)
            .ok_or(Refusal::NoEvidence)
            .and_then(choose_ref);
        match chosen {
            Ok(r) => {
                plan.edits.insert(key, r);
            }
            Err(why) => plan.refused.push((key, why)),
        }
    }
    if plan.edits.is_empty() {
        return Ok(plan);
    }
    let new = rewrite_refs(lock, &plan.edits)?;
    check_rewrite(lock, &new, callers, &plan)?;
    plan.new_lock = Some(new);
    Ok(plan)
}

/// The repair's own check (fail closed): same line count, only the edited
/// `ref:` lines differ, and the edited records are no longer flagged while the
/// refused ones still are.
fn check_rewrite(
    old: &str,
    new: &str,
    callers: &[String],
    plan: &LockFixPlan,
) -> Result<(), String> {
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    if a.len() != b.len() {
        return Err("repair changed the number of lines".into());
    }
    let changed: Vec<(&&str, &&str)> = a.iter().zip(&b).filter(|(x, y)| x != y).collect();
    let only_refs = changed.iter().all(|(x, y)| {
        unquote(x.trim().trim_start_matches("ref:")).len() == 40 && y.trim().starts_with("ref: '")
    });
    if changed.len() != plan.edits.len() || !only_refs {
        return Err("repair changed lines other than the flagged `ref:` lines".into());
    }
    let after = self_sha_lock_records(new, callers);
    let refused: Vec<&String> = plan.refused.iter().map(|(k, _)| k).collect();
    let expected_left = after.iter().all(|k| refused.contains(&k));
    if !expected_left || refused.iter().any(|k| !after.contains(k)) {
        return Err("repair left a different set of flagged records than it planned".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0da816c05ae7486671d863e07935f93981c0c2d5";
    const SHA2: &str = "55556cf5ba71ba744695861503c13148d3915a5d";

    fn ev(tags: &[&str], branches: &[&str], default: Option<&str>) -> RefEvidence {
        RefEvidence {
            tags: tags.iter().map(|s| s.to_string()).collect(),
            branches: branches.iter().map(|s| s.to_string()).collect(),
            default_branch: default.map(String::from),
        }
    }

    fn lock() -> String {
        format!(
            "version: 'v0.0.3'\ndependencies:\n    'a/leaf@{SHA}':\n        ref: '{SHA}'\n        commit: 'sha1-{SHA}'\n        owner_id: 1\n    'a/caller@{SHA2}':\n        ref: '{SHA2}'\n        commit: 'sha1-{SHA2}'\n        uses:\n            - 'a/leaf@{SHA}'\n    'a/ok@v1':\n        ref: 'v1'\n"
        )
    }

    #[test]
    fn the_catalogue_order_of_refs() {
        // Highest full-semver tag wins over a higher short tag and any branch.
        assert_eq!(
            choose_ref(&ev(&["v1.2.3", "v1.10.0", "v9"], &["main"], None)).unwrap(),
            "v1.10.0"
        );
        // No full-semver tag: the highest semver tag.
        assert_eq!(
            choose_ref(&ev(&["v1", "v2.1", "nightly"], &["main"], None)).unwrap(),
            "v2.1"
        );
        // No semver tag: the first tag.
        assert_eq!(
            choose_ref(&ev(&["rc", "beta"], &["main"], None)).unwrap(),
            "beta"
        );
        // A pre-release is not semver.
        assert_eq!(
            choose_ref(&ev(&["v1.2.3-rc1", "v0.9.0"], &[], None)).unwrap(),
            "v0.9.0"
        );
        // No tag: the default branch when it contains the commit, else the only one.
        assert_eq!(
            choose_ref(&ev(&[], &["dev", "main"], Some("main"))).unwrap(),
            "main"
        );
        assert_eq!(choose_ref(&ev(&[], &["only"], None)).unwrap(), "only");
    }

    #[test]
    fn ties_and_nothing_are_refusals_not_guesses() {
        assert_eq!(
            choose_ref(&ev(&["v1.2.3", "1.2.3"], &[], None)),
            Err(Refusal::Tie(vec!["1.2.3".into(), "v1.2.3".into()]))
        );
        assert_eq!(
            choose_ref(&ev(&[], &["a", "b"], Some("main"))),
            Err(Refusal::Tie(vec!["a".into(), "b".into()]))
        );
        assert_eq!(choose_ref(&ev(&[], &[], Some("main"))), Err(Refusal::NoRef));
        // 0.9.0 is lower than 0.10.0, not higher as a string compare says.
        assert_eq!(
            choose_ref(&ev(&["v0.9.0", "v0.10.0"], &[], None)).unwrap(),
            "v0.10.0"
        );
        // An oversized component is not semver (no overflow).
        assert_eq!(
            choose_ref(&ev(&["v99999999999999999999.0.0", "v1.0.0"], &[], None)).unwrap(),
            "v1.0.0"
        );
    }

    #[test]
    fn rewrite_touches_only_the_named_ref_line() {
        let l = lock();
        let mut edits = BTreeMap::new();
        edits.insert(format!("a/leaf@{SHA}"), "main".to_string());
        let new = rewrite_refs(&l, &edits).unwrap();
        assert_eq!(new.lines().count(), l.lines().count());
        assert!(new.contains(&format!(
            "'a/leaf@{SHA}':\n        ref: 'main'\n        commit: 'sha1-{SHA}'"
        )));
        // The caller record with the same ref shape is untouched.
        assert!(new.contains(&format!("'a/caller@{SHA2}':\n        ref: '{SHA2}'")));
        assert!(new.contains("'a/ok@v1':\n        ref: 'v1'\n"));
    }

    #[test]
    fn rewrite_keeps_line_endings_and_refuses_bad_refs() {
        let l = lock().replace('\n', "\r\n");
        let mut edits = BTreeMap::new();
        edits.insert(format!("a/leaf@{SHA}"), "v1".to_string());
        let new = rewrite_refs(&l, &edits).unwrap();
        assert!(new.contains("ref: 'v1'\r\n") && !new.contains("ref: 'v1'\n\n"));
        for bad in ["", "a b", "it's", "x:y", SHA, "a\nb"] {
            let mut e = BTreeMap::new();
            e.insert(format!("a/leaf@{SHA}"), bad.to_string());
            assert!(rewrite_refs(&l, &e).is_err(), "{bad:?}");
        }
        let mut missing = BTreeMap::new();
        missing.insert("a/none@v1".to_string(), "main".to_string());
        assert!(rewrite_refs(&l, &missing).is_err());
    }

    #[test]
    fn plan_repairs_the_leaf_and_leaves_the_exempt_caller() {
        let l = lock();
        let callers = vec![format!("a/caller@{SHA2}")];
        let mut evidence = BTreeMap::new();
        evidence.insert(format!("a/leaf@{SHA}"), ev(&[], &["main"], Some("main")));
        let plan = plan_lock_fix(&l, &callers, &evidence).unwrap();
        assert_eq!(
            plan.edits.get(&format!("a/leaf@{SHA}")).map(String::as_str),
            Some("main")
        );
        assert!(plan.refused.is_empty());
        let new = plan.new_lock.unwrap();
        assert!(self_sha_lock_records(&new, &callers).is_empty());
        // Idempotent: nothing is left to repair.
        assert!(plan_lock_fix(&new, &callers, &evidence)
            .unwrap()
            .new_lock
            .is_none());
    }

    #[test]
    fn a_refused_record_stays_flagged_and_blocks_nothing_else() {
        let l = lock().replace(
            "a/ok@v1':\n        ref: 'v1'",
            &format!("b/two@{SHA2}':\n        ref: '{SHA2}'"),
        );
        let callers: Vec<String> = Vec::new();
        let mut evidence = BTreeMap::new();
        evidence.insert(format!("a/leaf@{SHA}"), ev(&["v1.0.0"], &[], None));
        let plan = plan_lock_fix(&l, &callers, &evidence).unwrap();
        // The leaf is repaired, the two without evidence are refused and still flagged.
        assert_eq!(plan.edits.len(), 1);
        assert_eq!(plan.refused.len(), 2);
        assert!(plan.refused.iter().all(|(_, r)| *r == Refusal::NoEvidence));
        let left = self_sha_lock_records(plan.new_lock.as_deref().unwrap(), &callers);
        assert_eq!(left.len(), 2);
    }

    #[test]
    fn reusable_callers_are_read_from_job_level_uses_only() {
        let wf = format!(
            "jobs:\n  g:\n    uses: Meta/Standards/.github/workflows/x-reusable.yml@{SHA2} # note\n  s:\n    steps:\n      - uses: actions/checkout@{SHA}\n      - uses: \"a/b/.github/workflows/y.yml@v1\"\n  t:\n    uses: ./.github/workflows/local.yml\n"
        );
        // Only the full-SHA reusable caller, lowercased; a step action, a tag
        // ref and a local path are not callers.
        assert_eq!(
            reusable_callers(&wf),
            vec![format!("meta/standards@{SHA2}")]
        );
        assert!(reusable_callers("name: x\n").is_empty());
    }

    #[test]
    fn nothing_to_repair_is_a_plan_with_no_text() {
        let plan = plan_lock_fix(
            "version: 'v0.0.3'\ndependencies:\n    'a/ok@v1':\n        ref: 'v1'\n",
            &[],
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(plan.edits.is_empty() && plan.refused.is_empty() && plan.new_lock.is_none());
    }

    #[test]
    fn the_repair_check_catches_an_edit_that_strays() {
        let l = lock();
        let mut plan = LockFixPlan::default();
        plan.edits.insert(format!("a/leaf@{SHA}"), "main".into());
        // A rewrite that also changed another line must not pass the check.
        let stray = l
            .replacen(&format!("ref: '{SHA}'"), "ref: 'main'", 1)
            .replace("owner_id: 1", "owner_id: 2");
        assert!(check_rewrite(&l, &stray, &[], &plan).is_err());
        // One that changed nothing must not pass either.
        assert!(check_rewrite(&l, &l, &[], &plan).is_err());
    }
}
