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
//! * `POST /api/v1/diagnose` — body: a `Gate`; returns the pure engine's
//!   `Diagnosis` (state + one conservative move per unsatisfied check).
//! * `POST /api/v1/fight`    — body: `{slug, gate, repo_root?}`; returns the
//!   full `Outcome`/`Report` evidence manifest from the shared planner.
//!
//! Binding is **loopback-only by design** (`127.0.0.1`, port from
//! `SQUABBLE_PORT`, default 7741): this backend has no auth layer, and the
//! boj gateway is co-located. It deliberately refuses to bind non-loopback.
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
    /// workflow ground-truthing. Absent/missing paths are fail-safe (empty
    /// context, conservative classification).
    repo_root: Option<String>,
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "squabble-app",
        "version": env!("CARGO_PKG_VERSION"),
        "endpoints": ["/health", "/api/v1/diagnose", "/api/v1/fight"],
    }))
}

async fn diagnose_handler(Json(gate): Json<Gate>) -> Json<Diagnosis> {
    Json(diagnose(&gate))
}

async fn fight_handler(Json(req): Json<FightRequest>) -> Json<Outcome> {
    let root = PathBuf::from(req.repo_root.unwrap_or_else(|| ".".to_string()));
    let (_context, outcome) = squabble_fight::plan_at_root(&req.gate, &req.slug, &root);
    Json(outcome)
}

fn port_from_env() -> u16 {
    match std::env::var("SQUABBLE_PORT") {
        Ok(v) => v.parse().unwrap_or_else(|_| {
            eprintln!("squabble-app: SQUABBLE_PORT=`{v}` is not a port; using {DEFAULT_PORT}");
            DEFAULT_PORT
        }),
        Err(_) => DEFAULT_PORT,
    }
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/v1/diagnose", post(diagnose_handler))
        .route("/api/v1/fight", post(fight_handler));

    // Loopback-only: no auth layer exists, so a wider bind is not offered.
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port_from_env()));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("squabble-app: cannot bind {addr}: {e}");
            std::process::exit(69); // EX_UNAVAILABLE — fail loudly, no fake OK.
        }
    };
    println!(
        "squabble-app {} listening on http://{addr} (loopback-only)",
        env!("CARGO_PKG_VERSION")
    );

    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("squabble-app: server error: {e}");
        std::process::exit(69);
    }
}
