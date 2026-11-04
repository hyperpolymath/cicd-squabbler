// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Read-only native Actions lock diagnosis. The official tool owns parsing;
//! squabbler preserves its output and checks the observation's provenance.

use serde::Deserialize;
use squabble_core::startup::{same_source_revision, NativeLockStatus, NativeLockVerification};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Output};

const SUPPORTED_VERSION: &str = "actions-lock version v0.2.0";

/// Bind the workflow tree to a complete commit ID. Uncommitted or unreadable
/// workflow inputs cannot explain a historical run at that commit.
pub(crate) fn checkout_revision(root: &Path) -> Result<String, String> {
    source_revision(root, false)
}

/// The official reader also follows same-repository action definitions. A
/// pristine full checkout closes that input set without a second action parser.
fn source_revision(root: &Path, full_tree: bool) -> Result<String, String> {
    let git = |args: &[&str]| -> Result<Output, String> {
        Command::new("git")
            .current_dir(root)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .map_err(|e| format!("cannot read checkout identity: {e}"))
    };
    let top = git(&["rev-parse", "--show-toplevel"])?;
    let canonical = root
        .canonicalize()
        .map_err(|e| format!("cannot resolve checkout root: {e}"))?;
    let top_path = String::from_utf8_lossy(&top.stdout).trim().to_string();
    if !top.status.success()
        || Path::new(&top_path).canonicalize().ok().as_ref() != Some(&canonical)
    {
        return Err(
            "path is not the repository root; nested workflow trees cannot explain this run".into(),
        );
    }
    for path in [root.join(".github"), root.join(".github/workflows")] {
        if path
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_symlink())
        {
            return Err("workflow source contains a symlink component".into());
        }
    }
    let head = git(&["rev-parse", "--verify", "HEAD"])?;
    let sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
    if !head.status.success() || !same_source_revision(Some(&sha), Some(&sha)) {
        return Err("checkout HEAD is not a readable full commit ID".into());
    }
    let scope = if full_tree { "." } else { ".github/workflows" };
    let state = git(&[
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        "--ignored=matching",
        "--",
        scope,
    ])?;
    if !state.status.success() || !state.stdout.is_empty() {
        return Err("source tree has uncommitted, untracked, ignored or unreadable inputs; use a pristine checkout at the run's commit".into());
    }
    // Assume-unchanged / skip-worktree flags can otherwise hide source drift.
    let tracked = git(&["ls-files", "-v", "--", scope])?;
    if !tracked.status.success()
        || String::from_utf8_lossy(&tracked.stdout)
            .lines()
            .any(|line| !line.starts_with("H "))
    {
        return Err("workflow index cannot establish an unmodified checkout".into());
    }
    let tree = git(&["ls-tree", "-r", "-z", "HEAD", "--", scope])?;
    if !tree.status.success()
        || tree
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
            .any(|entry| !entry.starts_with(b"100644 blob ") && !entry.starts_with(b"100755 blob "))
    {
        return Err("source tree contains a symlink, gitlink or unsupported file mode".into());
    }
    Ok(sha)
}

fn command(binary: &Path, root: &Path) -> Command {
    let mut cmd = Command::new(binary);
    cmd.current_dir(root);
    for name in [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
        "GH_ACTIONS_LOCK_WORKFLOWS_DIR",
        "GH_ACTIONS_LOCK_REPO_ROOT",
    ] {
        cmd.env_remove(name);
    }
    cmd
}

/// Invoke an explicitly selected, operator-verified official binary. There
/// is no download, resolver, credential requirement, write mode or fallback.
pub(crate) fn verify(
    binary: &Path,
    root: &Path,
    head_sha: &str,
    workflows: &[String],
) -> NativeLockVerification {
    let mut evidence = NativeLockVerification {
        head_sha: head_sha.into(),
        verifier: binary.display().to_string(),
        tool_version: String::new(),
        exit_code: None,
        status: NativeLockStatus::Unverified,
        stdout: String::new(),
        stderr: String::new(),
        reason: String::new(),
    };
    match source_revision(root, true) {
        Ok(observed) if same_source_revision(Some(&observed), Some(head_sha)) => {}
        result => {
            evidence.reason = format!("native verifier not run: full input closure is not a pristine checkout at the run's source ({result:?})");
            return evidence;
        }
    }
    let version = match command(binary, root).arg("--version").output() {
        Ok(output) => output,
        Err(error) => {
            evidence.reason = format!("native lock verifier did not run: {error}");
            return evidence;
        }
    };
    evidence.tool_version = String::from_utf8_lossy(&version.stdout).trim().into();
    if !version.status.success()
        || evidence.tool_version != SUPPORTED_VERSION
        || !version.stderr.is_empty()
    {
        evidence.stderr = String::from_utf8_lossy(&version.stderr).into();
        evidence.reason =
            "unsupported or unreadable verifier version; no lock result inferred".into();
        return evidence;
    }
    match command(binary, root)
        .args(["--verify-local", "--json", "--hostname", "github.com"])
        .output()
    {
        Ok(output) => {
            evidence.exit_code = output.status.code();
            evidence.stdout = String::from_utf8_lossy(&output.stdout).into();
            evidence.stderr = String::from_utf8_lossy(&output.stderr).into();
            let (status, reason) = classify(
                evidence.exit_code,
                &evidence.stdout,
                &evidence.stderr,
                workflows,
            );
            evidence.status = status;
            evidence.reason = reason;
        }
        Err(error) => evidence.reason = format!("native lock verifier did not run: {error}"),
    }
    // A concurrent checkout/edit invalidates the source binding, even when
    // the subprocess returned a syntactically valid positive result.
    if !same_source_revision(source_revision(root, true).ok().as_deref(), Some(head_sha)) {
        evidence.status = NativeLockStatus::Unverified;
        evidence.reason =
            "checkout changed or became unreadable during native lock verification".into();
    }
    evidence
}

#[derive(Deserialize)]
struct NativeReport {
    cli_version: String,
    lockfile_version: String,
    valid: bool,
    findings: Vec<NativeFinding>,
    workflows: Vec<NativeWorkflow>,
}

#[derive(Deserialize)]
struct NativeWorkflow {
    path: String,
    valid: bool,
    findings: Vec<NativeFinding>,
}

#[derive(Clone, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct NativeFinding {
    workflow: String,
    category: String,
    severity: String,
    detail: String,
    #[serde(default)]
    dependency: Option<String>,
}

/// v0.2.0 contract: 0 = valid, 1 = reported blocking findings, 2 = tool error.
/// Source: github/gh-actions-lock d5dca22a06ff8590d50b41612038996800cfb336,
/// cmd/gh-actions-lock/{root,verify}.go. Only a parser diagnostic within exit
/// 2 establishes malformed syntax; an IO error must remain unverified.
fn classify(
    exit: Option<i32>,
    stdout: &str,
    stderr: &str,
    workflows: &[String],
) -> (NativeLockStatus, String) {
    let unknown = || {
        (NativeLockStatus::Unverified, "native verifier output is incomplete, contradictory or a tool failure; inspect the retained exit and output".into())
    };
    if exit == Some(2)
        && stdout.trim().is_empty()
        && stderr.trim_start().starts_with(
            "opening lockfile: lockfile is unreadable: .github/workflows/actions.lock: line ",
        )
    {
        return (NativeLockStatus::Malformed, "the official native parser rejected this source lock; GitHub's own startup cause has not been independently read".into());
    }
    if !matches!(exit, Some(0 | 1)) || !stderr.trim().is_empty() {
        return unknown();
    }
    // Typed decoding rejects duplicate known keys (Value would silently keep
    // the last `valid` field and could turn contradictory output into green).
    let Ok(mut report) = serde_json::from_str::<NativeReport>(stdout) else {
        return unknown();
    };
    if report.cli_version != "v0.2.0" || report.lockfile_version.is_empty() {
        return unknown();
    }
    let expected: BTreeSet<_> = workflows.iter().map(String::as_str).collect();
    let mut seen = BTreeSet::new();
    let mut all_valid = true;
    let mut flattened = Vec::new();
    for workflow in &report.workflows {
        if !seen.insert(workflow.path.as_str())
            || (workflow.valid && !workflow.findings.is_empty())
            || (!workflow.valid
                && !workflow
                    .findings
                    .iter()
                    .any(|finding| finding.severity == "error"))
            || workflow.findings.iter().any(|finding| {
                finding.workflow != workflow.path
                    || finding.category.is_empty()
                    || finding.detail.is_empty()
            })
        {
            return unknown();
        }
        all_valid &= workflow.valid;
        flattened.extend(workflow.findings.iter().cloned());
    }
    flattened.sort();
    report.findings.sort();
    if expected.is_empty()
        || seen != expected
        || report.valid != all_valid
        || flattened != report.findings
    {
        return unknown();
    }
    match (exit, report.valid, report.findings.is_empty()) {
        (Some(0), true, true) => (NativeLockStatus::Verified, "official offline verification covered every local workflow; this is not a CI pass or a training outcome".into()),
        (Some(1), false, false) => (NativeLockStatus::CoverageMismatch, "official offline verification reported workflow/lock findings; inspect the workflow-specific evidence, not a settings change".into()),
        _ => unknown(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    struct Checkout(PathBuf);

    impl Checkout {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("squabble-native-{}-{stamp}", std::process::id()));
            std::fs::create_dir(&root).unwrap();
            let fixture = Self(root);
            fixture.git(&["init", "-q"]);
            for dir in [
                ".github/workflows",
                "nested/.github/workflows",
                "actions/demo",
            ] {
                std::fs::create_dir_all(fixture.0.join(dir)).unwrap();
            }
            std::fs::write(fixture.0.join(".github/workflows/ci.yml"), "name: fixture\non: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: $/actions/demo\n").unwrap();
            std::fs::write(
                fixture.0.join("nested/.github/workflows/ci.yml"),
                "name: wrong source\n",
            )
            .unwrap();
            std::fs::write(
                fixture.0.join("actions/demo/action.yaml"),
                "name: fixture\nruns:\n  using: composite\n  steps: []\n",
            )
            .unwrap();
            std::fs::write(fixture.0.join(".gitignore"), "actions/demo/action.yml\n").unwrap();
            fixture.commit();
            fixture
        }

        fn git(&self, args: &[&str]) {
            let output = Command::new("git")
                .current_dir(&self.0)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        fn commit(&self) {
            self.git(&["add", "--all"]);
            // Disposable test repositories only; no author or signing
            // configuration of the actual project is changed.
            self.git(&[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "fixture",
            ]);
        }
    }

    impl Drop for Checkout {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn source_identity_requires_the_repository_root_and_unmodified_workflows() {
        let checkout = Checkout::new();
        assert!(checkout_revision(&checkout.0).is_ok());
        assert!(source_revision(&checkout.0, true).is_ok());
        assert!(checkout_revision(&checkout.0.join("nested"))
            .unwrap_err()
            .contains("repository root"));
        std::fs::write(checkout.0.join(".github/workflows/ci.yml"), "changed\n").unwrap();
        assert!(checkout_revision(&checkout.0).is_err());
        checkout.git(&[
            "update-index",
            "--assume-unchanged",
            ".github/workflows/ci.yml",
        ]);
        assert!(
            checkout_revision(&checkout.0).is_err(),
            "index flags cannot hide modified source"
        );
    }

    #[test]
    fn changed_or_ignored_shadowing_self_actions_prevent_a_native_source_claim() {
        let checkout = Checkout::new();
        let head = checkout_revision(&checkout.0).unwrap();
        let action = checkout.0.join("actions/demo/action.yaml");
        let original = std::fs::read(&action).unwrap();
        std::fs::write(&action, "changed self action\n").unwrap();
        assert!(
            checkout_revision(&checkout.0).is_ok(),
            "coordinate-only workflow source is unchanged"
        );
        assert!(source_revision(&checkout.0, true).is_err());
        let result = verify(
            Path::new("/must-not-be-executed"),
            &checkout.0,
            &head,
            &[".github/workflows/ci.yml".into()],
        );
        assert_eq!(result.status, NativeLockStatus::Unverified);
        assert_eq!(result.exit_code, None);
        assert!(result.reason.contains("not a pristine checkout"));
        std::fs::write(&action, original).unwrap();
        assert!(source_revision(&checkout.0, true).is_ok());
        // The official reader prefers action.yml to action.yaml. Even an
        // ignored file can therefore replace the committed action input.
        std::fs::write(
            checkout.0.join("actions/demo/action.yml"),
            "ignored shadow\n",
        )
        .unwrap();
        assert!(source_revision(&checkout.0, true).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn committed_self_action_symlinks_are_not_treated_as_immutable_input_bytes() {
        let checkout = Checkout::new();
        let action = checkout.0.join("actions/demo/action.yaml");
        std::fs::remove_file(&action).unwrap();
        std::os::unix::fs::symlink("../../.gitignore", &action).unwrap();
        checkout.commit();
        assert!(source_revision(&checkout.0, true)
            .unwrap_err()
            .contains("symlink"));
    }

    fn coverage(valid: bool) -> String {
        let findings = if valid {
            json!([])
        } else {
            json!([{"workflow": ".github/workflows/ci.yml", "category": "stale", "severity": "error", "detail":"unused lock entry"}])
        };
        json!({"cli_version": "v0.2.0", "lockfile_version":"v0.0.3", "valid": valid, "findings": findings,
            "workflows": [{"path": ".github/workflows/ci.yml", "valid": valid, "findings": findings}]}).to_string()
    }

    #[test]
    fn native_parser_failure_is_distinct_from_io_and_coverage_findings() {
        let files = vec![".github/workflows/ci.yml".into()];
        // Observed from the official v0.2.0 binary on Hypatia's historical lock.
        let malformed = "opening lockfile: lockfile is unreadable: .github/workflows/actions.lock: line 347, column 5: dependency key \"google/clusterfuzzlite/actions/run_fuzzers@v1\" is not a valid pin (expected OWNER/REPO@REF)\n";
        assert_eq!(
            classify(Some(2), "", malformed, &files).0,
            NativeLockStatus::Malformed
        );
        assert_eq!(
            classify(Some(2), "", "opening lockfile: permission denied", &files).0,
            NativeLockStatus::Unverified
        );
        assert_eq!(
            classify(Some(2), "", "unsupported future lockfile version", &files).0,
            NativeLockStatus::Unverified
        );
        assert_eq!(
            classify(Some(1), &coverage(false), "", &files).0,
            NativeLockStatus::CoverageMismatch
        );
        assert_eq!(
            classify(Some(0), &coverage(true), "", &files).0,
            NativeLockStatus::Verified
        );
    }

    #[test]
    fn empty_malformed_partial_contradictory_and_wrong_version_results_never_verify() {
        let files = vec![".github/workflows/ci.yml".into()];
        for (exit, body) in [
            (Some(0), "".into()),
            (Some(0), "[]".into()),
            (Some(0), "{".into()),
            (Some(0), coverage(false)),
            (Some(1), coverage(true)),
            (None, coverage(true)),
            (Some(0), coverage(true).replace("v0.2.0", "v999.0.0")),
            (Some(0), coverage(true).replace("ci.yml", "other.yml")),
            (
                Some(0),
                coverage(true).replacen("\"valid\":true", "\"valid\":false,\"valid\":true", 1),
            ),
            (
                Some(0),
                json!({"cli_version":"v0.2.0", "valid":true, "findings":[], "workflows":[]})
                    .to_string(),
            ),
        ] {
            assert_eq!(
                classify(exit, &body, "", &files).0,
                NativeLockStatus::Unverified,
                "{exit:?}: {body}"
            );
        }
        let mut partial = files.clone();
        partial.push(".github/workflows/missing.yml".into());
        assert_eq!(
            classify(Some(0), &coverage(true), "", &partial).0,
            NativeLockStatus::Unverified
        );
        assert_eq!(
            classify(
                Some(0),
                &coverage(true),
                "opening lockfile: lockfile is unreadable: .github/workflows/actions.lock: line 1",
                &files
            )
            .0,
            NativeLockStatus::Unverified
        );
        let mut contradictory: serde_json::Value = serde_json::from_str(&coverage(false)).unwrap();
        contradictory["findings"] = json!([]);
        assert_eq!(
            classify(Some(1), &contradictory.to_string(), "", &files).0,
            NativeLockStatus::Unverified
        );
        contradictory["valid"] = json!(true);
        contradictory["workflows"][0]["valid"] = json!(true);
        assert_eq!(
            classify(Some(0), &contradictory.to_string(), "", &files).0,
            NativeLockStatus::Unverified
        );
    }

    #[test]
    fn verifier_child_has_no_tokens_or_subject_redirect_environment() {
        let cmd = command(Path::new("/verified/actions-lock"), Path::new("/checkout"));
        for key in [
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN",
            "GH_ACTIONS_LOCK_WORKFLOWS_DIR",
            "GH_ACTIONS_LOCK_REPO_ROOT",
        ] {
            assert!(cmd
                .get_envs()
                .any(|(name, value)| name == key && value.is_none()));
        }
    }
}
