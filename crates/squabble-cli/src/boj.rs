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
//! Design constraints honoured here:
//!
//! * **Host-only, feature-gated.** `squabble-core` must never depend on boj
//!   (detachability is the product identity); the default build compiles this
//!   module out entirely and gains zero dependencies.
//! * **Fail-closed, no silent skip** (`AGENTIC.a2ml`): an unreachable server
//!   or expert *appends evidence* to the escalation — the hand-off stands and
//!   the failure is recorded; nothing is dropped and nothing is faked.
//! * **No overclaim** (doctrine #10): `dispatch-fix` has no single actuator
//!   cartridge today (fleet-mcp tracks, it does not fix), so a dispatch is
//!   recorded as *assessed + tracked, actuation external* — never as "a fix
//!   was applied" or "a PR was opened".

use squabble_core::moves::{EscalationKind, ExpertGroup};
use squabble_core::outcome::{Escalation, Outcome};
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

    /// `POST /cartridge/<name>/invoke {"tool", "arguments"}` → the cartridge's
    /// result object (returned unwrapped by the router on 200).
    pub fn invoke(
        &self,
        cartridge: &str,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let url = format!("{}/cartridge/{cartridge}/invoke", self.base);
        let body = serde_json::json!({ "tool": tool, "arguments": arguments });
        match self.agent.post(&url).send_json(body) {
            Ok(resp) => resp
                .into_json()
                .map_err(|e| format!("`{cartridge}/{tool}` returned non-JSON: {e}")),
            Err(ureq::Error::Status(code, resp)) => {
                let detail = resp.into_string().unwrap_or_default();
                Err(format!(
                    "`{cartridge}/{tool}` failed: HTTP {code} {}",
                    truncate(&detail, 200)
                ))
            }
            Err(e) => Err(format!("`{cartridge}/{tool}` unreachable: {e}")),
        }
    }
}

/// One concrete expert call derived from an escalation.
struct ExpertCall {
    cartridge: &'static str,
    tool: &'static str,
    arguments: serde_json::Value,
    /// Honest framing of what a successful call *means* (no overclaim).
    meaning: &'static str,
}

/// Map an escalation to the cartridge tool call(s) for its specialist group.
/// The routing is deliberately host-side data, not core types.
fn route(e: &Escalation, repo_root: &Path) -> Vec<ExpertCall> {
    let path = repo_root.display().to_string();
    match (e.group, e.obligation) {
        (ExpertGroup::Security, _) | (_, EscalationKind::Scan) => vec![ExpertCall {
            cartridge: "panic-attack-mcp",
            tool: "panic_attack_scan",
            arguments: serde_json::json!({ "path": path }),
            meaning: "weak-point scan",
        }],
        (ExpertGroup::Proof, _) | (_, EscalationKind::VerifyClaim) => vec![ExpertCall {
            cartridge: "echidna-llm-mcp",
            tool: "consult",
            arguments: serde_json::json!({
                "question": format!(
                    "Verify the claim behind failing CI check `{}`: {}",
                    e.check, e.evidence
                )
            }),
            meaning: "proof consultation",
        }],
        // Hypatia / HypatiaFleet, assess or dispatch: hypatia assesses; the
        // fleet cartridge only *tracks* gate results. Actuation (fix + PR) has
        // no cartridge today and stays external — recorded as such.
        (ExpertGroup::Hypatia | ExpertGroup::HypatiaFleet, _) => vec![ExpertCall {
            cartridge: "hypatia-mcp",
            tool: "hypatia_scan_repo",
            arguments: serde_json::json!({ "path": path }),
            meaning: "hypatia assessment (actuation external — no fixer cartridge yet)",
        }],
    }
}

/// Execute every escalation's expert call, folding verdicts (or fail-closed
/// unreachability evidence) back into the report. Never removes or downgrades
/// an escalation: a summon can only *add* evidence.
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

    let client = BojClient::from_env();
    let health = client.health();
    if let Err(err) = &health {
        // Fail-closed: record once at the report level and on every
        // escalation, so no hand-off silently looks "attempted".
        report.blockers.push(format!("summon: {err}"));
        for e in &mut report.escalations {
            e.evidence
                .push_str(" | summon: expert unreachable — escalation stands, fail-closed");
        }
        return;
    }

    for e in &mut report.escalations {
        for call in route(e, repo_root) {
            match client.invoke(call.cartridge, call.tool, call.arguments) {
                Ok(v) => {
                    let compact = truncate(&v.to_string(), 400);
                    e.evidence.push_str(&format!(
                        " | summon {} `{}/{}`: {compact}",
                        call.meaning, call.cartridge, call.tool
                    ));
                    report.blockers.push(format!(
                        "summon `{}` → {}/{} ok ({})",
                        e.check, call.cartridge, call.tool, call.meaning
                    ));
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
                }
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…", &s[..cut])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn routing_maps_groups_to_expected_cartridges() {
        let root = Path::new("/repo");
        let sec = route(
            &escalation(ExpertGroup::Security, EscalationKind::Scan),
            root,
        );
        assert_eq!(sec[0].cartridge, "panic-attack-mcp");
        let proof = route(
            &escalation(ExpertGroup::Proof, EscalationKind::VerifyClaim),
            root,
        );
        assert_eq!(proof[0].cartridge, "echidna-llm-mcp");
        let fleet = route(
            &escalation(ExpertGroup::HypatiaFleet, EscalationKind::DispatchFix),
            root,
        );
        assert_eq!(fleet[0].cartridge, "hypatia-mcp");
        assert!(fleet[0].meaning.contains("actuation external"));
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

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "aé".repeat(300);
        let t = truncate(&s, 401); // lands mid-'é' (2-byte) without the guard
        assert!(t.ends_with('…'));
        assert!(t.len() <= 405);
    }
}
