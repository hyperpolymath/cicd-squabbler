// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Reads the `gate_triage` bot directive — the host half of the green-polarity
//! classifier.
//!
//! [`squabble_core::polarity`] is deliberately scanner-agnostic: it holds no
//! step names. They live in `.machine_readable/bot_directives/gate_triage.a2ml`
//! under `signature-skipped-steps` / `signature-success-steps`, and this module
//! is what turns that file into a [`VacuitySignature`].
//!
//! Fail-safe like [`crate::context`]: an absent or unrecognised directive
//! yields an *unusable* signature, and an unusable signature matches nothing.
//! A missing directive therefore means "detect no vacuity", never "detect
//! vacuity everywhere".

use squabble_core::polarity::VacuitySignature;
use std::path::Path;

use crate::context::extract_array;

/// Where the directive lives, relative to a repo checkout.
pub const DIRECTIVE_PATH: &str = ".machine_readable/bot_directives/gate_triage.a2ml";

/// Load the vacuity signature from a repo checkout. Never fails.
pub fn load_signature(repo_root: &Path) -> VacuitySignature {
    let raw = std::fs::read_to_string(repo_root.join(DIRECTIVE_PATH)).unwrap_or_default();
    parse_signature(&raw)
}

/// Pure half — the unit of test coverage. [`load_signature`] only supplies the
/// file's text.
pub fn parse_signature(raw: &str) -> VacuitySignature {
    VacuitySignature {
        skipped_steps: extract_array(raw, "signature-skipped-steps"),
        success_steps: extract_array(raw, "signature-success-steps"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIRECTIVE: &str = r#"
[gate-triage.detection]
source = "actions jobs API step conclusions"
signature-skipped-steps = ["Run Hypatia scan"]
signature-success-steps = ["Create stub findings"]
check-conclusion-when-vacuous = "success"
evidence-required = ["run-count", "stub-rate", "upstream-exists", "target-tech-present"]
"#;

    #[test]
    fn both_signature_halves_are_read_from_the_directive() {
        let sig = parse_signature(DIRECTIVE);
        assert_eq!(sig.skipped_steps, vec!["Run Hypatia scan".to_string()]);
        assert_eq!(sig.success_steps, vec!["Create stub findings".to_string()]);
        assert!(sig.is_usable());
    }

    #[test]
    fn the_two_keys_do_not_bleed_into_each_other() {
        // `signature-skipped-steps` and `signature-success-steps` share a long
        // prefix; a sloppy prefix match would merge them.
        let sig = parse_signature(DIRECTIVE);
        assert!(!sig.skipped_steps.contains(&"Create stub findings".to_string()));
        assert!(!sig.success_steps.contains(&"Run Hypatia scan".to_string()));
    }

    #[test]
    fn an_absent_directive_detects_nothing() {
        // Fail-safe: no directive must mean "detect no vacuity", never
        // "detect vacuity everywhere".
        let sig = parse_signature("");
        assert!(!sig.is_usable());
    }

    #[test]
    fn a_missing_file_yields_an_unusable_signature() {
        let sig = load_signature(Path::new("/nonexistent-repo-root"));
        assert!(!sig.is_usable());
    }

    #[test]
    fn the_repos_own_directive_parses() {
        // Ground-truth against the real file rather than only a fixture.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root");
        let sig = load_signature(root);
        assert!(
            sig.is_usable(),
            "this repo's own {DIRECTIVE_PATH} must parse into a usable signature"
        );
    }
}
