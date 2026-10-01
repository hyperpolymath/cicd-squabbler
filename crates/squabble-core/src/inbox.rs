// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `inbox` — which notification threads are finished, pure.
//!
//! A thread whose subject (a pull request or an issue) is merged or closed
//! asks nothing more of anyone: `squabble inbox-sweep` unsubscribes from it
//! and marks it done. Every other thread is kept, with the reason, so the
//! inbox ends up holding only what is still live.
//!
//! Nothing here is guessed. A subject whose state could not be read is kept;
//! a subject type the sweep does not understand (a release, a check suite, a
//! discussion) is kept. Unsubscribing is reversible — GitHub re-subscribes
//! on a new mention or review request — but a wrongly cleared thread is an
//! item the owner never sees, so the default is always "keep".

use crate::chains::RepoId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One notification thread, as the REST `notifications` endpoint lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub reason: String,
    /// `subject.type`: `PullRequest`, `Issue`, `Release`, `CheckSuite`, …
    pub subject_type: String,
    pub repo: RepoId,
    /// `subject.url`, an API URL; `None` for subjects without one.
    pub subject_url: Option<String>,
    pub unread: bool,
    pub updated_at: String,
}

/// The issue or pull request a thread is about.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SubjectRef {
    pub repo: RepoId,
    pub number: u64,
}

impl SubjectRef {
    /// Parse `https://api.github.com/repos/{o}/{r}/(pulls|issues)/{n}`.
    /// Anything else — a different host, a commit URL, a release — is `None`.
    pub fn from_api_url(url: &str) -> Option<Self> {
        let rest = url.strip_prefix("https://api.github.com/repos/")?;
        let parts: Vec<&str> = rest.split('/').collect();
        let [owner, name, kind, number] = parts.as_slice() else {
            return None;
        };
        if owner.is_empty() || name.is_empty() || !matches!(*kind, "pulls" | "issues") {
            return None;
        }
        Some(Self {
            repo: RepoId::new(format!("{owner}/{name}")),
            number: number.parse().ok()?,
        })
    }
}

/// A subject's state as the forge reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubjectState {
    Open,
    Merged,
    Closed,
    /// Could not be read; the reason is carried, and the thread is kept.
    Unknown(String),
}

/// What to do with one thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Verdict {
    /// Unsubscribe, then mark done.
    ClearMerged,
    ClearClosed,
    KeepOpen,
    KeepUnknownState,
    KeepUnsupportedSubject,
}

impl Verdict {
    /// True for the verdicts that write.
    pub fn clears(self) -> bool {
        matches!(self, Verdict::ClearMerged | Verdict::ClearClosed)
    }

    /// One-line label for reports.
    pub fn label(self) -> &'static str {
        match self {
            Verdict::ClearMerged => "clear: subject merged",
            Verdict::ClearClosed => "clear: subject closed",
            Verdict::KeepOpen => "keep: subject still open",
            Verdict::KeepUnknownState => "keep: subject state could not be read",
            Verdict::KeepUnsupportedSubject => "keep: not a pull request or issue",
        }
    }
}

/// The subject a thread refers to, when the sweep understands it.
pub fn subject_of(t: &Thread) -> Option<SubjectRef> {
    if !matches!(t.subject_type.as_str(), "PullRequest" | "Issue") {
        return None;
    }
    SubjectRef::from_api_url(t.subject_url.as_deref()?)
}

/// Decide one thread given the state lookup (`None` = not looked up).
pub fn decide(t: &Thread, state: Option<&SubjectState>) -> Verdict {
    if subject_of(t).is_none() {
        return Verdict::KeepUnsupportedSubject;
    }
    match state {
        Some(SubjectState::Merged) => Verdict::ClearMerged,
        Some(SubjectState::Closed) => Verdict::ClearClosed,
        Some(SubjectState::Open) => Verdict::KeepOpen,
        Some(SubjectState::Unknown(_)) | None => Verdict::KeepUnknownState,
    }
}

/// Count verdicts, for the before-anything-is-written summary.
pub fn tally(verdicts: &[Verdict]) -> BTreeMap<Verdict, usize> {
    let mut m = BTreeMap::new();
    for v in verdicts {
        *m.entry(*v).or_insert(0) += 1;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(kind: &str, url: Option<&str>) -> Thread {
        Thread {
            id: "1".into(),
            reason: "author".into(),
            subject_type: kind.into(),
            repo: RepoId::new("o/r"),
            subject_url: url.map(str::to_string),
            unread: false,
            updated_at: "2026-10-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn subject_urls_parse_and_reject() {
        let s = SubjectRef::from_api_url("https://api.github.com/repos/o/r/pulls/12").unwrap();
        assert_eq!((s.repo.as_str(), s.number), ("o/r", 12));
        assert!(SubjectRef::from_api_url("https://api.github.com/repos/o/r/issues/3").is_some());
        for bad in [
            "https://api.github.com/repos/o/r/commits/abc",
            "https://api.github.com/repos/o/r/pulls/x",
            "https://api.github.com/repos/o/r/pulls/1/extra",
            "https://evil.example/repos/o/r/pulls/1",
            "https://api.github.com/repos//r/pulls/1",
        ] {
            assert_eq!(SubjectRef::from_api_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_resolved_subjects_clear() {
        let t = thread(
            "PullRequest",
            Some("https://api.github.com/repos/o/r/pulls/1"),
        );
        assert_eq!(
            decide(&t, Some(&SubjectState::Merged)),
            Verdict::ClearMerged
        );
        assert_eq!(
            decide(&t, Some(&SubjectState::Closed)),
            Verdict::ClearClosed
        );
        assert_eq!(decide(&t, Some(&SubjectState::Open)), Verdict::KeepOpen);
        assert!(!Verdict::KeepOpen.clears());
    }

    #[test]
    fn unread_state_and_unknown_kinds_are_kept() {
        let t = thread(
            "PullRequest",
            Some("https://api.github.com/repos/o/r/pulls/1"),
        );
        assert_eq!(decide(&t, None), Verdict::KeepUnknownState);
        assert_eq!(
            decide(&t, Some(&SubjectState::Unknown("502".into()))),
            Verdict::KeepUnknownState
        );
        let rel = thread(
            "Release",
            Some("https://api.github.com/repos/o/r/releases/9"),
        );
        assert_eq!(
            decide(&rel, Some(&SubjectState::Closed)),
            Verdict::KeepUnsupportedSubject
        );
        let none = thread("PullRequest", None);
        assert_eq!(
            decide(&none, Some(&SubjectState::Merged)),
            Verdict::KeepUnsupportedSubject
        );
    }

    #[test]
    fn a_pull_request_thread_with_an_issue_url_is_still_understood() {
        // GitHub sometimes points PR threads at the issues endpoint.
        let t = thread(
            "PullRequest",
            Some("https://api.github.com/repos/o/r/issues/5"),
        );
        assert_eq!(subject_of(&t).unwrap().number, 5);
    }

    #[test]
    fn tally_counts_each_verdict() {
        let m = tally(&[
            Verdict::ClearMerged,
            Verdict::ClearMerged,
            Verdict::KeepOpen,
        ]);
        assert_eq!(m[&Verdict::ClearMerged], 2);
        assert_eq!(m[&Verdict::KeepOpen], 1);
    }
}
