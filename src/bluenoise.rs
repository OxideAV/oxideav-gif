//! A 64 × 64 blue-noise threshold map for ordered dithering, built once
//! on first use.
//!
//! An ordered dither adds a position-dependent offset to every pixel
//! before the nearest-entry search. A recursive (Bayer) threshold matrix
//! gives a regular cross-hatch; a *blue-noise* map — one whose
//! thresholds have no low-frequency structure, so that the pixels below
//! any level are spread as evenly as possible — gives an irregular,
//! grain-like pattern with no visible tile.
//!
//! The map is a rank ordering of the 4096 positions built from an
//! energy field: every chosen position contributes a Gaussian bump
//! (wrapping around the tile edges), so the field is high inside
//! clusters and low inside voids.
//!
//! 1. Scatter a tenth of the positions from a fixed-seed generator, then
//!    relax them: move the point at the field's maximum (the tightest
//!    cluster) to the field's minimum among free positions (the largest
//!    void) until the point just removed is itself the largest void.
//! 2. Rank the relaxed points by removing the tightest cluster each
//!    time: the last one removed gets rank 0.
//! 3. Starting again from the relaxed set, fill the largest void each
//!    time and give it the next rank, until the tile is full.

use std::sync::OnceLock;

const SIDE: usize = 64;
const AREA: usize = SIDE * SIDE;
/// Bumps are truncated at this many positions from their centre.
const REACH: i32 = 7;
/// Standard deviation of a bump, in positions.
const SIGMA: f32 = 1.9;

struct Field {
    energy: Vec<f32>,
    taken: Vec<bool>,
    bump: Vec<f32>,
}

impl Field {
    fn new() -> Self {
        let span = (2 * REACH + 1) as usize;
        let mut bump = vec![0.0; span * span];
        for dy in -REACH..=REACH {
            for dx in -REACH..=REACH {
                let r2 = (dx * dx + dy * dy) as f32;
                bump[(dy + REACH) as usize * span + (dx + REACH) as usize] =
                    (-r2 / (2.0 * SIGMA * SIGMA)).exp();
            }
        }
        Field {
            energy: vec![0.0; AREA],
            taken: vec![false; AREA],
            bump,
        }
    }

    fn set(&mut self, at: usize, taken: bool) {
        self.taken[at] = taken;
        let sign = if taken { 1.0 } else { -1.0 };
        let (x, y) = ((at % SIDE) as i32, (at / SIDE) as i32);
        let span = (2 * REACH + 1) as usize;
        for dy in -REACH..=REACH {
            let yy = (y + dy).rem_euclid(SIDE as i32) as usize;
            for dx in -REACH..=REACH {
                let xx = (x + dx).rem_euclid(SIDE as i32) as usize;
                self.energy[yy * SIDE + xx] +=
                    sign * self.bump[(dy + REACH) as usize * span + (dx + REACH) as usize];
            }
        }
    }

    /// Taken position with the highest energy.
    fn tightest_cluster(&self) -> usize {
        let mut best = (f32::NEG_INFINITY, 0);
        for (i, (&e, &t)) in self.energy.iter().zip(&self.taken).enumerate() {
            if t && e > best.0 {
                best = (e, i);
            }
        }
        best.1
    }

    /// Free position with the lowest energy.
    fn largest_void(&self) -> usize {
        let mut best = (f32::INFINITY, 0);
        for (i, (&e, &t)) in self.energy.iter().zip(&self.taken).enumerate() {
            if !t && e < best.0 {
                best = (e, i);
            }
        }
        best.1
    }
}

fn build() -> Vec<u16> {
    let mut field = Field::new();
    let seeds = AREA / 10;
    let mut state = 0x2545_F491u32;
    let mut placed = 0;
    while placed < seeds {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let at = (state >> 8) as usize % AREA;
        if !field.taken[at] {
            field.set(at, true);
            placed += 1;
        }
    }
    // Relax; the bound only guards against a floating-point cycle.
    for _ in 0..AREA * 4 {
        let cluster = field.tightest_cluster();
        field.set(cluster, false);
        let void = field.largest_void();
        field.set(void, true);
        if void == cluster {
            break;
        }
    }
    let relaxed = field.taken.clone();
    let mut rank = vec![0u16; AREA];
    for r in (0..seeds).rev() {
        let at = field.tightest_cluster();
        field.set(at, false);
        rank[at] = r as u16;
    }
    for (at, &t) in relaxed.iter().enumerate() {
        if t {
            field.set(at, true);
        }
    }
    for r in seeds..AREA {
        let at = field.largest_void();
        field.set(at, true);
        rank[at] = r as u16;
    }
    rank
}

/// Threshold rank (`0..4096`, each value exactly once per tile) at a
/// pixel position.
#[inline]
pub(crate) fn rank(x: usize, y: usize) -> u16 {
    static MAP: OnceLock<Vec<u16>> = OnceLock::new();
    MAP.get_or_init(build)[(y % SIDE) * SIDE + (x % SIDE)]
}

/// Number of distinct ranks in the map.
pub(crate) const RANKS: i32 = AREA as i32;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_are_a_permutation() {
        let mut seen = vec![false; AREA];
        for y in 0..SIDE {
            for x in 0..SIDE {
                let r = rank(x, y) as usize;
                assert!(!seen[r]);
                seen[r] = true;
            }
        }
        assert_eq!(rank(3, 5), rank(3 + SIDE, 5 + 2 * SIDE));
    }

    /// At every tested level, the positions below it are spread evenly:
    /// each 16 × 16 quarter-tile block holds close to its share, and far
    /// fewer of them touch each other than in a random scatter.
    #[test]
    fn low_ranks_are_evenly_spread() {
        for level in [AREA / 16, AREA / 8, AREA / 4] {
            let expect = level as f32 / 16.0;
            for by in 0..4 {
                for bx in 0..4 {
                    let mut n = 0;
                    for y in 0..16 {
                        for x in 0..16 {
                            if (rank(bx * 16 + x, by * 16 + y) as usize) < level {
                                n += 1;
                            }
                        }
                    }
                    assert!(
                        (n as f32 - expect).abs() <= expect * 0.35 + 1.0,
                        "level {level}: block ({bx},{by}) holds {n}, expected about {expect}"
                    );
                }
            }
        }
        // Horizontal neighbours both below 1/8: a random scatter would
        // give about AREA / 64 = 64 such pairs.
        let level = AREA / 8;
        let mut pairs = 0;
        for y in 0..SIDE {
            for x in 0..SIDE {
                if (rank(x, y) as usize) < level && (rank(x + 1, y) as usize) < level {
                    pairs += 1;
                }
            }
        }
        assert!(pairs < 8, "{pairs} adjacent pairs");
    }
}
