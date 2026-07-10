// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble-app` — the squabbler's loopback HTTP backend.
//!
//! This is the process the boj-server `cicd-squabbler-mcp` cartridge talks
//! to. Cartridge gateways run in a Deno sandbox with `--allow-net` only (no
//! subprocess), so the squabbler must be reachable over loopback HTTP rather
//! than exec'd; this binary serves the *same* shared planner
//! (`squabble-fight`) the CLI uses, so the two hosts cannot drift.
//!
//! Surface (all JSON):
//!
//! * `GET  /health`          — liveness + version.
//! * `POST /api/v1/diagnose` — body: a `Gate` (`{checks:[...]}` — a
//!   `{gate:{checks:[...]}}` wrapper is also accepted for symmetry with
//!   `/api/v1/fight`); returns the pure engine's `Diagnosis`.
//! * `POST /api/v1/fight`    — body: `{slug, gate, repo_root?}`; returns the
//!   full `Outcome`/`Report` evidence manifest from the shared planner.
//!   When `repo_root` is absent the fight runs **ungrounded**: no filesystem
//!   is probed (deliberately not the daemon's own working directory, which
//!   would mis-attribute evidence) and the report carries an explicit
//!   "ground-truth absent" blocker.
//!
//! The bind address is hardcoded to `127.0.0.1` (only the port is
//! configurable via `SQUABBLE_PORT`) because this backend has no auth layer
//! and the boj gateway is co-located; no interface knob is offered, and none
//! should be added without adding authentication first. A malformed or zero
//! `SQUABBLE_PORT` refuses to boot (exit 69) rather than silently binding
//! somewhere the cartridge is not pointing.
//!
//! The GitHub App *webhook* leg (`check_run.requested_action` — the
//! "Squabble!" button — authenticated via octocrab) is still the next step
//! and remains honestly absent: no webhook route is registered, so a webhook
//! delivery gets a plain 404 rather than a fake 200.

use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use squabble_core::gate::Gate;
use squabble_core::outcome::Outcome;
use squabble_core::{diagnose, Diagnosis};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

const DEFAULT_PORT: u16 = 7741;

#[derive(Debug, Deserialize)]
struct FightRequest {
    /// `owner/repo` of the repo under fight (used for upstream-owner attribution).
    slug: String,
    /// The gate: required checks + realised runs (from `squabble fetch` or a host).
    gate: Gate,
    /// Optional path to a local checkout; enables the a2ml context reader and
    /// workflow ground-truthing. Absent → the fight runs ungrounded (no
    /// filesystem probes) and says so in the report.
    repo_root: Option<PathBuf>,
}

/// Accepts both the bare-`Gate` body and a `{gate: ...}` wrapper, so callers
/// who generalise from `/api/v1/fight`'s shape by analogy don't get a 422.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DiagnoseRequest {
    Bare(Gate),
    Wrapped { gate: Gate },
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "squabble-app",
        "version": env!("CARGO_PKG_VERSION"),
        "endpoints": ["/health", "/api/v1/diagnose", "/api/v1/fight"],
    }))
}

async fn diagnose_handler(Json(req): Json<DiagnoseRequest>) -> Json<Diagnosis> {
    let gate = match req {
        DiagnoseRequest::Bare(g) | DiagnoseRequest::Wrapped { gate: g } => g,
    };
    Json(diagnose(&gate))
}

async fn fight_handler(Json(req): Json<FightRequest>) -> Json<Outcome> {
    let outcome = match req.repo_root {
        Some(root) => squabble_fight::plan_at_root(&req.gate, &req.slug, &root).1,
        // No checkout supplied: plan ungrounded rather than silently probing
        // whatever directory this daemon happens to have been launched from.
        None => squabble_fight::plan_ungrounded(&req.gate, &req.slug),
    };
    Json(outcome)
}

/// Parse `SQUABBLE_PORT`. Fail-closed: a value that is set but malformed or
/// zero (zero would bind an OS-assigned ephemeral port the cartridge cannot
/// know about) refuses to boot instead of silently using the default.
fn port_from_env() -> Result<u16, String> {
    match std::env::var("SQUABBLE_PORT") {
        Ok(v) => match v.parse::<u16>() {
            Ok(0) => Err(
                "SQUABBLE_PORT=0 would bind an ephemeral port the cartridge cannot target"
                    .to_string(),
            ),
            Ok(p) => Ok(p),
            Err(e) => Err(format!("SQUABBLE_PORT=`{v}` is not a port: {e}")),
        },
        Err(_) => Ok(DEFAULT_PORT),
    }
}

#[tokio::main]
async fn main() {
    let port = match port_from_env() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("squabble-app: {e} — refusing to boot (fail-closed)");
            std::process::exit(69); // EX_UNAVAILABLE
        }
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/v1/diagnose", post(diagnose_handler))
        .route("/api/v1/fight", post(fight_handler));

    // Loopback-only: the interface is hardcoded, not configurable (no auth
    // layer exists, so a wider bind is not offered).
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("squabble-app: cannot bind {addr}: {e}");
            std::process::exit(69); // EX_UNAVAILABLE — fail loudly, no fake OK.
        }
    };
    let bound = listener.local_addr().unwrap_or(addr);
    println!(
        "squabble-app {} listening on http://{bound} (loopback-only)",
        env!("CARGO_PKG_VERSION")
    );

    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("squabble-app: server error: {e}");
        std::process::exit(69);
    }
}
