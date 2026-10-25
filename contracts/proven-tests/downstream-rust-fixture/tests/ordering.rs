// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//!
//! The tests live in a module named `ordering` so that libtest prints
//! `test ordering::<name> ... ok`. Those printed names are the keys of
//! `.proven/coords.json`, which is what ties an executed test to a coordinate:
//! nothing in the evidence path guesses a coordinate from a test name.

use ordering_demo::{is_total, sorted_seqs, Pair};

mod ordering {
    use super::*;

    const CASES: usize = 256;
    const SEED: u64 = 0x5eed;

    /// Inputs for the stability property: `CASES` case vectors from a 32-bit
    /// LCG. Deterministic, so a failure is reproducible from the seed alone.
    fn generated_cases(seed: u64, cases: usize) -> Vec<Vec<Pair>> {
        let mut state = seed;
        let mut out = Vec::with_capacity(cases);
        for _ in 0..cases {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let len = ((state >> 33) % 5) as usize;
            let mut case = Vec::with_capacity(len);
            for seq in 0..len {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let key = ((state >> 33) % 3) as u32;
                case.push(Pair::new(key, seq as u32));
            }
            out.push(case);
        }
        out
    }

    /// The property: sorting by the total order is stable on equal keys, so the
    /// resulting sequence of `seq` values is the input order grouped by key.
    fn stability_failure(case: &[Pair]) -> bool {
        let actual = sorted_seqs(case.to_vec());
        let mut expected: Vec<u32> = (0..case.len() as u32).collect();
        expected.sort_by_key(|seq| case[*seq as usize].key);
        actual != expected
    }

    /// Shrink a failing case by dropping one element at a time while the
    /// failure persists. A report that names the minimal input is worth more
    /// than one that names a 4-element vector.
    fn shrink(case: &[Pair]) -> Vec<Pair> {
        let mut best = case.to_vec();
        let mut i = 0;
        while i < best.len() {
            let mut candidate = best.clone();
            candidate.remove(i);
            if stability_failure(&candidate) {
                best = candidate;
                i = 0;
            } else {
                i += 1;
            }
        }
        best
    }

    /// Example evidence at UnitTest/Functionality: one fixed slice, checked.
    #[test]
    fn is_total() {
        let pairs = [Pair::new(2, 1), Pair::new(1, 9), Pair::new(2, 0), Pair::new(1, 0)];
        assert!(is_total(&pairs));
    }

    /// Example evidence at UnitTest/Dependability: a comparator that disagreed
    /// with its own reverse would make `is_total` false.
    #[test]
    fn sorts_pair() {
        let pairs = vec![Pair::new(2, 5), Pair::new(1, 7), Pair::new(2, 1)];
        assert_eq!(sorted_seqs(pairs), vec![7, 1, 5]);
    }

    /// Property evidence at PropertyBasedTest/Dependability: over `CASES`
    /// generated cases, sorting is stable on equal keys.
    ///
    /// Declared `provisionally_proven`, never `actually_proven`: this is
    /// sampling, however many cases run. `.proven/coords.json` says so out loud.
    #[test]
    fn is_stable() {
        let cases = generated_cases(SEED, CASES);
        let mut failures = 0usize;
        let mut minimal: Option<Vec<Pair>> = None;

        for case in &cases {
            if stability_failure(case) {
                failures += 1;
                if minimal.is_none() {
                    minimal = Some(shrink(case));
                }
            }
        }

        assert_eq!(
            cases.len(),
            CASES,
            "the generator changed size; update evidence.property.cases_run in .proven/coords.json"
        );
        assert_eq!(
            failures, 0,
            "stability failed in {} of {} generated cases; minimal case: {:?}",
            failures,
            cases.len(),
            minimal.as_deref().unwrap_or(&[])
        );
    }
}
