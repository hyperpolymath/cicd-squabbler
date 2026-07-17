// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Applying self-win moves to the working tree — the squabbler's *first
//! mutating path*, and the one place `squabble ≠ bypass` must be defended at
//! write time, not just at plan time.
//!
//! Only [`Move::InjectPathFilterPassThrough`] is appliable in v0.1, and it is
//! chosen deliberately: it is a **pure local file edit** (no network, no
//! GitHub-ruleset API, no git) and it is **provably gate-strengthening**.
//! Removing an `on.*.paths` inclusion filter makes the workflow trigger on
//! *more* pull requests — so the required check is *created and evaluated more
//! often, never less*. It cannot turn a `Failed` run into `Passed`, and it
//! cannot drop a required context, so it is not a path to `Green` by weakening
//! anything (`squabble-core`'s [`squabble_core::gate::Gate::evaluate`] is
//! untouched; the SPARK invariant needs no re-proof). It is only ever proposed
//! for a *Missing* (stranded) check, where the PR is already blocked — so the
//! transition can only be blocked→(green-or-legitimately-red), never a
//! regression.
//!
//! The other self-win moves stay propose-only for now: reconciling a required
//! context or re-pinning a reusable needs the GitHub ruleset API / a target
//! SHA (network), and ground-truthing is a diagnosis, not an edit. They are
//! left untouched here and surface in the report as proposals.
//!
//! What this module does **not** do — on purpose:
//!
//! * **No commit, no push.** It writes files and stops. Committing and pushing
//!   are outward-facing, hard-to-undo acts and are the operator's step
//!   (doctrine #11, stop-first; the squabbler is not a lander).
//! * **No re-run / re-evaluation.** Only CI can re-run the checks, so applying
//!   an edit does not make the gate green; the caller keeps the [`Outcome`] red
//!   and records what changed (`no-overclaim`).
//! * **No cost optimisation yet.** The estate doctrine pairs "drop `on.*.paths`"
//!   with an always-run `changes` detector + `needs:`/`if:` guards so unrelated
//!   PRs skip heavy work (a skipped-by-`if` job still counts as a passing
//!   required check). Modelling each job's real path-relevance is a job-graph
//!   transform too risky for a line-based editor, so v0.1 does the minimal,
//!   always-correct strip and leaves the detector as a documented follow-up.
//!
//! [`Outcome`]: squabble_core::outcome::Outcome

use squabble_core::moves::Move;
use squabble_core::outcome::AppliedChange;
use std::path::Path;

/// The result of an [`apply_moves`] run: what was written, and every move that
/// could not be applied (recorded, never silently skipped — `no-silent-skip`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// Concrete edits written to disk, one per successfully-applied move.
    pub applied: Vec<AppliedChange>,
    /// Human blocker lines for moves that were appliable-in-principle but could
    /// not be enacted (unreadable/unwritable file, nothing to strip).
    pub skipped: Vec<String>,
}

/// Apply every appliable self-win move in `moves` to the tree at `repo_root`.
///
/// Fail-closed and per-move: an I/O failure on one move is recorded in
/// [`ApplyOutcome::skipped`] and the rest still run; nothing is committed or
/// pushed. Non-appliable moves (escalations, owner assignments, and the
/// propose-only self-wins) are ignored here — they are not this module's job.
pub fn apply_moves(repo_root: &Path, moves: &[Move]) -> ApplyOutcome {
    let mut out = ApplyOutcome::default();
    for m in moves {
        if let Move::InjectPathFilterPassThrough { check, workflow } = m {
            match strip_path_filter(repo_root, workflow) {
                Ok(Some(detail)) => out.applied.push(AppliedChange {
                    move_kind: "inject-path-filter-pass-through".to_string(),
                    file: format!(".github/workflows/{workflow}"),
                    detail,
                }),
                // Idempotent no-op: the filter was already gone. Say so rather
                // than claim an edit that did not happen.
                Ok(None) => out.skipped.push(format!(
                    "`{check}`: `{workflow}` has no `on.*.paths` filter to strip (already applied?) — no change written"
                )),
                Err(e) => out
                    .skipped
                    .push(format!("`{check}`: could not strip path filter from `{workflow}`: {e}")),
            }
        }
    }
    out
}

/// Strip the `on.*.paths` inclusion filter from one workflow file, in place.
///
/// Returns `Ok(Some(detail))` when a filter was found and removed (the file was
/// rewritten), `Ok(None)` when there was nothing to strip (idempotent no-op),
/// or `Err` when the file could not be read or written.
fn strip_path_filter(repo_root: &Path, workflow: &str) -> Result<Option<String>, String> {
    let path = repo_root.join(".github/workflows").join(workflow);
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let (new_text, removed) = remove_on_paths(&text);
    if !removed {
        return Ok(None);
    }
    std::fs::write(&path, &new_text).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(Some(
        "removed the `on.*.paths` trigger filter so the required check is always created \
         (it now runs on every PR — a strict coverage increase, never a bypass); \
         layering an always-run `changes` detector to skip heavy work on unrelated PRs \
         is a documented follow-up"
            .to_string(),
    ))
}

/// Remove any top-level `on.*.paths` filter block from workflow YAML text,
/// returning the rewritten text and whether anything was removed.
///
/// Deliberately a **tolerant, line-based** transform matching the ground-truth
/// scanner's style (see [`crate::workflows`]) rather than a full YAML engine:
///
/// * Only the trigger block is touched — scanning stops at the top-level
///   `jobs:` key, so a `paths:` appearing inside a job/step is never removed.
/// * Both forms are handled: the block form (`paths:` then an indented list)
///   and the inline form (`paths: ['a', 'b']` on one line).
/// * Only `paths:` (the *inclusion* filter that strands off-path PRs) is
///   removed. `paths-ignore:` is left alone — it is a different, rarer shape and
///   conservative-by-default beats a broader edit we are less sure of.
///
/// Removing `paths:` can leave a trigger key with no mapping (e.g. a bare
/// `pull_request:`); GitHub Actions treats that as "trigger on all events of
/// that type", which is exactly the intended un-stranding.
fn remove_on_paths(text: &str) -> (String, bool) {
    let mut out: Vec<&str> = Vec::new();
    let mut before_jobs = true;
    let mut removed = false;
    // When `Some(key_indent)` we are inside a removed block-form `paths:` value
    // and skip every following more-indented line (and blanks) until dedent.
    let mut skipping: Option<usize> = None;

    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();

        if let Some(key_indent) = skipping {
            if t.is_empty() {
                // A blank line does not end the list value; drop it too so no
                // stray blank is left where the filter was.
                continue;
            }
            if indent > key_indent {
                // Still inside the `paths:` list value → skip.
                continue;
            }
            // Dedented to <= the key: the block ended. Fall through and handle
            // this line normally.
            skipping = None;
        }

        if before_jobs && indent == 0 && t.starts_with("jobs:") {
            before_jobs = false;
        }

        if before_jobs && (t == "paths:" || t.starts_with("paths:")) {
            let after = t.strip_prefix("paths:").unwrap_or("").trim();
            removed = true;
            if after.is_empty() {
                // Block form: also skip the indented list that follows.
                skipping = Some(indent);
            }
            // Inline or block form: drop this key line either way.
            continue;
        }

        out.push(line);
    }

    let mut joined = out.join("\n");
    if text.ends_with('\n') {
        joined.push('\n');
    }
    (joined, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_block_form_paths_and_keeps_siblings() {
        let src = "\
name: Lint
on:
  pull_request:
    paths:
      - '.github/workflows/**'
      - 'src/**'
    branches: [main]
jobs:
  lint:
    runs-on: ubuntu-latest
";
        let (out, removed) = remove_on_paths(src);
        assert!(removed);
        assert!(!out.contains("paths:"));
        assert!(!out.contains(".github/workflows/**"));
        // Sibling trigger keys and the jobs block survive intact.
        assert!(out.contains("branches: [main]"));
        assert!(out.contains("pull_request:"));
        assert!(out.contains("jobs:"));
        assert!(out.contains("  lint:"));
    }

    #[test]
    fn strips_inline_form_paths() {
        let src = "\
on:
  push:
    paths: ['a', 'b']
    branches: [main]
jobs:
  x:
    runs-on: ubuntu-latest
";
        let (out, removed) = remove_on_paths(src);
        assert!(removed);
        assert!(!out.contains("paths:"));
        assert!(out.contains("branches: [main]"));
    }

    #[test]
    fn leaves_bare_trigger_when_paths_was_the_only_subkey() {
        let src = "\
on:
  pull_request:
    paths:
      - '.github/workflows/**'
jobs:
  x:
    runs-on: ubuntu-latest
";
        let (out, removed) = remove_on_paths(src);
        assert!(removed);
        // `pull_request:` becomes a bare trigger (all PRs) — valid and intended.
        assert!(out.contains("pull_request:"));
        assert!(!out.contains("paths:"));
        assert!(out.contains("jobs:"));
    }

    #[test]
    fn does_not_touch_paths_inside_jobs() {
        // A `paths:` under a job/step (after `jobs:`) must be preserved — it is
        // not a trigger filter.
        let src = "\
on:
  pull_request:
    paths:
      - 'src/**'
jobs:
  x:
    steps:
      - uses: actions/some-action@v1
        with:
          paths: 'keep/me'
";
        let (out, removed) = remove_on_paths(src);
        assert!(removed);
        // The trigger filter is gone but the step's `paths:` input stays.
        assert!(out.contains("paths: 'keep/me'"));
        assert_eq!(out.matches("paths:").count(), 1);
    }

    #[test]
    fn is_idempotent() {
        let src = "\
on:
  pull_request:
    paths:
      - 'src/**'
jobs:
  x:
    runs-on: ubuntu-latest
";
        let (once, removed1) = remove_on_paths(src);
        assert!(removed1);
        let (twice, removed2) = remove_on_paths(&once);
        assert!(!removed2, "second pass must find nothing to strip");
        assert_eq!(once, twice);
    }

    #[test]
    fn no_filter_is_a_no_op() {
        let src = "\
on:
  pull_request:
    branches: [main]
jobs:
  x:
    runs-on: ubuntu-latest
";
        let (out, removed) = remove_on_paths(src);
        assert!(!removed);
        assert_eq!(out, src);
    }

    #[test]
    fn apply_moves_writes_the_file_and_records_the_change() {
        let dir = unique_tmp("apply-writes");
        let wf_dir = dir.join(".github/workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        let wf = wf_dir.join("linter.yml");
        std::fs::write(
            &wf,
            "on:\n  pull_request:\n    paths:\n      - '.github/workflows/**'\njobs:\n  lint:\n    runs-on: ubuntu-latest\n",
        )
        .unwrap();

        let out = apply_moves(
            &dir,
            &[Move::InjectPathFilterPassThrough {
                check: "lint-workflows".into(),
                workflow: "linter.yml".into(),
            }],
        );

        assert_eq!(out.applied.len(), 1);
        assert!(out.skipped.is_empty());
        assert_eq!(out.applied[0].file, ".github/workflows/linter.yml");
        assert_eq!(out.applied[0].move_kind, "inject-path-filter-pass-through");

        let written = std::fs::read_to_string(&wf).unwrap();
        assert!(!written.contains("paths:"), "filter must be gone on disk");

        // A second apply is a recorded no-op, not a spurious second change.
        let again = apply_moves(
            &dir,
            &[Move::InjectPathFilterPassThrough {
                check: "lint-workflows".into(),
                workflow: "linter.yml".into(),
            }],
        );
        assert!(again.applied.is_empty());
        assert_eq!(again.skipped.len(), 1);
        assert!(again.skipped[0].contains("no change written"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn apply_moves_is_fail_closed_on_missing_file() {
        let dir = unique_tmp("apply-missing");
        // No workflow file exists — the move must be recorded as skipped, not
        // silently dropped, and nothing must panic.
        let out = apply_moves(
            &dir,
            &[Move::InjectPathFilterPassThrough {
                check: "lint-workflows".into(),
                workflow: "nope.yml".into(),
            }],
        );
        assert!(out.applied.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert!(out.skipped[0].contains("could not strip path filter"));
    }

    #[test]
    fn apply_moves_ignores_non_appliable_moves() {
        let dir = unique_tmp("apply-ignore");
        let out = apply_moves(
            &dir,
            &[
                Move::GroundTruthCheckNames {
                    workflow: "ci.yml".into(),
                },
                Move::AssignGateOwner {
                    check: "gov".into(),
                    owner: "hyperpolymath/standards".into(),
                    disposition: squabble_core::moves::OwnershipDisposition::Ownerless,
                    rationale: "x".into(),
                },
            ],
        );
        assert!(out.applied.is_empty());
        assert!(out.skipped.is_empty());
    }

    /// A unique, isolated temp dir without pulling in a `tempfile` dependency
    /// (this crate stays dep-light). Namespaced by pid + a label so parallel
    /// test threads never collide.
    fn unique_tmp(label: &str) -> std::path::PathBuf {
        let base =
            std::env::temp_dir().join(format!("squabble-apply-{}-{}", std::process::id(), label));
        std::fs::remove_dir_all(&base).ok();
        std::fs::create_dir_all(&base).unwrap();
        base
    }
}
