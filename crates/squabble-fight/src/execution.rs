// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Reads the `execution_taxonomy` bot directive — the host half of
//! [`squabble_core::execution`].
//!
//! [`squabble_core::execution`] is deliberately vocabulary-free: it holds no
//! step names. They live in
//! `.machine_readable/bot_directives/execution_taxonomy.a2ml`, in
//! `[execution-taxonomy]`, and this module is what turns that file into a
//! [`PhaseSignatures`].
//!
//! Fail-safe, exactly like [`crate::gate_triage`] and [`crate::context`]: an
//! absent, unreadable or unrecognised directive yields an *unusable* signature
//! set, and an unusable set attributes no preflight class. A missing directive
//! therefore means "claim no preflight cause", never "everything is preflight".
//! The classifier still reports GitHub's implicit `Set up job`, which is a
//! GitHub primitive rather than a repo declaration.

use squabble_core::execution::PhaseSignatures;
use std::path::Path;

use crate::context::extract_array;

/// Where the directive lives, relative to a repo checkout.
pub const DIRECTIVE_PATH: &str = ".machine_readable/bot_directives/execution_taxonomy.a2ml";

/// The section that carries the step-name signatures.
pub const SECTION: &str = "[execution-taxonomy]";

/// Load the phase signatures from a repo checkout. Never fails.
pub fn load_signatures(repo_root: &Path) -> PhaseSignatures {
    let raw = std::fs::read_to_string(repo_root.join(DIRECTIVE_PATH)).unwrap_or_default();
    parse_signatures(&raw)
}

/// Pure half — the unit of test coverage. [`load_signatures`] only supplies the
/// file's text.
///
/// Scoped to [`SECTION`] on purpose: `extract_array` is line-oriented and would
/// otherwise read a `setup-steps` key written as an example inside any other
/// section. A table or section header closes the scope, so a following section
/// cannot donate its arrays to this one.
pub fn parse_signatures(raw: &str) -> PhaseSignatures {
    let section = section_of(raw, SECTION);
    PhaseSignatures {
        setup_steps: extract_array(section, "setup-steps"),
        prerequisite_steps: extract_array(section, "prerequisite-steps"),
    }
}

/// The text of one `[section]`, from its header to the next header.
///
/// Returns an empty string when the header is absent — an absent section is an
/// absent declaration, and returning the whole file would let an example line
/// elsewhere become policy.
fn section_of<'a>(raw: &'a str, header: &str) -> &'a str {
    let Some(i) = raw.find(header) else {
        return "";
    };
    let rest = &raw[i + header.len()..];
    let mut end = rest.len();
    let mut at = 0usize;
    for line in rest.split_inclusive('\n') {
        let code = line.split('#').next().unwrap_or("").trim();
        if code.starts_with('[') {
            end = at;
            break;
        }
        at += line.len();
    }
    &rest[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIRECTIVE: &str = r#"
[metadata]
version = "0.1.0"

[execution-taxonomy]
# setup-steps = ["A commented example that must not be policy"]
setup-steps = [
  "Set up job",
  "Checkout repository",
  "Install Rust toolchain",
]
prerequisite-steps = ["Probe whether fight can inspect the full gate"]

[execution-taxonomy.notes]
setup-steps = ["this belongs to a different section and must not be read"]
"#;

    #[test]
    fn parses_the_declared_lists_and_ignores_the_gaps() {
        let s = parse_signatures(DIRECTIVE);
        assert_eq!(
            s.setup_steps,
            vec![
                "Set up job".to_string(),
                "Checkout repository".to_string(),
                "Install Rust toolchain".to_string(),
            ]
        );
        assert_eq!(
            s.prerequisite_steps,
            vec!["Probe whether fight can inspect the full gate".to_string()]
        );
        assert!(s.is_usable());
    }

    #[test]
    fn a_subsection_cannot_donate_its_keys() {
        // The trailing `[execution-taxonomy.notes]` redefines `setup-steps`.
        // Section scoping is what keeps that out.
        let s = parse_signatures(DIRECTIVE);
        assert!(!s.setup_steps.iter().any(|x| x.contains("different section")));
    }

    #[test]
    fn an_absent_directive_is_unusable_not_permissive() {
        let s = parse_signatures("");
        assert!(!s.is_usable());
        assert!(s.setup_steps.is_empty() && s.prerequisite_steps.is_empty());
        // Unusable means "claim no preflight class", which is the fail-closed
        // answer — not "everything is preflight".
        assert!(!s.is_setup("Install Rust toolchain"));
        // GitHub's implicit step is a primitive, not a declaration, so it is
        // still recognised.
        assert!(s.is_setup("Set up job"));
    }

    #[test]
    fn a_comment_only_declaration_declares_nothing() {
        let s = parse_signatures(
            "[execution-taxonomy]\n# setup-steps = [\"Install Rust toolchain\"]\n",
        );
        assert!(!s.is_usable());
        assert!(!s.is_setup("Install Rust toolchain"));
    }

    #[test]
    fn a_missing_repo_root_loads_to_the_unusable_set() {
        let s = load_signatures(Path::new("/nonexistent-repo-root"));
        assert!(!s.is_usable());
    }

    #[test]
    fn this_repos_own_directive_names_only_steps_that_exist_in_this_estate() {
        // Ground truth, re-read on every run: the shipped directive must parse,
        // or the classifier silently degrades to "policy step" for everything.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let s = load_signatures(root);
        assert!(
            s.is_usable(),
            "{} must declare at least one step name",
            DIRECTIVE_PATH
        );
        // `Set up job` is compiled in, so declaring it adds nothing; the
        // directive's job is the repo-specific names.
        assert!(s
            .setup_steps
            .iter()
            .chain(s.prerequisite_steps.iter())
            .any(|n| n != "Set up job"));
    }
}
