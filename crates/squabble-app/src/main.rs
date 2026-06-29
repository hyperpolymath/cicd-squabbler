// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble-app` — the GitHub App front-end (STUB).
//!
//! Planned shape (see docs/CHARTER.adoc): an `axum` webhook server that
//! receives `check_run` `requested_action` events (the "Squabble!" button in
//! the PR checks section — the closest native button GitHub exposes; a button
//! inside the merge box is NOT possible), authenticates via `octocrab`, and
//! calls the same `squabble_core` engine the CLI uses. *Provisional:* hosted on
//! the farm (owned compute), per the cost-governor doctrine — owner to confirm.
//!
//! v0.1 deliberately does not pretend to serve: it exits non-zero with a clear
//! message rather than binding a port that does nothing (fail loudly, no
//! silent green).

use std::process::ExitCode;

fn main() -> ExitCode {
    // Touch the shared engine so the dependency is real and the link is wired.
    let v = squabble_core::gate::CheckRun::Passed;
    eprintln!(
        "squabble-app {}: the webhook server is not implemented yet.\n\
         Next step: add axum + octocrab, handle check_run.requested_action,\n\
         call squabble_core (CheckRun discriminant proves the link: {:?}).\n\
         See docs/CHARTER.adoc.",
        env!("CARGO_PKG_VERSION"),
        v
    );
    ExitCode::from(69) // EX_UNAVAILABLE — honestly unavailable, not a fake OK.
}
