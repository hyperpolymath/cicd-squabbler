// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! Reads the `gate_triage` bot directive — the host half of the green-polarity
//! classifier.
//!
//! [`squabble_core::polarity`] is deliberately scanner-agnostic: it holds no
//! step names. They live in `.machine_readable/bot_directives/gate_triage.a2ml`
//! in `[[gate-triage.detection.signatures]]` tables, one per scanner, and this
//! module is what turns that file into a [`SignatureSet`].
//!
//! The same file supplies axis 0: the gate's applicability predicate from
//! `[gate-triage.applicability]`, and the repo's own declaration from the
//! manifest that section names.
//!
//! Fail-safe like [`crate::context`]: an absent or unrecognised directive
//! yields an *unusable* signature set, and an unusable set matches nothing.
//! A missing directive therefore means "detect no vacuity", never "detect
//! vacuity everywhere". An absent applicability predicate likewise means
//! "applies here", never "applies nowhere".

use squabble_core::polarity::{
    Applicability, RepoDeclaration, ScannerSignature, SignatureSet, VacuitySignature,
};
use std::path::Path;

use crate::context::extract_array;

/// Where the directive lives, relative to a repo checkout.
pub const DIRECTIVE_PATH: &str = ".machine_readable/bot_directives/gate_triage.a2ml";

/// The array-of-tables header that introduces one scanner's signature.
pub const SIGNATURE_TABLE: &str = "[[gate-triage.detection.signatures]]";

/// Manifest consulted for the repo's own declaration when the directive names
/// none. Ground-truthed against this repo on 2026-09-09; the directive used to
/// name a `0.1-` file that has never existed here.
pub const DEFAULT_MANIFEST: &str = "0-AI-MANIFEST.a2ml";

/// Load every scanner signature from a repo checkout. Never fails.
pub fn load_signatures(repo_root: &Path) -> SignatureSet {
    parse_signatures(&read(repo_root, DIRECTIVE_PATH))
}

/// Pure half — the unit of test coverage. [`load_signatures`] only supplies
/// the file's text.
///
/// One [`SIGNATURE_TABLE`] header opens one scanner. A chunk ends at the next
/// header of any kind, so a following `[section]` cannot donate its arrays to
/// the last table.
///
/// A signature that is not *usable* (either half empty) is dropped rather than
/// carried: it would match nothing anyway, and dropping it keeps
/// `is_usable()` on the set honest about how many scanners are really covered.
pub fn parse_signatures(raw: &str) -> SignatureSet {
    let mut out = Vec::new();
    for chunk in raw.split(SIGNATURE_TABLE).skip(1) {
        let body = until_next_header(chunk);
        let signature = VacuitySignature {
            skipped_steps: extract_array(body, "skipped-steps"),
            success_steps: extract_array(body, "success-steps"),
        };
        if !signature.is_usable() {
            continue;
        }
        out.push(ScannerSignature {
            scanner: extract_scalar(body, "scanner").unwrap_or_else(|| "unnamed".to_string()),
            signature,
        });
    }

    if out.is_empty() {
        // Legacy fallback: directives at version 0.2.0 carried a single flat
        // pair of keys and no tables. Repos across the estate still ship that
        // copy, and `fight` reads the directive of the repo it is fighting —
        // so without this, upgrading the host would SILENTLY stop detecting
        // hypatia vacuity on every unmigrated repo. It cannot double-parse:
        // this repo's own directive no longer carries the flat keys, and the
        // branch is only reached when no table was found at all.
        let legacy = VacuitySignature {
            skipped_steps: extract_array(raw, "signature-skipped-steps"),
            success_steps: extract_array(raw, "signature-success-steps"),
        };
        if legacy.is_usable() {
            return SignatureSet::single("legacy-flat-keys", legacy);
        }
    }

    SignatureSet::new(out)
}

/// Load the gate's applicability predicate (axis 0). Never fails.
///
/// Absent keys yield [`Applicability::default`], which `applicability_verdict`
/// short-circuits on — so "no predicate" means "applies here". Inapplicable is
/// a claim, and an absent claim must not be manufactured.
pub fn load_applicability(repo_root: &Path) -> Applicability {
    parse_applicability(&read(repo_root, DIRECTIVE_PATH))
}

/// Pure half of [`load_applicability`].
pub fn parse_applicability(raw: &str) -> Applicability {
    let section = section_of(raw, "[gate-triage.applicability]");
    Applicability {
        runs_for_operator_types: extract_array(section, "runs-for-operator-types"),
        runs_on_channels: extract_array(section, "runs-on-channels"),
    }
}

/// Load the repo's own declaration from the manifest the directive names.
///
/// Never fails, and never guesses: an absent manifest, an absent key or a
/// manifest with no `@` keys all yield `None` fields, and a `None` field is
/// skipped by `applicability_verdict` rather than treated as a mismatch.
pub fn load_declaration(repo_root: &Path) -> RepoDeclaration {
    let directive = read(repo_root, DIRECTIVE_PATH);
    let section = section_of(&directive, "[gate-triage.applicability]");
    let manifest_name =
        extract_scalar(section, "manifest-file").unwrap_or_else(|| DEFAULT_MANIFEST.to_string());
    let op_key = extract_scalar(section, "operator-type-key")
        .unwrap_or_else(|| "@gitforge_OperatorType".to_string());
    let ch_key = extract_scalar(section, "channel-key").unwrap_or_else(|| "@channel".to_string());

    let manifest = read(repo_root, &manifest_name);
    RepoDeclaration {
        operator_type: extract_scalar(&manifest, &op_key),
        channel: extract_scalar(&manifest, &ch_key),
    }
}

fn read(repo_root: &Path, rel: &str) -> String {
    std::fs::read_to_string(repo_root.join(rel)).unwrap_or_default()
}

/// Truncate `chunk` at the next line that opens a table or section, so one
/// table's keys cannot be read out of the next one.
fn until_next_header(chunk: &str) -> &str {
    let mut end = chunk.len();
    let mut at = 0usize;
    for line in chunk.split_inclusive('\n') {
        if line.trim_start().starts_with('[') {
            end = at;
            break;
        }
        at += line.len();
    }
    &chunk[..end]
}

/// The text of one `[section]`, from its header to the next header.
///
/// Returns the whole input when the header is absent, which keeps a
/// header-less test fixture usable; every caller's keys are distinctive
/// enough that a whole-file search is not a false-positive risk.
fn section_of<'a>(raw: &'a str, header: &str) -> &'a str {
    match raw.find(header) {
        Some(i) => {
            let rest = &raw[i + header.len()..];
            let body_end = until_next_header(rest).len();
            &rest[..body_end]
        }
        None => raw,
    }
}

/// Read a `key = "value"` scalar. Commented lines are skipped: the directive
/// carries `#   runs-on-channels = ["alpha"]` as a worked example, and a
/// parser that read its own documentation would be its own fake green.
fn extract_scalar(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            continue;
        }
        let Some(rest) = t.strip_prefix(key) else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=').or_else(|| rest.strip_prefix(':')) else {
            continue;
        };
        let rest = rest.trim_start();
        if let Some(open) = rest.strip_prefix('"') {
            if let Some(close) = open.find('"') {
                return Some(open[..close].to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use squabble_core::polarity::{StepConclusion, StepOutcome};

    /// Two scanners, written the way the real directive writes them.
    const DIRECTIVE: &str = r#"
[gate-triage.detection]
source = "actions jobs API step conclusions"
check-conclusion-when-vacuous = "success"

[[gate-triage.detection.signatures]]
scanner = "hypatia"
skipped-steps = ["Run Hypatia scan"]
success-steps = ["Create stub findings (when Hypatia unavailable)"]
census = "33/33"

[[gate-triage.detection.signatures]]
scanner = "panic-attack"
skipped-steps = ["Run panic-attack assail"]
success-steps = ["Create stub findings (when panic-attack unavailable)"]
census = "33/33"
"#;

    fn workspace_root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
    }

    fn step(name: &str, c: StepConclusion) -> StepOutcome {
        StepOutcome {
            name: name.to_string(),
            conclusion: c,
        }
    }

    #[test]
    fn every_table_becomes_its_own_scanner() {
        let set = parse_signatures(DIRECTIVE);
        assert_eq!(set.signatures.len(), 2, "one table, one scanner");
        assert_eq!(set.signatures[0].scanner, "hypatia");
        assert_eq!(set.signatures[1].scanner, "panic-attack");
        assert!(set.is_usable());
    }

    #[test]
    fn the_two_step_keys_do_not_bleed_into_each_other() {
        // `skipped-steps` and `success-steps` are read from the same chunk; a
        // sloppy substring match would merge them.
        let set = parse_signatures(DIRECTIVE);
        let h = &set.signatures[0].signature;
        assert_eq!(h.skipped_steps, vec!["Run Hypatia scan".to_string()]);
        assert_eq!(
            h.success_steps,
            vec!["Create stub findings (when Hypatia unavailable)".to_string()]
        );
    }

    #[test]
    fn one_tables_steps_never_leak_into_the_next() {
        // The whole point of chunking. If the parser searched the whole file
        // per key, every scanner would get hypatia's steps.
        let set = parse_signatures(DIRECTIVE);
        let pa = &set.signatures[1].signature;
        assert_eq!(pa.skipped_steps, vec!["Run panic-attack assail".to_string()]);
        assert!(
            !pa.skipped_steps.contains(&"Run Hypatia scan".to_string()),
            "panic-attack must not inherit hypatia's steps"
        );
    }

    #[test]
    fn a_scanner_signature_cannot_match_another_scanners_job() {
        // The disjunction's safety property, ASSERTED rather than assumed:
        // `step_concluded` is false for an absent step, so a job that never
        // recorded hypatia's steps cannot be judged by hypatia's signature.
        let set = parse_signatures(DIRECTIVE);
        let panic_attack_job = [
            step("Run panic-attack assail", StepConclusion::Skipped),
            step(
                "Create stub findings (when panic-attack unavailable)",
                StepConclusion::Success,
            ),
        ];
        let matched = set
            .matching(&panic_attack_job)
            .expect("panic-attack's own signature must match");
        assert_eq!(
            matched.scanner, "panic-attack",
            "matched the wrong scanner — the disjunction leaked"
        );
    }

    #[test]
    fn a_half_written_table_is_dropped_not_carried() {
        let set = parse_signatures(
            r#"
[[gate-triage.detection.signatures]]
scanner = "half"
skipped-steps = ["Run something"]
"#,
        );
        assert!(
            set.signatures.is_empty(),
            "a signature with one empty half matches nothing and must not be counted as coverage"
        );
        assert!(!set.is_usable());
    }

    #[test]
    fn an_absent_directive_detects_nothing() {
        // Fail-safe: no directive must mean "detect no vacuity", never
        // "detect vacuity everywhere".
        assert!(!parse_signatures("").is_usable());
        assert!(!load_signatures(Path::new("/nonexistent-repo-root")).is_usable());
    }

    #[test]
    fn a_legacy_flat_directive_still_parses() {
        // Repos across the estate still ship the 0.2.0 directive. Without the
        // fallback, upgrading the host would silently stop detecting hypatia
        // vacuity on every one of them.
        let set = parse_signatures(
            r#"
[gate-triage.detection]
signature-skipped-steps = ["Run Hypatia scan"]
signature-success-steps = ["Create stub findings (when Hypatia unavailable)"]
"#,
        );
        assert!(set.is_usable(), "a 0.2.0 directive must still be readable");
        assert_eq!(set.signatures.len(), 1);
    }

    #[test]
    fn the_tables_win_over_legacy_keys_so_hypatia_is_never_parsed_twice() {
        let mut both = DIRECTIVE.to_string();
        both.push_str("\nsignature-skipped-steps = [\"Run Hypatia scan\"]\n");
        both.push_str("signature-success-steps = [\"Create stub findings\"]\n");
        let set = parse_signatures(&both);
        assert_eq!(set.signatures.len(), 2, "the legacy pair must not add a third");
    }

    // ---- ground truth ------------------------------------------------------

    /// Every `name:` value in this repo's own gate workflow. Extracted from the
    /// shipped artefact, so it cannot drift the way a copied-in fixture can —
    /// which is exactly the failure PR #60 nearly shipped, when the directive
    /// carried a step name that had come from a ruling rather than a file.
    fn real_workflow_step_names() -> Vec<String> {
        let raw = std::fs::read_to_string(
            workspace_root().join(".github/workflows/static-analysis-gate.yml"),
        )
        .expect("this repo ships .github/workflows/static-analysis-gate.yml");
        raw.lines()
            .filter_map(|l| {
                let t = l.trim();
                let t = t.strip_prefix("- ").unwrap_or(t);
                t.strip_prefix("name:").map(|v| v.trim().to_string())
            })
            .collect()
    }

    #[test]
    fn the_repos_own_directive_names_real_workflow_steps() {
        let names = real_workflow_step_names();
        assert!(
            names.len() > 10,
            "extractor found only {} names — it has stopped reading the workflow",
            names.len()
        );

        let set = load_signatures(workspace_root());
        assert!(
            set.is_usable(),
            "this repo's own {DIRECTIVE_PATH} must parse into a usable signature set"
        );
        assert_eq!(
            set.signatures.len(),
            3,
            "the workflow carries three stub paths; all three must be described"
        );

        for entry in &set.signatures {
            for step_name in entry
                .signature
                .skipped_steps
                .iter()
                .chain(entry.signature.success_steps.iter())
            {
                assert!(
                    names.contains(step_name),
                    "directive names step {step_name:?} for scanner {:?}, but no such step \
                     exists in static-analysis-gate.yml. `step_concluded` compares EXACTLY, \
                     so this signature would match no job on earth. Do not abbreviate.",
                    entry.scanner
                );
            }
        }
    }

    #[test]
    fn all_three_scanners_are_named() {
        let set = load_signatures(workspace_root());
        let mut scanners: Vec<&str> = set.signatures.iter().map(|s| s.scanner.as_str()).collect();
        scanners.sort_unstable();
        assert_eq!(scanners, vec!["hypatia", "panic-attack", "patch-bridge"]);
    }

    // ---- axis 0 ------------------------------------------------------------

    #[test]
    fn an_undeclared_predicate_means_applies_here() {
        // Never manufacture an inapplicability claim from silence.
        let a = parse_applicability("");
        assert!(a.is_undeclared());
    }

    #[test]
    fn the_directives_commented_example_is_not_read_as_a_predicate() {
        // `[gate-triage.applicability]` carries a worked example in comments:
        //   #   runs-on-channels = ["alpha"]
        // A parser that read its own documentation would declare this repo
        // inapplicable on every channel but alpha.
        let a = load_applicability(workspace_root());
        assert!(
            a.is_undeclared(),
            "read a predicate out of the directive's comments: {a:?}"
        );
    }

    #[test]
    fn a_real_predicate_is_read_when_one_is_written() {
        let a = parse_applicability(
            r#"
[gate-triage.applicability]
runs-for-operator-types = ["developer", "platform_maintainer"]
runs-on-channels = ["alpha"]
"#,
        );
        assert_eq!(a.runs_on_channels, vec!["alpha".to_string()]);
        assert_eq!(a.runs_for_operator_types.len(), 2);
        assert!(!a.is_undeclared());
    }

    #[test]
    fn this_repos_manifest_declares_neither_key() {
        // Measured 2026-09-09: `0-AI-MANIFEST.a2ml` carries no `@` keys at all.
        // Axis 0 is therefore READ but cannot FIRE, and this test is what will
        // fail the day someone writes a declaration — at which point the
        // directive's `applicability-can-fire-today = false` must be updated
        // rather than left to rot.
        let d = load_declaration(workspace_root());
        assert!(d.operator_type.is_none(), "got {:?}", d.operator_type);
        assert!(d.channel.is_none(), "got {:?}", d.channel);
    }

    #[test]
    fn the_directive_names_a_manifest_that_exists() {
        // The directive named `0.1-AI-MANIFEST.a2ml` until 2026-09-09; no such
        // file has ever existed here, so the declaration could never be read.
        let directive = read(workspace_root(), DIRECTIVE_PATH);
        let section = section_of(&directive, "[gate-triage.applicability]");
        let named = extract_scalar(section, "manifest-file").expect("directive names a manifest");
        assert!(
            workspace_root().join(&named).is_file(),
            "directive names manifest {named:?}, which does not exist in this repo"
        );
    }
}
