// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Ecosystem-context reader — lets the squabbler "argue its case systemically".
//!
//! A fight is not just "which checks are red"; it is "what is this repo, what
//! does it coordinate with, and therefore who owns the reds it cannot win".
//! This module reads the target repo's `.machine_readable/` descriptiles
//! (a2ml) and distils them into a [`RepoContext`] the fight orchestration uses
//! to attribute owners and narrate the case.
//!
//! Two deliberate design choices keep this honest and lightweight:
//!
//! * **Host-only.** This lives in `squabble-cli`, never in `squabble-core` —
//!   the pure engine stays free of any estate/format dependency (detachability).
//! * **Fail-safe, not fail-open.** a2ml appears in more than one dialect across
//!   the estate, and a target repo may have no `.machine_readable/` at all. So
//!   this reader never errors: a missing file or unrecognised shape yields an
//!   empty field, and the fight falls back to ground-truthing the workflow
//!   source (which is the authoritative owner signal anyway). Context enriches
//!   the argument; it is never the sole basis for a move.

use std::path::Path;

/// The distilled ecosystem context of a repo under fight.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoContext {
    /// What the repo *is* (semantic authority statement, if declared).
    pub is: Vec<String>,
    /// What the repo *is not* (the `what-this-is-not` boundary list).
    pub is_not: Vec<String>,
    /// The repo this one coordinates with, normalised to `owner/repo`
    /// (e.g. `standards` → `hyperpolymath/standards`). Drives upstream owner
    /// attribution for reds produced by shared/reusable gates.
    pub coordination_owner: Option<String>,
    /// The declared golden-path command (e.g. `just ci`).
    pub golden_path: Option<String>,
    /// Languages/licences the repo bans (best-effort; e.g. `AGPL`, `Python`).
    pub banned_langs: Vec<String>,
    /// Declared estate/external relationships.
    pub relationships: Vec<String>,
    /// Whether any descriptile was actually found and read. `false` means the
    /// fight is proceeding on workflow ground-truth alone.
    pub found: bool,
}

impl RepoContext {
    /// Load context from a repo checkout. Never fails: absent files or
    /// unrecognised shapes leave fields empty and `found == false`.
    pub fn load(repo_root: &Path) -> Self {
        let desc = repo_root.join(".machine_readable/descriptiles");
        let read = |name: &str| std::fs::read_to_string(desc.join(name)).unwrap_or_default();
        let ecosystem = read("ECOSYSTEM.a2ml");
        let clade = read("CLADE.a2ml");
        let anchor = read("ANCHOR.a2ml");
        let agentic = read("AGENTIC.a2ml");
        let found = [&ecosystem, &clade, &anchor, &agentic]
            .iter()
            .any(|s| !s.is_empty());
        let mut ctx = Self::from_sources(&ecosystem, &clade, &anchor, &agentic);
        ctx.found = found;
        ctx
    }

    /// Build context from the raw a2ml texts. Pure and the unit of test
    /// coverage — [`load`](Self::load) only supplies these four strings.
    pub fn from_sources(ecosystem: &str, _clade: &str, anchor: &str, agentic: &str) -> Self {
        RepoContext {
            is: extract_scalar(anchor, "defines").into_iter().collect(),
            is_not: extract_array(ecosystem, "not"),
            coordination_owner: extract_scalar(ecosystem, "coordination").map(normalise_owner),
            golden_path: extract_scalar(anchor, "command"),
            banned_langs: extract_banned(agentic),
            relationships: extract_array(ecosystem, "related"),
            found: false,
        }
    }
}

/// Normalise a bare coordination name to `owner/repo`. Estate convention is that
/// a bare `standards` means `hyperpolymath/standards`; an already-qualified
/// `owner/repo` is left untouched.
fn normalise_owner(raw: String) -> String {
    if raw.contains('/') {
        raw
    } else {
        format!("hyperpolymath/{raw}")
    }
}

/// The candidate banned tokens the estate policy uses. Best-effort: a repo's
/// AGENTIC records these in prose/comments, so we scan for their presence on any
/// line that also signals a ban. This is narrative enrichment, never the basis
/// for a move.
const BANNED_CANDIDATES: &[&str] = &[
    "AGPL",
    "TypeScript",
    "Python",
    "Go",
    "Nix",
    "Node",
    "npm",
    "Ruby",
    "Perl",
    "Kotlin",
    "Swift",
];

fn extract_banned(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line;
        let ll = l.to_ascii_lowercase();
        let signals_ban = ll.contains("banned")
            || ll.contains("never use")
            || ll.contains("deny")
            || ll.contains("agpl");
        if !signals_ban {
            continue;
        }
        for cand in BANNED_CANDIDATES {
            if l.contains(cand) && !out.iter().any(|e: &String| e == cand) {
                out.push((*cand).to_string());
            }
        }
    }
    out
}

/// Extract the first double-quoted value on the first line whose trimmed start
/// is `key` immediately followed by `=` or `:`. Tolerant of TOML-like and
/// S-expr-ish a2ml alike.
fn extract_scalar(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix(key) {
            let rest = rest.trim_start();
            if rest.starts_with('=') || rest.starts_with(':') {
                let mut vals = Vec::new();
                push_quoted(line, &mut vals);
                if let Some(first) = vals.into_iter().next() {
                    return Some(first);
                }
            }
        }
    }
    None
}

/// Extract every double-quoted string in an array-valued key, whether the array
/// is written on one line or spread across several (`key = [ ... ]`).
fn extract_array(text: &str, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut collecting = false;
    for line in text.lines() {
        let t = line.trim();
        if !collecting {
            if let Some(rest) = t.strip_prefix(key) {
                let rest = rest.trim_start();
                if (rest.starts_with('=') || rest.starts_with(':')) && line.contains('[') {
                    collecting = true;
                    push_quoted(line, &mut out);
                    if line.contains(']') {
                        break;
                    }
                }
            }
        } else {
            push_quoted(line, &mut out);
            if t.contains(']') {
                break;
            }
        }
    }
    out
}

/// Push every double-quoted substring of `line` onto `out`.
fn push_quoted(line: &str, out: &mut Vec<String>) {
    let mut rest = line;
    while let Some(open) = rest.find('"') {
        let after = &rest[open + 1..];
        if let Some(close) = after.find('"') {
            out.push(after[..close].to_string());
            rest = &after[close + 1..];
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ECOSYSTEM: &str = r#"
[position]
pipeline = "tooling / CI-CD"
coordination = "standards"

[relations]
related = ["hyperpolymath/hypatia (the big system)", "hyperpolymath/gitbot-fleet"]

[what-this-is-not]
not = [
  "A bypass. Never enforce_admins=off, never drop a required context.",
  "hypatia-dependent. Detachability is the genesis.",
  "A red→green code-fixer (deferred past v0.1)."
]
"#;

    const ANCHOR: &str = r#"
[authority]
defines = "What it means to reach a CI/CD gate's green LEGITIMATELY (squabble != bypass)."

[golden-path]
command = "just ci"
"#;

    const AGENTIC: &str = r#"
[agent-constraints]
# - Never use banned languages (TypeScript, Python, Go, etc.)
# - Never use AGPL license (use MPL-2.0)
"#;

    #[test]
    fn extracts_coordination_owner_normalised() {
        let ctx = RepoContext::from_sources(ECOSYSTEM, "", ANCHOR, AGENTIC);
        assert_eq!(
            ctx.coordination_owner.as_deref(),
            Some("hyperpolymath/standards")
        );
    }

    #[test]
    fn extracts_multiline_is_not_array() {
        let ctx = RepoContext::from_sources(ECOSYSTEM, "", ANCHOR, AGENTIC);
        assert_eq!(ctx.is_not.len(), 3);
        assert!(ctx.is_not[1].contains("hypatia-dependent"));
    }

    #[test]
    fn extracts_relationships_single_line_array() {
        let ctx = RepoContext::from_sources(ECOSYSTEM, "", ANCHOR, AGENTIC);
        assert_eq!(ctx.relationships.len(), 2);
        assert!(ctx.relationships[0].contains("hypatia"));
    }

    #[test]
    fn extracts_golden_path_and_defines() {
        let ctx = RepoContext::from_sources(ECOSYSTEM, "", ANCHOR, AGENTIC);
        assert_eq!(ctx.golden_path.as_deref(), Some("just ci"));
        assert_eq!(ctx.is.len(), 1);
        assert!(ctx.is[0].contains("LEGITIMATELY"));
    }

    #[test]
    fn best_effort_banned_langs_include_agpl() {
        let ctx = RepoContext::from_sources(ECOSYSTEM, "", ANCHOR, AGENTIC);
        assert!(ctx.banned_langs.iter().any(|b| b == "AGPL"));
        assert!(ctx.banned_langs.iter().any(|b| b == "TypeScript"));
    }

    #[test]
    fn empty_sources_yield_empty_context_not_an_error() {
        let ctx = RepoContext::from_sources("", "", "", "");
        assert!(ctx.coordination_owner.is_none());
        assert!(ctx.is_not.is_empty());
        assert!(!ctx.found);
    }

    #[test]
    fn already_qualified_owner_is_untouched() {
        let eco = "coordination = \"hyperpolymath/custom-standards\"\n";
        let ctx = RepoContext::from_sources(eco, "", "", "");
        assert_eq!(
            ctx.coordination_owner.as_deref(),
            Some("hyperpolymath/custom-standards")
        );
    }
}
