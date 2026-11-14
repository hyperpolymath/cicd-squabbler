// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Evidence for `squabble relock --gather`: ask an action's own repository
//! which tags point at a SHA and, failing that, which branches contain it.
//!
//! Read-only: it only runs `git ls-remote`, `git init --bare` in a private
//! temporary directory, and a blob-less `git fetch` into it. It never writes
//! to the repository it asks and never to the lock.

use squabble_core::lockfix::RefEvidence;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

/// Run git, return stdout. Any non-zero exit is an error that names the
/// command's stderr, so a failed lookup is loud.
fn git(dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let out = cmd.output().map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("git {} failed: {}", args.join(" "), err.trim()))
    }
}

/// Split `owner/repo@<sha>` into the repository path and the SHA.
fn split_key(key: &str) -> Result<(&str, &str), String> {
    key.split_once('@')
        .filter(|(repo, _)| repo.matches('/').count() == 1)
        .ok_or_else(|| format!("`{key}` is not owner/repo@sha"))
}

/// The tags whose commit is `sha`, from `git ls-remote --tags` output. An
/// annotated tag's commit is its peeled `^{}` line, a lightweight tag's is its
/// own line.
fn tags_from_ls_remote(output: &str, sha: &str) -> Vec<String> {
    let mut commit_of: Vec<(String, String)> = Vec::new();
    for line in output.lines() {
        let Some((id, name)) = line.split_once('\t') else {
            continue;
        };
        let Some(tag) = name.strip_prefix("refs/tags/") else {
            continue;
        };
        match tag.strip_suffix("^{}") {
            Some(base) => match commit_of.iter_mut().find(|(n, _)| n == base) {
                Some(entry) => entry.1 = id.to_string(),
                None => commit_of.push((base.to_string(), id.to_string())),
            },
            None if !commit_of.iter().any(|(n, _)| n == tag) => {
                commit_of.push((tag.to_string(), id.to_string()));
            }
            None => {}
        }
    }
    let mut tags: Vec<String> = commit_of
        .into_iter()
        .filter(|(_, id)| id.eq_ignore_ascii_case(sha))
        .map(|(n, _)| n)
        .collect();
    tags.sort();
    tags
}

/// The default branch from `git ls-remote --symref <url> HEAD`.
fn default_branch(url: &str) -> Option<String> {
    let out = git(None, &["ls-remote", "--symref", url, "HEAD"]).ok()?;
    out.lines()
        .find_map(|l| l.strip_prefix("ref: refs/heads/"))
        .and_then(|l| l.split_whitespace().next())
        .map(String::from)
}

/// A temporary directory removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self, String> {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "squabble-gather-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
        Ok(TempDir(p))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The branches of `url` that contain `sha`: every branch's history without
/// blobs, then `for-each-ref --contains`. An unknown commit (not reachable
/// from any branch) is no branches, not an error.
fn branches_containing(url: &str, sha: &str) -> Result<Vec<String>, String> {
    let tmp = TempDir::new()?;
    let dir = tmp.0.as_path();
    git(Some(dir), &["init", "--bare", "-q"])?;
    git(
        Some(dir),
        &[
            "fetch",
            "-q",
            "--no-tags",
            "--filter=blob:none",
            url,
            "+refs/heads/*:refs/heads/*",
        ],
    )?;
    if git(Some(dir), &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_err() {
        return Ok(Vec::new());
    }
    let out = git(
        Some(dir),
        &[
            "for-each-ref",
            "--contains",
            sha,
            "--format=%(refname:short)",
            "refs/heads",
        ],
    )?;
    Ok(out.lines().map(String::from).collect())
}

/// Gather the evidence for one record key against `<base>/<owner>/<repo>`.
/// Branches are asked only when no tag holds the commit.
pub fn gather(base: &str, key: &str) -> Result<RefEvidence, String> {
    let (repo, sha) = split_key(key)?;
    let url = format!("{}/{repo}", base.trim_end_matches('/'));
    let tags = tags_from_ls_remote(&git(None, &["ls-remote", "--tags", &url])?, sha);
    let branches = if tags.is_empty() {
        branches_containing(&url, sha)?
    } else {
        Vec::new()
    };
    Ok(RefEvidence {
        tags,
        branches,
        default_branch: default_branch(&url),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn ls_remote_tags_lightweight_annotated_and_others() {
        let out = format!(
            "{A}\trefs/tags/v1.0.0\n{B}\trefs/tags/v2.0.0\n{A}\trefs/tags/v2.0.0^{{}}\n{B}\trefs/tags/other\nnot a line\n{B}\trefs/heads/main\n"
        );
        // v1.0.0 is lightweight at A; v2.0.0 is annotated and its peeled commit is A.
        assert_eq!(tags_from_ls_remote(&out, A), vec!["v1.0.0", "v2.0.0"]);
        assert_eq!(tags_from_ls_remote(&out, B), vec!["other"]);
        assert!(tags_from_ls_remote(&out, "cccccccccccccccccccccccccccccccccccccccc").is_empty());
    }

    #[test]
    fn keys_must_be_owner_repo_at_sha() {
        assert_eq!(split_key("a/b@abc").unwrap(), ("a/b", "abc"));
        for bad in ["a@abc", "a/b/c@abc", "nothing"] {
            assert!(split_key(bad).is_err(), "{bad}");
        }
    }

    /// A local "remote": `<base>/o/r` holding one commit on main, an annotated
    /// tag v1.0.0 on it, a second commit on main, and a branch `topic` at the
    /// second commit. Returns (base dir, first sha, second sha).
    fn fixture() -> (TempDir, String, String) {
        let base = TempDir::new().unwrap();
        let repo = base.0.join("o").join("r");
        std::fs::create_dir_all(&repo).unwrap();
        let g = |args: &[&str]| git(Some(&repo), args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@example.invalid"]);
        g(&["config", "user.name", "t"]);
        g(&["config", "commit.gpgsign", "false"]);
        g(&["commit", "-q", "--allow-empty", "-m", "one"]);
        let first = g(&["rev-parse", "HEAD"]).trim().to_string();
        g(&["tag", "-a", "v1.0.0", "-m", "v1"]);
        g(&["commit", "-q", "--allow-empty", "-m", "two"]);
        let second = g(&["rev-parse", "HEAD"]).trim().to_string();
        g(&["branch", "topic"]);
        (base, first, second)
    }

    fn base_of(t: &TempDir) -> String {
        t.0.to_string_lossy().into_owned()
    }

    #[test]
    fn an_annotated_tag_is_found_by_its_peeled_commit_and_branches_are_not_asked() {
        let (base, first, _) = fixture();
        let e = gather(&base_of(&base), &format!("o/r@{first}")).unwrap();
        assert_eq!(e.tags, vec!["v1.0.0"]);
        assert!(e.branches.is_empty());
        assert_eq!(e.default_branch.as_deref(), Some("main"));
    }

    #[test]
    fn without_a_tag_the_branches_that_contain_the_commit_are_named() {
        let (base, _, second) = fixture();
        let e = gather(&base_of(&base), &format!("o/r@{second}")).unwrap();
        assert!(e.tags.is_empty());
        assert_eq!(e.branches, vec!["main", "topic"]);
        assert_eq!(e.default_branch.as_deref(), Some("main"));
    }

    #[test]
    fn a_commit_no_branch_holds_is_no_evidence_not_an_error() {
        let (base, _, _) = fixture();
        let e = gather(&base_of(&base), &format!("o/r@{B}")).unwrap();
        assert!(e.tags.is_empty() && e.branches.is_empty());
    }

    #[test]
    fn an_unreachable_remote_is_an_error() {
        let base = TempDir::new().unwrap();
        assert!(gather(&base_of(&base), &format!("nope/none@{A}")).is_err());
    }
}
