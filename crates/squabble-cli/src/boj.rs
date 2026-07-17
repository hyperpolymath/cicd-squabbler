// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! boj-server client — the optional "call in the big guns" leg (feature `boj`).
//!
//! boj-server ("Bundle of Joy") is the estate's single MCP/REST gateway; the
//! specialist experts the squabbler escalates to already exist there as
//! cartridges. This module maps each [`Escalation`] to a concrete cartridge
//! tool call over the documented REST invoke contract:
//!
//! ```text
//! POST {BOJ_URL:-http://localhost:7700}/cartridge/<name>/invoke
//! body: {"tool": "<tool>", "arguments": {...}}
//! ```
//!
//! Loopback callers bypass boj's trust enforcement, so no auth headers are
//! needed for a co-located client.
//!
//! **Why an in-process HTTP client here when `fetch.rs` deliberately shells
//! out to `gh` instead?** A deliberate, scoped reversal: this leg needs
//! structured error handling (transport vs HTTP-status vs error-shaped-body
//! are three different fail-closed outcomes below) and safe JSON bodies,
//! which a `curl` subprocess makes brittle. The reversal is contained by the
//! feature gate — the default build compiles this module out entirely, so
//! `fetch.rs`'s "no HTTP client dependency" stance still holds for it.
//!
//! Design constraints honoured here:
//!
//! * **Host-only, feature-gated.** `squabble-core` must never depend on boj
//!   (detachability is the product identity).
//! * **Fail-closed, no silent skip** (`AGENTIC.a2ml`): an unreachable server,
//!   a failing expert, *or a 200 whose body carries an error signal* records
//!   an explicit failure — the hand-off stands, nothing is dropped, nothing
//!   is faked. Transport success alone is never reported as a verdict.
//! * **No overclaim** (doctrine #10): `dispatch-fix` has no single actuator
//!   cartridge today (fleet-mcp tracks, it does not fix), so a dispatch is
//!   recorded as *assessed, actuation external* — never as "a fix was
//!   applied" or "a PR was opened".
//! * **Full-fidelity evidence**: every call is recorded verbatim as a typed
//!   [`ExpertVerdict`] on the report (never truncated); the escalation's
//!   `evidence` string carries only a short human narration.

use squabble_core::moves::ExpertGroup;
use squabble_core::outcome::{Escalation, ExpertVerdict, Outcome};
use std::path::Path;
use std::time::Duration;

const DEFAULT_BASE: &str = "http://localhost:7700";
const TIMEOUT: Duration = Duration::from_secs(10);

/// Minimal REST client for boj-server's cartridge-invoke contract.
pub struct BojClient {
    base: String,
    agent: ureq::Agent,
}

impl BojClient {
    /// Base URL from `BOJ_URL`, defaulting to the documented loopback port.
    pub fn from_env() -> Self {
        let base = std::env::var("BOJ_URL").unwrap_or_else(|_| DEFAULT_BASE.to_string());
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(TIMEOUT)
            .timeout(TIMEOUT)
            .build();
        BojClient { base, agent }
    }

    /// Liveness probe: `GET /health` → `{status:"ok", cartridges_loaded, ...}`.
    pub fn health(&self) -> Result<serde_json::Value, String> {
        self.agent
            .get(&format!("{}/health", self.base))
            .call()
            .map_err(|e| format!("boj-server unreachable at {}: {e}", self.base))?
            .into_json()
            .map_err(|e| format!("boj-server /health returned non-JSON: {e}"))
    }

    /// `POST /cartridge/<name>/invoke {"tool", "arguments"}`.
    ///
    /// The boj router returns the cartridge's result object **unwrapped** on
    /// HTTP 200 — including cartridge-level *failures* (the gateway pattern
    /// emits `{status: 503, data: {error: ...}}` and the router forwards the
    /// inner object). So a 200 is not a verdict: the body is inspected for
    /// the estate error shapes (`error` key, `success: false`) and those are
    /// returned as `Err`, keeping the summon fail-closed end to end.
    pub fn invoke(
        &self,
        cartridge: &str,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let url = format!("{}/cartridge/{cartridge}/invoke", self.base);
        let body = serde_json::json!({ "tool": tool, "arguments": arguments });
        match self.agent.post(&url).send_json(body) {
            Ok(resp) => {
                let v: serde_json::Value = resp
                    .into_json()
                    .map_err(|e| format!("`{cartridge}/{tool}` returned non-JSON: {e}"))?;
                if let Some(err) = body_error(&v) {
                    return Err(format!("`{cartridge}/{tool}` reported failure: {err}"));
                }
                Ok(v)
            }
            Err(ureq::Error::Status(code, resp)) => {
                let detail = resp.into_string().unwrap_or_default();
                Err(format!("`{cartridge}/{tool}` failed: HTTP {code} {detail}"))
            }
            Err(e) => Err(format!("`{cartridge}/{tool}` unreachable: {e}")),
        }
    }
}

/// Detect the estate error shapes in an otherwise-200 body: a top-level
/// `error` field, or `success: false`. Returns the error description.
fn body_error(v: &serde_json::Value) -> Option<String> {
    if let Some(err) = v.get("error") {
        return Some(err.to_string());
    }
    if v.get("success") == Some(&serde_json::Value::Bool(false)) {
        return Some(v.to_string());
    }
    None
}

/// One concrete expert call derived from an escalation.
struct ExpertCall {
    cartridge: &'static str,
    tool: &'static str,
    arguments: serde_json::Value,
    /// Honest framing of what a successful call *means* (no overclaim).
    meaning: &'static str,
}

/// Map an escalation to the cartridge tool call for its specialist group.
///
/// **Group is primary**: the planner already chose the specialist; the
/// obligation never reroutes to a different one (a HypatiaFleet escalation
/// with a `scan` obligation is still hypatia's case, possibly *using* a
/// scan). The routing is deliberately host-side data, not core types.
fn route(e: &Escalation, repo_path: &str) -> ExpertCall {
    match e.group {
        ExpertGroup::Security => ExpertCall {
            cartridge: "panic-attack-mcp",
            tool: "panic_attack_scan",
            arguments: serde_json::json!({ "path": repo_path }),
            meaning: "weak-point scan",
        },
        ExpertGroup::Proof => ExpertCall {
            cartridge: "echidna-llm-mcp",
            tool: "consult",
            arguments: serde_json::json!({
                "question": format!(
                    "Verify the claim behind failing CI check `{}`: {}",
                    e.check, e.evidence
                )
            }),
            meaning: "proof consultation",
        },
        // Hypatia / HypatiaFleet: hypatia assesses; the fleet cartridge only
        // *tracks* gate results. Actuation (fix + PR) has no cartridge today
        // and stays external — recorded as such.
        ExpertGroup::Hypatia | ExpertGroup::HypatiaFleet => ExpertCall {
            cartridge: "hypatia-mcp",
            tool: "hypatia_scan_repo",
            arguments: serde_json::json!({ "path": repo_path }),
            meaning: "hypatia assessment (actuation external — no fixer cartridge yet)",
        },
    }
}

/// Execute every escalation's expert call, folding verdicts (or fail-closed
/// unreachability evidence) back into the report. Never removes or downgrades
/// an escalation: a summon can only *add* evidence, and never changes the
/// outcome colour.
pub fn summon(outcome: &mut Outcome, repo_root: &Path) {
    let Outcome::Red { report } = outcome else {
        return;
    };
    if report.escalations.is_empty() {
        report
            .blockers
            .push("summon: no escalations to summon experts for".to_string());
        return;
    }

    // Experts run as separate processes with their own working directories, so
    // a relative path (the CLI's default is ".") would resolve against the
    // WRONG tree over there — and panic-attack's contract requires an absolute
    // path. Refuse to send garbage: no canonical path, no path-based calls.
    let repo_path = std::fs::canonicalize(repo_root)
        .map(|p| p.display().to_string())
        .map_err(|e| {
            format!(
                "cannot canonicalize repo root `{}`: {e}",
                repo_root.display()
            )
        });

    let client = BojClient::from_env();
    if let Err(err) = client.health() {
        // Fail-closed: record once at the report level and on every
        // escalation, so no hand-off silently looks "attempted".
        report.blockers.push(format!("summon: {err}"));
        for e in &mut report.escalations {
            e.evidence
                .push_str(" | summon: expert unreachable — escalation stands, fail-closed");
        }
        return;
    }

    let mut verdicts = Vec::new();
    for e in &mut report.escalations {
        let repo_path = match &repo_path {
            Ok(p) => p.as_str(),
            Err(err) => {
                e.evidence
                    .push_str(&format!(" | summon skipped: {err} — fail-closed"));
                report.blockers.push(format!("summon `{}`: {err}", e.check));
                continue;
            }
        };
        let call = route(e, repo_path);
        match client.invoke(call.cartridge, call.tool, call.arguments) {
            Ok(v) => {
                e.evidence.push_str(&format!(
                    " | summoned {} `{}/{}`: ok (full verdict in expert_verdicts)",
                    call.meaning, call.cartridge, call.tool
                ));
                report.blockers.push(format!(
                    "summon `{}` → {}/{} ok ({})",
                    e.check, call.cartridge, call.tool, call.meaning
                ));
                verdicts.push(ExpertVerdict {
                    check: e.check.clone(),
                    cartridge: call.cartridge.to_string(),
                    tool: call.tool.to_string(),
                    ok: true,
                    meaning: call.meaning.to_string(),
                    verdict: v.to_string(),
                });
            }
            Err(err) => {
                e.evidence.push_str(&format!(
                    " | summon `{}/{}` failed: {err} — escalation stands, fail-closed",
                    call.cartridge, call.tool
                ));
                report.blockers.push(format!(
                    "summon `{}` → {}/{} FAILED: {err}",
                    e.check, call.cartridge, call.tool
                ));
                verdicts.push(ExpertVerdict {
                    check: e.check.clone(),
                    cartridge: call.cartridge.to_string(),
                    tool: call.tool.to_string(),
                    ok: false,
                    meaning: call.meaning.to_string(),
                    verdict: err,
                });
            }
        }
    }
    report.expert_verdicts.extend(verdicts);
}

#[cfg(test)]
mod tests {
    use super::*;
    use squabble_core::moves::EscalationKind;
    use squabble_core::outcome::Report;

    fn escalation(group: ExpertGroup, obligation: EscalationKind) -> Escalation {
        Escalation {
            check: "some-check".into(),
            group,
            obligation,
            evidence: "why".into(),
        }
    }

    #[test]
    fn routing_is_group_primary() {
        let sec = route(
            &escalation(ExpertGroup::Security, EscalationKind::Scan),
            "/r",
        );
        assert_eq!(sec.cartridge, "panic-attack-mcp");
        let proof = route(
            &escalation(ExpertGroup::Proof, EscalationKind::VerifyClaim),
            "/r",
        );
        assert_eq!(proof.cartridge, "echidna-llm-mcp");
        // The obligation must never reroute away from the planner's chosen
        // specialist: HypatiaFleet stays hypatia's case even for scan/verify.
        for obligation in [
            EscalationKind::Scan,
            EscalationKind::VerifyClaim,
            EscalationKind::DispatchFix,
            EscalationKind::AssessConfidence,
        ] {
            let fleet = route(&escalation(ExpertGroup::HypatiaFleet, obligation), "/r");
            assert_eq!(fleet.cartridge, "hypatia-mcp");
            assert!(fleet.meaning.contains("actuation external"));
        }
    }

    #[test]
    fn body_error_detects_estate_error_shapes() {
        // The boj router unwraps cartridge failures into 200 bodies; both
        // estate shapes must be caught or a hard miss records as "ok".
        assert!(body_error(&serde_json::json!({"error": "ANTHROPIC_API_KEY not set"})).is_some());
        assert!(body_error(&serde_json::json!({"success": false, "detail": "x"})).is_some());
        assert!(body_error(&serde_json::json!({"success": true, "findings": []})).is_none());
        assert!(body_error(&serde_json::json!({"findings": []})).is_none());
    }

    #[test]
    fn summon_is_fail_closed_when_server_unreachable() {
        // Point at a port nothing listens on: every escalation must gain
        // explicit unreachability evidence and none may be dropped.
        std::env::set_var("BOJ_URL", "http://127.0.0.1:1");
        let mut outcome = Outcome::Red {
            report: Report {
                summary: "s".into(),
                unsatisfied: vec![],
                moves_attempted: vec![],
                escalations: vec![escalation(ExpertGroup::Security, EscalationKind::Scan)],
                owner_assignments: vec![],
                expert_verdicts: vec![],
                applied: vec![],
                blockers: vec![],
            },
        };
        summon(&mut outcome, Path::new("/repo"));
        let Outcome::Red { report } = &outcome else {
            panic!("summon must never change the outcome colour");
        };
        assert_eq!(report.escalations.len(), 1);
        assert!(report.escalations[0].evidence.contains("fail-closed"));
        assert!(report.blockers.iter().any(|b| b.starts_with("summon:")));
        std::env::remove_var("BOJ_URL");
    }
}
