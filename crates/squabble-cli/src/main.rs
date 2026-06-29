// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! `squabble` — the CLI front-end to `squabble-core`.
//!
//! v0.1 surface: `squabble diagnose <gate.json>` reads a gate description
//! (the required contexts and their realised runs), runs the pure engine, and
//! prints the state plus the legitimate moves it proposes. git/`gh` plumbing
//! that *applies* moves is the next implementation step (see docs/CHARTER.adoc);
//! this binary fails loudly rather than pretending to land anything.

use squabble_core::{diagnose, gate::Gate};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("diagnose") => match args.get(2) {
            Some(path) => run_diagnose(path),
            None => {
                eprintln!("squabble diagnose: missing <gate.json> argument");
                ExitCode::from(2)
            }
        },
        Some("--version") | Some("-V") => {
            println!("squabble {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "squabble {} — CI/CD fighter (squabble ≠ bypass)\n\n\
                 USAGE:\n  squabble diagnose <gate.json>\n  squabble --version\n",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::from(2)
        }
    }
}

fn run_diagnose(path: &str) -> ExitCode {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("squabble: cannot read `{path}`: {e}");
            return ExitCode::from(2);
        }
    };
    let gate: Gate = match serde_json::from_str(&text) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("squabble: `{path}` is not a valid gate: {e}");
            return ExitCode::from(2);
        }
    };

    let d = diagnose(&gate);
    println!("gate state: {:?}", d.state);
    if d.proposed.is_empty() {
        println!("no moves proposed — gate is satisfied or has no work the engine recognises.");
    } else {
        println!("proposed legitimate moves ({}):", d.proposed.len());
        for m in &d.proposed {
            println!("  - {}", m.describe());
        }
    }
    ExitCode::SUCCESS
}
