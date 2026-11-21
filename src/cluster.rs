//! Palette selection from a colour census: exact tables for pictures
//! that already fit, variance-minimising box splitting plus weighted
//! k-means for the ones that do not.
//!
//! The work is done on a *census* of the picture — its distinct colours
//! with their pixel counts — rather than on the pixels, so its cost
//! follows the number of colours, not the picture size:
//!
//! * up to a few thousand distinct colours are counted one by one, so a
//!   picture that stays within the table budget is reproduced exactly
//!   and needs no clustering at all, and a picture slightly over it is
//!   clustered on its true colours;
//! * past that, every colour is pooled into a `64 × 64 × 64` grid of
//!   cells (the top six bits of each channel), keeping the pixel count
//!   and the exact colour sum of each cell, so a cell is represented by
//!   the mean of the colours that fell into it.
//!
//! [`select`] then partitions the census points in the chosen
//! [`Metric`] space. Starting from one box holding everything, it
//! repeatedly takes the box with the largest weighted sum of squared
//! deviations from its mean, projects its points on the direction of
//! greatest variance (the dominant eigenvector of the box's covariance
//! matrix), and cuts where the two halves' summed squared deviations are
//! smallest. For points `x_i` with weights `w_i`, a group's squared
//! deviation is `Σ w|x|² − |Σ w x|² / Σ w`, so the best cut maximises
//! `|S_left|² / W_left + |S_right|² / W_right` — one pass over the
//! sorted projections with running sums. The box means seed a weighted
//! k-means (assign every census point to its nearest centre, move each
//! centre to the weighted mean of its points) that runs until the
//! centres stop moving or the iteration budget is spent; each round can
//! only lower the total squared error.

use crate::colorspace::Metric;
use crate::nearest::Nearest;

/// One census point: a position in the metric space and its pixel count.
#[derive(Clone, Copy)]
pub(crate) struct Sample {
    pub(crate) at: [f32; 3],
    pub(crate) weight: f32,
}

// ---------------------------------------------------------------------
// Census.
// ---------------------------------------------------------------------

/// Distinct colours counted one by one before the census falls back to
/// the cell grid.
const DISTINCT_LIMIT: usize = 4096;
const DISTINCT_SLOTS: usize = 4 * DISTINCT_LIMIT;
const NO_COLOR: u32 = u32::MAX;

const CELL_SHIFT: u32 = 2;
const CELL_BITS: u32 = 8 - CELL_SHIFT;
const CELL_COUNT: usize = 1 << (3 * CELL_BITS);

#[derive(Clone, Copy, Default)]
struct Cell {
    count: u64,
    sum: [u64; 3],
}

/// The colours of a picture (or of several) with their pixel counts.
///
/// Up to [`DISTINCT_LIMIT`] distinct colours are counted exactly; past
/// that every colour is pooled into the six-bit-per-channel cell grid.
pub(crate) struct Census {
    /// Open-addressing table of packed colours, [`NO_COLOR`] when free.
    slots: Vec<u32>,
    /// Position in `colors` of the colour held by the matching slot.
    slot_entry: Vec<u16>,
    colors: Vec<([u8; 3], u64)>,
    /// The previous colour and its position (runs are common).
    last: u32,
    last_entry: usize,
    /// Allocated on the first overflow.
    cells: Vec<Cell>,
    occupied: Vec<u32>,
    pooled: bool,
}

fn pack(rgb: [u8; 3]) -> u32 {
    u32::from(rgb[0]) << 16 | u32::from(rgb[1]) << 8 | u32::from(rgb[2])
}

impl Census {
    pub(crate) fn new() -> Self {
        Census {
            slots: vec![NO_COLOR; DISTINCT_SLOTS],
            slot_entry: vec![0; DISTINCT_SLOTS],
            colors: Vec::new(),
            last: NO_COLOR,
            last_entry: 0,
            cells: Vec::new(),
            occupied: Vec::new(),
            pooled: false,
        }
    }

    #[inline]
    pub(crate) fn add(&mut self, rgb: [u8; 3]) {
        if self.pooled {
            self.add_to_cell(rgb, 1);
            return;
        }
        let key = pack(rgb);
        if key == self.last {
            self.colors[self.last_entry].1 += 1;
            return;
        }
        let mut slot = (key.wrapping_mul(0x9E37_79B1) >> 18) as usize;
        loop {
            let held = self.slots[slot];
            if held == key {
                let entry = self.slot_entry[slot] as usize;
                self.colors[entry].1 += 1;
                self.last = key;
                self.last_entry = entry;
                return;
            }
            if held == NO_COLOR {
                break;
            }
            slot = (slot + 1) & (DISTINCT_SLOTS - 1);
        }
        if self.colors.len() == DISTINCT_LIMIT {
            self.pool();
            self.add_to_cell(rgb, 1);
            return;
        }
        self.slots[slot] = key;
        self.slot_entry[slot] = self.colors.len() as u16;
        self.last = key;
        self.last_entry = self.colors.len();
        self.colors.push((rgb, 1));
    }

    /// Move the exact counts into the cell grid.
    fn pool(&mut self) {
        self.pooled = true;
        if self.cells.is_empty() {
            self.cells = vec![Cell::default(); CELL_COUNT];
        }
        for (rgb, count) in std::mem::take(&mut self.colors) {
            self.add_to_cell(rgb, count);
        }
    }

    #[inline]
    fn add_to_cell(&mut self, rgb: [u8; 3], count: u64) {
        let id = (rgb[0] as usize >> CELL_SHIFT) << (2 * CELL_BITS)
            | (rgb[1] as usize >> CELL_SHIFT) << CELL_BITS
            | (rgb[2] as usize >> CELL_SHIFT);
        let cell = &mut self.cells[id];
        if cell.count == 0 {
            self.occupied.push(id as u32);
        }
        cell.count += count;
        cell.sum[0] += count * u64::from(rgb[0]);
        cell.sum[1] += count * u64::from(rgb[1]);
        cell.sum[2] += count * u64::from(rgb[2]);
    }

    /// Forget everything gathered (cost follows what was gathered).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn clear(&mut self) {
        for (rgb, _) in self.colors.drain(..) {
            let key = pack(rgb);
            let mut slot = (key.wrapping_mul(0x9E37_79B1) >> 18) as usize;
            while self.slots[slot] != key {
                slot = (slot + 1) & (DISTINCT_SLOTS - 1);
            }
            self.slots[slot] = NO_COLOR;
        }
        if self.pooled {
            // `pool` emptied `colors` without freeing their slots.
            self.slots.fill(NO_COLOR);
        }
        for id in self.occupied.drain(..) {
            self.cells[id as usize] = Cell::default();
        }
        self.pooled = false;
        self.last = NO_COLOR;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.colors.is_empty() && self.occupied.is_empty()
    }

    /// The distinct colours in ascending R, G, B order when there are at
    /// most `limit` of them.
    pub(crate) fn exact(&self, limit: usize) -> Option<Vec<[u8; 3]>> {
        if self.pooled || self.colors.len() > limit {
            return None;
        }
        let mut colors: Vec<[u8; 3]> = self.colors.iter().map(|c| c.0).collect();
        colors.sort_unstable();
        Some(colors)
    }

    /// The census as weighted points of `metric`, in a fixed order
    /// (ascending colour, or ascending cell).
    pub(crate) fn samples(&self, metric: Metric) -> Vec<Sample> {
        if !self.pooled {
            let mut colors = self.colors.clone();
            colors.sort_unstable();
            return colors
                .into_iter()
                .map(|(rgb, count)| Sample {
                    at: metric.point(rgb),
                    weight: count as f32,
                })
                .collect();
        }
        let mut ids = self.occupied.clone();
        ids.sort_unstable();
        ids.iter()
            .map(|&id| {
                let c = &self.cells[id as usize];
                let n = c.count as f64;
                let mean = c.sum.map(|s| (s as f64 / n) as f32);
                Sample {
                    at: metric.point_f(mean),
                    weight: c.count as f32,
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------
// Box splitting + k-means.
// ---------------------------------------------------------------------

/// Running weight, weighted sum and weighted sum of squares of a group.
#[derive(Clone, Copy, Default)]
struct Moments {
    weight: f64,
    sum: [f64; 3],
    squares: f64,
}

impl Moments {
    fn add(&mut self, s: &Sample) {
        let w = f64::from(s.weight);
        let p = s.at.map(f64::from);
        self.weight += w;
        for (acc, v) in self.sum.iter_mut().zip(p) {
            *acc += w * v;
        }
        self.squares += w * (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]);
    }

    /// `|Σ w x|² / Σ w` — the part of the squared deviation a cut can
    /// change.
    fn explained(&self) -> f64 {
        if self.weight <= 0.0 {
            return 0.0;
        }
        (self.sum[0] * self.sum[0] + self.sum[1] * self.sum[1] + self.sum[2] * self.sum[2])
            / self.weight
    }

    /// Weighted sum of squared deviations from the mean.
    fn deviation(&self) -> f64 {
        (self.squares - self.explained()).max(0.0)
    }

    fn mean(&self) -> [f32; 3] {
        self.sum
            .map(|s| (s / self.weight.max(f64::MIN_POSITIVE)) as f32)
    }
}

/// A contiguous run of the working order: one box.
struct Span {
    start: usize,
    end: usize,
    moments: Moments,
}

fn moments_of(samples: &[Sample], ids: &[u32]) -> Moments {
    let mut m = Moments::default();
    for &i in ids {
        m.add(&samples[i as usize]);
    }
    m
}

/// Unit direction of greatest variance of a group (dominant eigenvector
/// of its weighted covariance matrix, by power iteration).
fn principal_direction(samples: &[Sample], ids: &[u32], m: &Moments) -> [f64; 3] {
    let mean = m.sum.map(|s| s / m.weight);
    let mut cov = [[0.0f64; 3]; 3];
    for &i in ids {
        let s = &samples[i as usize];
        let w = f64::from(s.weight);
        let d: [f64; 3] = std::array::from_fn(|k| f64::from(s.at[k]) - mean[k]);
        for r in 0..3 {
            for c in r..3 {
                cov[r][c] += w * d[r] * d[c];
            }
        }
    }
    cov[1][0] = cov[0][1];
    cov[2][0] = cov[0][2];
    cov[2][1] = cov[1][2];
    // Start from the coordinate axis with the largest variance.
    let mut v = [0.0f64; 3];
    let start = (0..3)
        .max_by(|&a, &b| cov[a][a].total_cmp(&cov[b][b]))
        .unwrap_or(0);
    v[start] = 1.0;
    for _ in 0..12 {
        let next: [f64; 3] =
            std::array::from_fn(|r| cov[r][0] * v[0] + cov[r][1] * v[1] + cov[r][2] * v[2]);
        let norm = (next[0] * next[0] + next[1] * next[1] + next[2] * next[2]).sqrt();
        if norm <= 0.0 || !norm.is_finite() {
            break;
        }
        v = next.map(|c| c / norm);
    }
    v
}

/// Reorder `ids` along the direction of greatest variance and return the
/// cut position (`1..len`) with the smallest summed squared deviation.
fn best_cut(samples: &[Sample], ids: &mut [u32], m: &Moments) -> usize {
    let dir = principal_direction(samples, ids, m);
    let mut keyed: Vec<(f32, u32)> = ids
        .iter()
        .map(|&i| {
            let p = samples[i as usize].at;
            let proj =
                dir[0] * f64::from(p[0]) + dir[1] * f64::from(p[1]) + dir[2] * f64::from(p[2]);
            (proj as f32, i)
        })
        .collect();
    keyed.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut left = Moments::default();
    let mut best = (f64::NEG_INFINITY, keyed.len() / 2);
    for (k, &(_, i)) in keyed.iter().enumerate().take(keyed.len() - 1) {
        left.add(&samples[i as usize]);
        let right = Moments {
            weight: m.weight - left.weight,
            sum: std::array::from_fn(|c| m.sum[c] - left.sum[c]),
            squares: 0.0,
        };
        let score = left.explained() + right.explained();
        if score > best.0 {
            best = (score, k + 1);
        }
    }
    for (slot, &(_, i)) in ids.iter_mut().zip(&keyed) {
        *slot = i;
    }
    best.1
}

/// Choose at most `budget` centres for `samples` by box splitting, then
/// refine them with up to `kmeans_rounds` rounds of weighted k-means.
pub(crate) fn select(samples: &[Sample], budget: usize, kmeans_rounds: usize) -> Vec<[f32; 3]> {
    if samples.is_empty() || budget == 0 {
        return Vec::new();
    }
    let mut order: Vec<u32> = (0..samples.len() as u32).collect();
    let mut spans = vec![Span {
        start: 0,
        end: samples.len(),
        moments: moments_of(samples, &order),
    }];
    while spans.len() < budget {
        let Some(pick) = spans
            .iter()
            .enumerate()
            .filter(|(_, s)| s.end - s.start > 1 && s.moments.deviation() > 1e-6)
            .max_by(|a, b| a.1.moments.deviation().total_cmp(&b.1.moments.deviation()))
            .map(|(i, _)| i)
        else {
            break;
        };
        let (start, end) = (spans[pick].start, spans[pick].end);
        let cut = start + best_cut(samples, &mut order[start..end], &spans[pick].moments);
        let left = moments_of(samples, &order[start..cut]);
        let right = moments_of(samples, &order[cut..end]);
        spans[pick] = Span {
            start,
            end: cut,
            moments: left,
        };
        spans.push(Span {
            start: cut,
            end,
            moments: right,
        });
    }
    let mut centres: Vec<[f32; 3]> = spans.iter().map(|s| s.moments.mean()).collect();

    for _ in 0..kmeans_rounds {
        let mut search = Nearest::new(&centres);
        let mut groups = vec![Moments::default(); centres.len()];
        let mut last = 0;
        for s in samples {
            last = search.find_fast(s.at, last).0;
            groups[last].add(s);
        }
        let mut moved = 0.0f32;
        for (centre, g) in centres.iter_mut().zip(&groups) {
            if g.weight > 0.0 {
                let next = g.mean();
                let d = [
                    next[0] - centre[0],
                    next[1] - centre[1],
                    next[2] - centre[2],
                ];
                moved = moved.max(d[0] * d[0] + d[1] * d[1] + d[2] * d[2]);
                *centre = next;
            }
        }
        if moved < 1e-3 {
            break;
        }
    }
    centres
}

/// Weighted mean squared distance from each sample to its nearest
/// centre.
#[cfg(test)]
pub(crate) fn mean_squared_error(samples: &[Sample], centres: &[[f32; 3]]) -> f64 {
    if samples.is_empty() || centres.is_empty() {
        return 0.0;
    }
    let search = Nearest::new(centres);
    let (mut err, mut weight) = (0.0f64, 0.0f64);
    for s in samples {
        err += f64::from(s.weight) * f64::from(search.find(s.at).1);
        weight += f64::from(s.weight);
    }
    err / weight.max(f64::MIN_POSITIVE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(r: f32, g: f32, b: f32, weight: f32) -> Sample {
        Sample {
            at: [r, g, b],
            weight,
        }
    }

    #[test]
    fn census_counts_exactly_then_pools() {
        let mut c = Census::new();
        assert!(c.is_empty());
        for rgb in [[9, 9, 9], [1, 2, 3], [1, 2, 3], [9, 9, 9], [1, 2, 3]] {
            c.add(rgb);
        }
        assert_eq!(c.exact(2), Some(vec![[1, 2, 3], [9, 9, 9]]));
        assert_eq!(c.exact(1), None);
        let s = c.samples(Metric::Rgb);
        assert_eq!((s[0].at, s[0].weight), ([1.0, 2.0, 3.0], 3.0));
        assert_eq!((s[1].at, s[1].weight), ([9.0, 9.0, 9.0], 2.0));

        // Past the distinct limit the counts move into cells whose means
        // are exact.
        c.clear();
        assert!(c.is_empty());
        let mut total = 0u64;
        for i in 0..(DISTINCT_LIMIT as u32 + 500) {
            let rgb = [(i % 256) as u8, (i / 256) as u8, 77];
            c.add(rgb);
            c.add(rgb);
            total += 2;
        }
        assert_eq!(c.exact(256), None);
        let s = c.samples(Metric::Rgb);
        assert!(s.len() < DISTINCT_LIMIT);
        assert_eq!(s.iter().map(|p| p.weight as u64).sum::<u64>(), total);
        let cell = s.iter().find(|p| p.at[0] < 4.0 && p.at[1] < 4.0).unwrap();
        assert_eq!((cell.at, cell.weight), ([1.5, 1.5, 77.0], 32.0));

        // A cleared census counts exactly again.
        c.clear();
        c.add([5, 5, 5]);
        assert_eq!(c.exact(4), Some(vec![[5, 5, 5]]));
    }

    #[test]
    fn two_clusters_split_between_them() {
        let samples = vec![
            pt(0.0, 0.0, 0.0, 5.0),
            pt(2.0, 0.0, 0.0, 5.0),
            pt(100.0, 100.0, 0.0, 1.0),
            pt(102.0, 100.0, 0.0, 1.0),
        ];
        let mut c = select(&samples, 2, 0);
        c.sort_by(|a, b| a[0].total_cmp(&b[0]));
        assert_eq!(c, vec![[1.0, 0.0, 0.0], [101.0, 100.0, 0.0]]);
    }

    #[test]
    fn budget_at_least_sample_count_reproduces_every_sample() {
        let samples: Vec<Sample> = (0..20)
            .map(|i| pt(i as f32 * 3.0, (i * i) as f32, 7.0, 1.0))
            .collect();
        let c = select(&samples, 64, 4);
        assert_eq!(c.len(), 20);
        assert!(mean_squared_error(&samples, &c) < 1e-9);
    }

    #[test]
    fn kmeans_never_raises_the_error() {
        let mut s = 11u32;
        let mut next = || {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 8) % 256
        };
        let samples: Vec<Sample> = (0..3000)
            .map(|_| {
                pt(
                    next() as f32,
                    next() as f32,
                    next() as f32,
                    1.0 + (next() % 9) as f32,
                )
            })
            .collect();
        let mut last = f64::INFINITY;
        for rounds in [0, 1, 2, 4, 8] {
            let e = mean_squared_error(&samples, &select(&samples, 32, rounds));
            assert!(e <= last + 1e-6, "rounds {rounds}: {e} > {last}");
            last = e;
        }
    }
}
