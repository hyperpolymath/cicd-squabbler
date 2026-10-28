// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! REST access for the one question GraphQL cannot answer cleanly: **what is
//! this repository called now?**
//!
//! GraphQL is the estate's batched read path, but a repository query answers
//! under the requested `owner/name` and only *mentions* the canonical one in
//! `nameWithOwner`. The REST repository endpoint follows redirects, so the
//! response body's `full_name` is the canonical name — and a difference from
//! the requested name is a move. Comparing names (never pins) is the finding:
//! a moved owner is invisible to pin resolution, because the revision may be
//! perfectly valid at the new name while the coordinate still names the old
//! one.
//!
//! The transport is `gh api --include <path>`, the same binary and auth every
//! other forge read already uses. `--include` gives us the status line and
//! `Location` even for the error responses `gh` exits non-zero on, so a 404 is
//! distinguishable from "no JSON came back".
//!
//! Fail-closed: HTTP `404` maps to [`CanonicalResolution::NotFound`], anything
//! else that did not produce a name maps to
//! [`CanonicalResolution::Unverified`] with the reason. Nothing is inferred.
//! A redirect to `/repositories/<id>` (GitHub's form for renames) carries no
//! name at all, so it is `Unverified` — not a guess.

use serde_json::Value;
use squabble_core::chains::RepoId;
use squabble_core::startup::CanonicalResolution;
use std::collections::BTreeMap;
use std::process::{Command, Stdio};

/// One REST response's usable parts: status, an optional `Location`, and the
/// body. Headers other than `Location` are deliberately dropped — nothing here
/// needs them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestResponse {
    /// HTTP status code, or `0` when no status line was present.
    pub status: u16,
    /// The first `Location` header of the final response, if any.
    pub location: Option<String>,
    pub body: String,
}

/// Something that can GET an API path and return a [`RestResponse`].
///
/// `path` is relative to the API root, e.g. `repos/owner/name`. Implementors
/// must return `Ok` with a response for any HTTP status: a 404 is data, and
/// conflating it with a transport failure would erase the dead/moved
/// distinction. `Err` is only for "no response at all".
pub trait RestTransport {
    /// GET an API-root-relative path, retaining HTTP error responses as data.
    ///
    /// # Errors
    ///
    /// Returns an error when no response is available.
    fn get(&self, path: &str) -> Result<RestResponse, String>;
}

/// The characters an API path may contain.
///
/// This is not cosmetic. The path is passed to `gh` as an argument, so a value
/// beginning with `-` would be read as an option, and a newline would end `gh`'s
/// argv entry and let the next line be interpreted. Callers build the path from
/// repository slugs that come from workflow YAML and from the API itself, which
/// is to say from data this program does not control. `Owner`/`Repo`/`.`/`-`/`_`
/// and the separators are all a real path ever needs; everything else is
/// refused before a process is spawned.
const PATH_ALLOWED: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-/=@+~:";

/// Check an API path before it becomes an argument to `gh`.
///
/// # Errors
///
/// Returns the reason the path was refused: empty, too long, an option-looking
/// leading `-`, a `..` segment, or any byte outside [`PATH_ALLOWED`].
pub fn check_api_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("empty API path".to_string());
    }
    if path.len() > 512 {
        return Err(format!("API path is {} bytes; refusing", path.len()));
    }
    if path.starts_with('-') {
        return Err(format!("API path '{path}' looks like an option; refusing"));
    }
    for seg in path.split('/') {
        if seg == ".." {
            return Err(format!("API path '{path}' traverses upward; refusing"));
        }
    }
    for &b in path.as_bytes() {
        if !PATH_ALLOWED.contains(&b) {
            return Err(format!(
                "API path '{path}' contains byte 0x{b:02x}, which is not allowed; refusing"
            ));
        }
    }
    Ok(())
}

/// `gh api --include`, which prints headers and body.
#[derive(Debug, Default, Clone, Copy)]
pub struct GhRest;

impl RestTransport for GhRest {
    /// Read an API path through the authenticated `gh` CLI. Non-empty stdout
    /// is parsed even when `gh` exits unsuccessfully.
    ///
    /// # Errors
    ///
    /// Returns an error if `gh` cannot be run or stdout is empty or whitespace.
    fn get(&self, path: &str) -> Result<RestResponse, String> {
        // Validate before spawning: an argument is not a string once it is in
        // `gh`'s argv, and nothing below this line may run on a rejected path.
        check_api_path(path)?;
        let out = Command::new("gh")
            .args(["api", "--include", "--"])
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("failed to run `gh api --include {path}`: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout);
        if text.trim().is_empty() {
            // No headers at all: keep the diagnostic text without inferring
            // an HTTP status from it.
            return Err(format!(
                "`gh api --include {path}` exited {} with no response: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(parse_included(&text))
    }
}

/// Split `gh api --include` output into status, `Location` and body.
///
/// Only the *last* header block's status/headers are kept (a followed redirect
/// can print more than one); the body starts after that block's blank line.
/// Any line beginning `HTTP/` starts a new header block. A missing or invalid
/// status gives `0`; without a header-ending blank line, the body is empty.
pub fn parse_included(text: &str) -> RestResponse {
    let mut status = 0u16;
    let mut location = None;
    let mut in_headers = false;
    let mut body_offset = text.len();
    let mut offset = 0usize;

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if let Some(rest) = trimmed.strip_prefix("HTTP/") {
            // "2.0 200 OK" / "1.1 404 Not Found"
            status = rest
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            location = None;
            in_headers = true;
            body_offset = text.len();
        } else if in_headers && trimmed.is_empty() {
            in_headers = false;
            body_offset = offset + line.len();
        } else if in_headers {
            if let Some((name, value)) = trimmed.split_once(':') {
                if name.eq_ignore_ascii_case("location") && location.is_none() {
                    location = Some(value.trim().to_string());
                }
            }
        }
        offset += line.len();
    }

    RestResponse {
        status,
        location,
        body: text.get(body_offset..).unwrap_or("").to_string(),
    }
}

/// An `owner/name` from a repository URL, if the location carries one.
/// GitHub's rename redirect points at `/repositories/<id>`, which does not —
/// that case returns `None` so it cannot be mistaken for a canonical answer.
fn slug_from_location(location: &str) -> Option<String> {
    let idx = location.find("/repos/")?;
    let rest = &location[idx + "/repos/".len()..];
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let mut segs = rest.split('/').filter(|s| !s.is_empty());
    let (o, n) = (segs.next()?, segs.next()?);
    Some(format!("{o}/{n}"))
}

/// Turn one repository response into a canonical resolution for `requested`.
///
/// A 404 gives `NotFound`. Otherwise `full_name` takes precedence over a
/// `Location` naming a repo, and names are compared case-insensitively.
/// Without either name, returns `Unverified`; invalid JSON does not prevent
/// using a name from `Location`.
pub fn canonical_resolution(requested: &RepoId, resp: &RestResponse) -> CanonicalResolution {
    if resp.status == 404 {
        return CanonicalResolution::NotFound;
    }
    if let Ok(v) = serde_json::from_str::<Value>(resp.body.trim()) {
        if let Some(full) = v.get("full_name").and_then(Value::as_str) {
            return if RepoId::new(full) == *requested {
                CanonicalResolution::Canonical
            } else {
                CanonicalResolution::Moved {
                    canonical: full.to_string(),
                }
            };
        }
    }
    if let Some(name) = resp.location.as_deref().and_then(slug_from_location) {
        return if RepoId::new(name.as_str()) == *requested {
            CanonicalResolution::Canonical
        } else {
            CanonicalResolution::Moved { canonical: name }
        };
    }
    let reason = match resp.status {
        0 => "no HTTP status line in the response".to_string(),
        s if (200..300).contains(&s) => "response carried no `full_name`".to_string(),
        s => format!("HTTP {s} with no resolvable name"),
    };
    CanonicalResolution::Unverified { reason }
}

/// A transport error provides no HTTP response, so it concludes nothing.
fn resolution_from_error(err: &str) -> CanonicalResolution {
    CanonicalResolution::Unverified {
        reason: err.to_string(),
    }
}

/// Split a slug with exactly two non-empty slash-separated components;
/// return `None` for any other shape.
fn split_slug(repo: &RepoId) -> Option<(&str, &str)> {
    let (o, n) = repo.as_str().split_once('/')?;
    (!o.is_empty() && !n.is_empty() && !n.contains('/')).then_some((o, n))
}

/// Resolve each distinct valid `owner/name` slug with one REST read. Returns
/// resolutions keyed by the written name and the number of attempted calls.
/// Invalid slugs become `Unverified` without a call.
///
/// Transport errors become `Unverified` with their reason; only a parsed HTTP
/// `404` response becomes `NotFound`. Processing continues after either result.
pub fn resolve_canonical(
    transport: &dyn RestTransport,
    repos: &[RepoId],
) -> (BTreeMap<RepoId, CanonicalResolution>, u32) {
    let mut out = BTreeMap::new();
    let mut calls = 0u32;
    for repo in repos {
        if out.contains_key(repo) {
            continue;
        }
        let Some((owner, name)) = split_slug(repo) else {
            out.insert(
                repo.clone(),
                CanonicalResolution::Unverified {
                    reason: "not an `owner/name` slug".to_string(),
                },
            );
            continue;
        };
        calls += 1;
        let resolution = match transport.get(&format!("repos/{owner}/{name}")) {
            Ok(resp) => canonical_resolution(repo, &resp),
            Err(e) => resolution_from_error(&e),
        };
        out.insert(repo.clone(), resolution);
    }
    (out, calls)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> RepoId {
        RepoId::new(s)
    }

    fn ok_body(slug: &str) -> RestResponse {
        RestResponse {
            status: 200,
            location: None,
            body: format!("{{\"full_name\":\"{slug}\",\"id\":1}}"),
        }
    }

    #[test]
    fn headers_and_body_are_separated_at_the_blank_line() {
        let raw = "HTTP/2.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"full_name\":\"a/b\"}\n";
        let resp = parse_included(raw);
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body.trim(), "{\"full_name\":\"a/b\"}");
        assert!(resp.location.is_none());
    }

    #[test]
    fn a_location_header_is_kept_even_when_gh_exits_non_zero() {
        let raw = "HTTP/2.0 301 Moved Permanently\nLocation: https://api.github.com/repos/new/name\n\n";
        let resp = parse_included(raw);
        assert_eq!(resp.status, 301);
        assert_eq!(
            resp.location.as_deref(),
            Some("https://api.github.com/repos/new/name")
        );
        assert!(resp.body.is_empty());
    }

    #[test]
    fn only_the_last_header_block_counts() {
        // A followed redirect may print both hops; the final one decides.
        let raw = "HTTP/1.1 301 Moved Permanently\nLocation: https://api.github.com/repositories/9\n\n\
                   HTTP/2.0 200 OK\nLocation: https://api.github.com/repos/new/name\n\n{\"full_name\":\"new/name\"}";
        let resp = parse_included(raw);
        assert_eq!(resp.status, 200);
        assert_eq!(
            resp.location.as_deref(),
            Some("https://api.github.com/repos/new/name")
        );
        assert_eq!(resp.body, "{\"full_name\":\"new/name\"}");
    }

    #[test]
    fn a_same_name_is_canonical_in_any_case() {
        let resp = ok_body("Owner/Repo");
        assert_eq!(
            canonical_resolution(&r("owner/repo"), &resp),
            CanonicalResolution::Canonical
        );
    }

    #[test]
    fn a_different_full_name_is_a_move() {
        let resp = ok_body("hyperpolymath/standards");
        assert_eq!(
            canonical_resolution(&r("hyperpolymath/standards-old"), &resp),
            CanonicalResolution::Moved {
                canonical: "hyperpolymath/standards".into()
            }
        );
    }

    #[test]
    fn a_404_is_not_found_not_unverified() {
        assert_eq!(
            canonical_resolution(
                &r("gone/repo"),
                &RestResponse {
                    status: 404,
                    location: None,
                    body: "{\"message\":\"Not Found\"}".into(),
                }
            ),
            CanonicalResolution::NotFound
        );
    }

    #[test]
    fn a_redirect_location_naming_the_repo_is_a_move() {
        assert_eq!(
            canonical_resolution(
                &r("old/name"),
                &RestResponse {
                    status: 301,
                    location: Some("https://api.github.com/repos/new/name".into()),
                    body: String::new(),
                }
            ),
            CanonicalResolution::Moved {
                canonical: "new/name".into()
            }
        );
    }

    #[test]
    fn a_redirect_to_an_id_is_unverified_not_a_guess() {
        match canonical_resolution(
            &r("old/name"),
            &RestResponse {
                status: 301,
                location: Some("https://api.github.com/repositories/123".into()),
                body: String::new(),
            },
        ) {
            CanonicalResolution::Unverified { reason } => assert!(reason.contains("301")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_200_without_a_name_is_unverified() {
        assert!(matches!(
            canonical_resolution(
                &r("a/b"),
                &RestResponse {
                    status: 200,
                    location: None,
                    body: "{}".into()
                }
            ),
            CanonicalResolution::Unverified { .. }
        ));
    }

    struct Fake(Vec<(&'static str, Result<RestResponse, &'static str>)>);

    impl RestTransport for Fake {
        fn get(&self, path: &str) -> Result<RestResponse, String> {
            match self.0.iter().find(|(p, _)| *p == path) {
                Some((_, Ok(resp))) => Ok(resp.clone()),
                Some((_, Err(e))) => Err((*e).to_string()),
                None => Err(format!("unexpected path {path}")),
            }
        }
    }

    #[test]
    fn every_coordinate_is_resolved_once_and_failures_stay_unverified() {
        let rest = Fake(vec![
            ("repos/old/lib", Ok(ok_body("new/lib"))),
            (
                "repos/gone/repo",
                Ok(RestResponse {
                    status: 404,
                    location: None,
                    body: String::new(),
                }),
            ),
            ("repos/rate/limited", Err("gh: API rate limit exceeded")),
            (
                "repos/new/lib",
                Ok(RestResponse {
                    status: 200,
                    location: None,
                    body: "{\"full_name\":\"new/lib\"}".into(),
                }),
            ),
        ]);
        let (out, calls) = resolve_canonical(
            &rest,
            &[
                r("old/lib"),
                r("gone/repo"),
                r("rate/limited"),
                r("new/lib"),
                r("old/lib"), // duplicate: not re-called
            ],
        );
        assert_eq!(calls, 4);
        assert_eq!(
            out.get(&r("old/lib")),
            Some(&CanonicalResolution::Moved {
                canonical: "new/lib".into()
            })
        );
        assert_eq!(out.get(&r("gone/repo")), Some(&CanonicalResolution::NotFound));
        assert!(matches!(
            out.get(&r("rate/limited")),
            Some(CanonicalResolution::Unverified { .. })
        ));
        assert_eq!(
            out.get(&r("new/lib")),
            Some(&CanonicalResolution::Canonical)
        );
    }

    #[test]
    fn transport_errors_stay_unverified_even_when_they_mention_404_or_not_found() {
        for reason in [
            "gh: Not Found (HTTP 404)",
            "connection to proxy404 failed",
            "gh executable Not Found",
            "connection reset",
        ] {
            let rest = Fake(vec![("repos/a/b", Err(reason))]);
            let (out, _) = resolve_canonical(&rest, &[r("a/b")]);
            assert_eq!(
                out.get(&r("a/b")),
                Some(&CanonicalResolution::Unverified {
                    reason: reason.into()
                })
            );
        }
    }

    #[test]
    fn a_malformed_slug_concludes_nothing_and_costs_no_call() {
        let rest = Fake(vec![]);
        let (out, calls) = resolve_canonical(&rest, &[r("not-a-slug")]);
        assert_eq!(calls, 0);
        assert!(matches!(
            out.get(&r("not-a-slug")),
            Some(CanonicalResolution::Unverified { .. })
        ));
    }

    #[test]
    fn a_plain_repository_path_is_accepted() {
        assert!(check_api_path("repos/hyperpolymath/cicd-squabbler").is_ok());
        assert!(check_api_path("repos/a/b/commits/main").is_ok());
    }

    #[test]
    fn an_option_looking_path_is_refused_before_it_reaches_argv() {
        let err = check_api_path("--help").expect_err("must be refused");
        assert!(err.contains("option"), "{err}");
    }

    #[test]
    fn a_path_carrying_a_metacharacter_or_newline_is_refused() {
        for bad in [
            "repos/a/b;rm -rf /",
            "repos/a/b && gh pr merge 1",
            "repos/a/b\nx",
            "repos/a/b`id`",
            "repos/a/b$(id)",
            "repos/a/../..",
        ] {
            assert!(check_api_path(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn an_empty_or_overlong_path_is_refused() {
        assert!(check_api_path("").is_err());
        let long = format!("repos/{}", "a".repeat(600));
        assert!(check_api_path(&long).is_err());
    }
}
