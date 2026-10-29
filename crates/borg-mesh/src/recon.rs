//! Range-based set reconciliation of two hash sets: the cost grows with the difference, not with
//! the history (the Negentropy / iroh-docs idea, in its simplest lock-step form).
//!
//! The hash space is split by nibbles into a 16-ary tree of ranges. Both sides hold the same list
//! of *open* ranges and, per round, send a fingerprint (blake3 of the sorted hashes, and the count)
//! of each. Equal ranges close. A differing range splits into its 16 children for the next round,
//! unless either side holds at most `LEAF` hashes in it: then both send their hashes of that range
//! and it closes. Both sides decide from the same two messages, so they stay in step without a
//! leader. Nothing differs: one round of one fingerprint. `d` lines differ among `n`: about
//! `log16(n / LEAF)` rounds, with at most `16·d` fingerprints and `LEAF·d` hashes per round.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::lines::Hash;
use crate::{from_hex, to_hex};

/// At or below this many hashes in a differing range, the hashes themselves are exchanged. Method:
/// trades rounds against message size (`log16(n / LEAF)` rounds, `LEAF·d` hashes per round); both
/// sides must use the same value, so it is part of the protocol (borg/mesh/<version>).
pub const LEAF: usize = 8;
/// A peer that keeps claiming differences beyond this is broken or hostile. Guard: bounds the open
/// ranges one session holds in memory (65 536, far past what two honest stores of a team differ by).
const MAX_OPEN: usize = 1 << 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Range {
    lo: Hash,
    /// Exclusive; `None` is the end of the hash space.
    hi: Option<Hash>,
    /// Nibbles fixed by `lo`.
    depth: usize,
}

impl Range {
    fn children(&self) -> Vec<Range> {
        let (byte, shift) = (self.depth / 2, if self.depth.is_multiple_of(2) { 4 } else { 0 });
        let with = |k: u8| {
            let mut h = self.lo;
            h[byte] |= k << shift;
            h
        };
        (0..16u8).map(|k| Range { lo: with(k), hi: if k < 15 { Some(with(k + 1)) } else { self.hi }, depth: self.depth + 1 }).collect()
    }

    fn contains(&self, h: &Hash) -> bool {
        *h >= self.lo && self.hi.is_none_or(|hi| *h < hi)
    }
}

/// One round's message: fingerprints of the open ranges, hashes of the leaf ranges (both in the
/// order both sides share).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Round {
    pub fps: Vec<(String, u32)>,
    pub leaves: Vec<Vec<String>>,
}

/// What one side exchanged: rounds, fingerprints and hashes it sent.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Cost {
    pub rounds: usize,
    pub fps: usize,
    pub hashes: usize,
}

pub struct Recon<'a, V> {
    set: &'a BTreeMap<Hash, V>,
    open: Vec<Range>,
    leaves: Vec<Range>,
    /// Hashes the peer holds and this side lacks / this side holds and the peer lacks.
    pub want: BTreeSet<Hash>,
    pub give: BTreeSet<Hash>,
    pub cost: Cost,
}

impl<'a, V> Recon<'a, V> {
    pub fn new(set: &'a BTreeMap<Hash, V>) -> Self {
        Recon {
            set,
            open: vec![Range { lo: [0; 32], hi: None, depth: 0 }],
            leaves: Vec::new(),
            want: BTreeSet::new(),
            give: BTreeSet::new(),
            cost: Cost::default(),
        }
    }

    fn keys(&self, r: &Range) -> impl Iterator<Item = &Hash> {
        let range = match r.hi {
            Some(hi) => self.set.range(r.lo..hi),
            None => self.set.range(r.lo..),
        };
        range.map(|(k, _)| k)
    }

    fn fingerprint(&self, r: &Range) -> (String, u32) {
        let mut h = blake3::Hasher::new();
        let mut n = 0u32;
        for k in self.keys(r) {
            h.update(k);
            n += 1;
        }
        (if n == 0 { "0".into() } else { to_hex(h.finalize().as_bytes()) }, n)
    }

    /// This side's message for the current round.
    pub fn message(&mut self) -> Round {
        let fps: Vec<(String, u32)> = self.open.iter().map(|r| self.fingerprint(r)).collect();
        let leaves: Vec<Vec<String>> = self.leaves.iter().map(|r| self.keys(r).map(|k| to_hex(k)).collect()).collect();
        self.cost.rounds += 1;
        self.cost.fps += fps.len();
        self.cost.hashes += leaves.iter().map(Vec::len).sum::<usize>();
        Round { fps, leaves }
    }

    /// Takes the peer's message for the round this side just sent; true once nothing is open.
    pub fn absorb(&mut self, mine: &Round, theirs: Round) -> Result<bool, String> {
        if theirs.fps.len() != self.open.len() || theirs.leaves.len() != self.leaves.len() {
            return Err("reconciliation: peer is out of step".into());
        }
        for (r, hashes) in self.leaves.iter().zip(theirs.leaves) {
            let theirs: BTreeSet<Hash> = hashes.iter().map(|h| from_hex::<32>(h)).collect::<Result<_, _>>()?;
            if !theirs.iter().all(|h| r.contains(h)) {
                return Err("reconciliation: peer sent a hash outside its range".into());
            }
            for k in self.keys(r).copied().collect::<Vec<_>>() {
                if !theirs.contains(&k) {
                    self.give.insert(k);
                }
            }
            self.want.extend(theirs.into_iter().filter(|h| !self.set.contains_key(h)));
        }
        let (mut open, mut leaves) = (Vec::new(), Vec::new());
        for ((r, my), their) in self.open.iter().zip(&mine.fps).zip(&theirs.fps) {
            if my == their {
                continue;
            }
            if (my.1 as usize) <= LEAF || (their.1 as usize) <= LEAF || r.depth >= 63 {
                leaves.push(*r);
            } else {
                open.extend(r.children());
            }
        }
        if open.len() > MAX_OPEN {
            return Err("reconciliation: too many differing ranges".into());
        }
        (self.open, self.leaves) = (open, leaves);
        Ok(self.open.is_empty() && self.leaves.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(range: std::ops::Range<u32>) -> BTreeMap<Hash, ()> {
        range.map(|i| (*blake3::hash(&i.to_le_bytes()).as_bytes(), ())).collect()
    }

    /// Runs both sides to the end; returns (a's want, a's give, a's cost + b's cost).
    fn run(a: &BTreeMap<Hash, ()>, b: &BTreeMap<Hash, ()>) -> (BTreeSet<Hash>, BTreeSet<Hash>, Cost) {
        let (mut ra, mut rb) = (Recon::new(a), Recon::new(b));
        loop {
            let (ma, mb) = (ra.message(), rb.message());
            let da = ra.absorb(&ma, mb.clone()).unwrap();
            let db = rb.absorb(&mb, ma).unwrap();
            assert_eq!(da, db, "both sides stay in step");
            if da {
                break;
            }
        }
        assert_eq!((&ra.want, &ra.give), (&rb.give, &rb.want));
        let cost = Cost { rounds: ra.cost.rounds, fps: ra.cost.fps + rb.cost.fps, hashes: ra.cost.hashes + rb.cost.hashes };
        (ra.want, ra.give, cost)
    }

    #[test]
    fn equal_sets_cost_one_fingerprint() {
        let a = set(0..5000);
        let (want, give, cost) = run(&a, &a.clone());
        assert!(want.is_empty() && give.is_empty());
        assert_eq!(cost, Cost { rounds: 1, fps: 2, hashes: 0 });
    }

    #[test]
    fn finds_exactly_the_difference_at_a_cost_bounded_by_it() {
        let common = set(0..20_000);
        let (mut a, mut b) = (common.clone(), common.clone());
        a.extend(set(100_000..100_003));
        b.extend(set(200_000..200_004));
        let (want, give, cost) = run(&a, &b);
        assert_eq!(want, set(200_000..200_004).into_keys().collect());
        assert_eq!(give, set(100_000..100_003).into_keys().collect());
        // 7 differences among 20k: a few rounds, hashes ≤ 2·LEAF·d, nowhere near 20k.
        assert!(cost.hashes <= 2 * LEAF * 7 && cost.fps <= 2 * (1 + 16 * 7 * cost.rounds), "{cost:?}");
        assert!(cost.rounds <= 6, "{cost:?}");
    }

    #[test]
    fn one_side_empty_and_disjoint_small_sets() {
        let a = set(0..5);
        let (want, give, _) = run(&a, &BTreeMap::new());
        assert_eq!((want.len(), give.len()), (0, 5));
        let (want, give, _) = run(&set(0..300), &set(300..600));
        assert_eq!((want.len(), give.len()), (300, 300));
    }
}
