// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//! A deliberately small subject: a total order on a pair, and a stable sort.
//!
//! Nothing here is clever. The point of the fixture is the *evidence path*, so
//! the subject must be boring enough that a reader can see at once whether a
//! test's coordinate and claim are honest.

/// A pair ordered lexicographically by `key`, then by `seq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pair {
    pub key: u32,
    pub seq: u32,
}

impl Pair {
    pub fn new(key: u32, seq: u32) -> Self {
        Self { key, seq }
    }

    /// Total order: `key` first, then `seq`. No two distinct pairs compare equal.
    pub fn cmp_total(&self, other: &Self) -> std::cmp::Ordering {
        self.key.cmp(&other.key).then(self.seq.cmp(&other.seq))
    }
}

/// Is the order total over a slice: antisymmetric and transitive on every pair?
pub fn is_total(pairs: &[Pair]) -> bool {
    for a in pairs {
        for b in pairs {
            let ab = a.cmp_total(b);
            let ba = b.cmp_total(a);
            if ab != ba.reverse() {
                return false;
            }
            for c in pairs {
                if ab == std::cmp::Ordering::Less && b.cmp_total(c) == std::cmp::Ordering::Less
                    && a.cmp_total(c) != std::cmp::Ordering::Less
                {
                    return false;
                }
            }
        }
    }
    true
}

/// Sort by the total order and return the ordered sequence numbers.
pub fn sorted_seqs(mut pairs: Vec<Pair>) -> Vec<u32> {
    pairs.sort_by(Pair::cmp_total);
    pairs.into_iter().map(|p| p.seq).collect()
}
