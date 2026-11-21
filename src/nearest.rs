//! Exact nearest-point search over a small point set (a colour table),
//! and the per-colour memo the index-plane mappers put in front of it.
//!
//! The search sorts the points along the coordinate with the widest
//! spread and walks outwards from the query's position in that order.
//! A point whose distance along that one coordinate already exceeds the
//! best full distance found so far cannot win — and neither can any
//! point beyond it — so the walk stops there. The result is exactly the
//! `argmin` of the squared Euclidean distance, with ties resolved to the
//! lowest original index (what a linear scan in index order returns).
//!
//! Most queries end sooner. If the query lies closer to a point `p` than
//! half the distance from `p` to its own nearest neighbour, the triangle
//! inequality puts every other point `f` strictly farther away:
//! `d(q, f) ≥ d(p, f) − d(q, p) > d(q, p)`. So each point carries a
//! quarter of its squared nearest-neighbour distance, the search returns
//! the moment a candidate falls under its own bound, and a caller's
//! guess (the answer for the previous pixel) is tested against that
//! bound before anything else.
//!
//! For long streams of queries (every pixel of a picture) a uniform grid
//! over the points' bounding box narrows the candidates further. A grid
//! cell with centre `c` and half-diagonal `r` lists every point within
//! `d_min(c) + 2r` of `c`, where `d_min(c)` is the distance from `c` to
//! its nearest point `e`. That list contains the nearest point `p` of
//! any query `q` inside the cell: `d(c, p) ≤ d(c, q) + d(q, p) ≤ r +
//! d(q, e) ≤ r + d(c, e) + r`. The grid is set up after a couple of
//! thousand queries and a cell's list is built the fourth time a query
//! lands in it (until then the sorted walk answers), so a short stream
//! never pays for lists it would not use, and a query outside the grid
//! falls back to the walk. Either way the answer is the same exact
//! `argmin`.

use crate::colorspace::Metric;
use crate::image::Rgb;

pub(crate) struct Nearest {
    /// Coordinate the points are sorted on.
    axis: usize,
    /// The sort coordinate of each point, ascending.
    keys: Vec<f32>,
    /// The points, in the same order as `keys`.
    points: Vec<[f32; 3]>,
    /// Original index of each sorted point.
    origin: Vec<u16>,
    /// Sorted position of each original index.
    slot: Vec<u16>,
    /// Per sorted point: a query whose squared distance to it is below
    /// this has it as its only nearest point (a quarter of the squared
    /// distance to the point's nearest neighbour, shaved for rounding).
    certain: Vec<f32>,
    /// Candidate lists per region of space, set up once enough queries
    /// have come through [`Self::find_fast`] to be worth it.
    grid: GridState,
}

enum GridState {
    /// Queries served so far by the sorted walk.
    Waiting(u32),
    Ready(Grid),
    /// Too few points, or a degenerate box.
    Unavailable,
}

/// Queries after which the grid is set up.
const GRID_AFTER: u32 = 2048;

/// Grid cells aimed at per point.
const CELLS_PER_POINT: f32 = 128.0;
/// Most cells a grid may have.
const MAX_CELLS: usize = 1 << 16;
/// Queries a cell answers by the sorted walk before its list is built.
const BUILD_ON_HIT: u8 = 4;

#[derive(Clone, Copy, Default)]
struct GridCell {
    /// Start of the cell's candidates in the arena.
    start: u32,
    /// Number of candidates; `0` while the list is not built.
    len: u16,
    hits: u8,
}

struct Grid {
    /// Corner of the grid.
    low: [f32; 3],
    /// Cells per unit length.
    scale: f32,
    dims: [usize; 3],
    /// Twice the half-diagonal of a cell.
    reach: f32,
    cells: Vec<GridCell>,
    /// The candidates of every built cell: position and original index.
    arena: Vec<([f32; 3], u16)>,
}

impl Grid {
    fn new(points: &[[f32; 3]]) -> Option<Self> {
        if points.len() < 16 {
            return None;
        }
        let mut low = [f32::INFINITY; 3];
        let mut high = [f32::NEG_INFINITY; 3];
        for p in points {
            for a in 0..3 {
                low[a] = low[a].min(p[a]);
                high[a] = high[a].max(p[a]);
            }
        }
        // Cell edge: the box (at least one unit thick each way) shared
        // out at `CELLS_PER_POINT` cells per point.
        let volume: f32 = (0..3).map(|a| (high[a] - low[a]).max(1.0)).product();
        let mut edge = (volume / (points.len() as f32 * CELLS_PER_POINT)).cbrt();
        if !edge.is_finite() || edge <= 0.0 {
            return None;
        }
        // Two cells of margin all round catch queries just outside.
        let dims_for = |edge: f32| -> [usize; 3] {
            std::array::from_fn(|a| ((high[a] - low[a]) / edge).ceil() as usize + 4)
        };
        let mut dims = dims_for(edge);
        let total = dims[0].saturating_mul(dims[1]).saturating_mul(dims[2]);
        if total > MAX_CELLS {
            edge *= (total as f32 / MAX_CELLS as f32).cbrt() * 1.1;
            dims = dims_for(edge);
        }
        let total = dims[0].checked_mul(dims[1])?.checked_mul(dims[2])?;
        if total > 2 * MAX_CELLS {
            return None;
        }
        Some(Grid {
            low: low.map(|v| v - 2.0 * edge),
            scale: 1.0 / edge,
            dims,
            reach: edge * 3.0f32.sqrt(),
            cells: vec![GridCell::default(); total],
            arena: Vec::new(),
        })
    }

    /// The cell holding `q`.
    #[inline]
    fn locate(&self, q: [f32; 3]) -> Option<usize> {
        let mut at = [0usize; 3];
        for a in 0..3 {
            let f = (q[a] - self.low[a]) * self.scale;
            // Also false for a NaN.
            if !(f >= 0.0 && f < self.dims[a] as f32) {
                return None;
            }
            at[a] = f as usize;
        }
        Some((at[2] * self.dims[1] + at[1]) * self.dims[0] + at[0])
    }

    /// Centre of a cell.
    fn centre(&self, cell: usize) -> [f32; 3] {
        let at = [
            cell % self.dims[0],
            cell / self.dims[0] % self.dims[1],
            cell / (self.dims[0] * self.dims[1]),
        ];
        std::array::from_fn(|a| self.low[a] + (at[a] as f32 + 0.5) / self.scale)
    }
}

fn squared_distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
}

impl Nearest {
    pub(crate) fn new(points: &[[f32; 3]]) -> Self {
        // Widest coordinate (largest max − min) prunes best.
        let mut axis = 0;
        let mut widest = -1.0f32;
        for a in 0..3 {
            let lo = points.iter().map(|p| p[a]).fold(f32::INFINITY, f32::min);
            let hi = points
                .iter()
                .map(|p| p[a])
                .fold(f32::NEG_INFINITY, f32::max);
            if hi - lo > widest {
                widest = hi - lo;
                axis = a;
            }
        }
        let mut order: Vec<u16> = (0..points.len() as u16).collect();
        order.sort_by(|&i, &j| {
            points[i as usize][axis]
                .total_cmp(&points[j as usize][axis])
                .then(i.cmp(&j))
        });
        let sorted: Vec<[f32; 3]> = order.iter().map(|&i| points[i as usize]).collect();
        let certain = (0..sorted.len())
            .map(|i| {
                let gap = (0..sorted.len())
                    .filter(|&j| j != i)
                    .map(|j| squared_distance(sorted[i], sorted[j]))
                    .fold(f32::INFINITY, f32::min);
                gap * 0.2499
            })
            .collect();
        let mut slot = vec![0u16; order.len()];
        for (at, &origin) in order.iter().enumerate() {
            slot[origin as usize] = at as u16;
        }
        Nearest {
            axis,
            keys: order.iter().map(|&i| points[i as usize][axis]).collect(),
            points: sorted,
            origin: order,
            slot,
            certain,
            grid: GridState::Waiting(0),
        }
    }

    /// [`Self::find_near`] through the grid of candidate lists (see the
    /// module docs); the same answer, faster over many queries.
    #[inline]
    pub(crate) fn find_fast(&mut self, q: [f32; 3], guess: usize) -> (usize, f32) {
        if let Some(&slot) = self.slot.get(guess) {
            let d = squared_distance(self.points[slot as usize], q);
            if d < self.certain[slot as usize] {
                return (guess, d);
            }
        }
        let grid = match &mut self.grid {
            GridState::Ready(grid) => grid,
            GridState::Unavailable => return self.find(q),
            GridState::Waiting(served) => {
                *served += 1;
                if *served >= GRID_AFTER {
                    self.grid = match Grid::new(&self.points) {
                        Some(grid) => GridState::Ready(grid),
                        None => GridState::Unavailable,
                    };
                }
                return self.find(q);
            }
        };
        let Some(cell) = grid.locate(q) else {
            return self.find(q);
        };
        let state = &mut grid.cells[cell];
        if state.len == 0 {
            state.hits += 1;
            if state.hits < BUILD_ON_HIT {
                return self.find(q);
            }
            // Every point within `d_min + reach` of the centre.
            let centre = grid.centre(cell);
            let nearest = self
                .points
                .iter()
                .map(|&p| squared_distance(p, centre))
                .fold(f32::INFINITY, f32::min);
            let bound = nearest.sqrt() + grid.reach;
            let bound = bound * bound * 1.0001 + 1e-4;
            let start = grid.arena.len();
            grid.arena.extend(
                self.points
                    .iter()
                    .zip(&self.origin)
                    .filter(|(p, _)| squared_distance(**p, centre) <= bound)
                    .map(|(&p, &o)| (p, o)),
            );
            let state = &mut grid.cells[cell];
            state.start = start as u32;
            state.len = (grid.arena.len() - start) as u16;
        }
        let state = grid.cells[cell];
        let candidates =
            &grid.arena[state.start as usize..state.start as usize + state.len as usize];
        let mut best = f32::INFINITY;
        let mut best_origin = u16::MAX;
        for &(p, o) in candidates {
            let d = squared_distance(p, q);
            if d < best || (d == best && o < best_origin) {
                best = d;
                best_origin = o;
            }
        }
        (best_origin as usize, best)
    }

    /// [`Self::find`], first trying `guess` (an original index, such as
    /// the answer to a nearby query).
    #[cfg(test)]
    pub(crate) fn find_near(&self, q: [f32; 3], guess: usize) -> (usize, f32) {
        if let Some(&slot) = self.slot.get(guess) {
            let d = squared_distance(self.points[slot as usize], q);
            if d < self.certain[slot as usize] {
                return (guess, d);
            }
        }
        self.find(q)
    }

    /// Index (into the slice given to [`Self::new`]) of the point nearest
    /// to `q`, and its squared distance. `(0, ∞)` for an empty set.
    pub(crate) fn find(&self, q: [f32; 3]) -> (usize, f32) {
        let n = self.keys.len();
        let qa = q[self.axis];
        let mut hi = self.keys.partition_point(|&k| k < qa);
        let mut lo = hi;
        let mut best = f32::INFINITY;
        let mut best_origin = u16::MAX;
        // `true` when the candidate is certainly the answer.
        let consider = |slot: usize, best: &mut f32, best_origin: &mut u16| {
            let d = squared_distance(self.points[slot], q);
            let o = self.origin[slot];
            if d < *best || (d == *best && o < *best_origin) {
                *best = d;
                *best_origin = o;
            }
            d < self.certain[slot]
        };
        loop {
            let mut progressed = false;
            if hi < n {
                let along = self.keys[hi] - qa;
                if along * along <= best {
                    if consider(hi, &mut best, &mut best_origin) {
                        break;
                    }
                    hi += 1;
                    progressed = true;
                } else {
                    hi = n;
                }
            }
            if lo > 0 {
                let along = qa - self.keys[lo - 1];
                if along * along <= best {
                    if consider(lo - 1, &mut best, &mut best_origin) {
                        break;
                    }
                    lo -= 1;
                    progressed = true;
                } else {
                    lo = 0;
                }
            }
            if !progressed {
                break;
            }
        }
        if best_origin == u16::MAX {
            (0, f32::INFINITY)
        } else {
            (best_origin as usize, best)
        }
    }
}

/// log2 of the memo size.
const MEMO_BITS: u32 = 15;
/// Marks a filled memo slot (the low 32 bits hold colour and index).
const MEMO_FILLED: u64 = 1 << 32;

/// Maps 8-bit colours onto the nearest entry of one colour table.
///
/// Every answer is exact for the chosen [`Metric`]; a direct-mapped memo
/// keyed on the 24-bit colour short-circuits repeats (flat regions, the
/// same colour in a later frame).
pub(crate) struct Mapper {
    metric: Metric,
    nearest: Nearest,
    /// The table's colours as stored.
    colors: Vec<[u8; 3]>,
    memo: Vec<u64>,
    /// The previous answer: neighbouring pixels usually share it.
    last: usize,
}

impl Mapper {
    pub(crate) fn new(palette: &[Rgb], metric: Metric) -> Self {
        let points: Vec<[f32; 3]> = palette
            .iter()
            .map(|c| metric.point([c.r, c.g, c.b]))
            .collect();
        Mapper {
            metric,
            nearest: Nearest::new(&points),
            colors: palette.iter().map(|c| [c.r, c.g, c.b]).collect(),
            memo: vec![0; 1 << MEMO_BITS],
            last: 0,
        }
    }

    /// Index of the table entry nearest to `rgb` (`0` for an empty table).
    #[inline]
    pub(crate) fn map(&mut self, rgb: [u8; 3]) -> u8 {
        let key = u32::from(rgb[0]) << 16 | u32::from(rgb[1]) << 8 | u32::from(rgb[2]);
        let slot = (key.wrapping_mul(0x9E37_79B1) >> (32 - MEMO_BITS)) as usize;
        let held = self.memo[slot];
        if held >> 8 == (MEMO_FILLED >> 8 | u64::from(key)) {
            return held as u8;
        }
        let (mut index, distance) = self.nearest.find_fast(self.metric.point(rgb), self.last);
        // A colour that is itself a table entry maps to that entry, even
        // if another entry should land on the very same point of the
        // metric space.
        if distance == 0.0
            && self.colors.get(index) != Some(&rgb)
            && let Some(same) = self.colors.iter().position(|&c| c == rgb)
        {
            index = same;
        }
        self.last = index;
        let index = index as u8;
        self.memo[slot] = MEMO_FILLED | u64::from(key) << 8 | u64::from(index);
        index
    }
}

/// How many of each entry's nearest neighbours are tried as the second
/// colour of a two-entry mix.
const MIX_NEIGHBOURS: usize = 10;
/// Weight of a mix's pattern energy against its colour error (see
/// [`PairMapper`]).
const PATTERN_WEIGHT: f32 = 1.0 / 8.0;

/// A colour as a mix of two table entries: `second` in a `share / 256`
/// fraction of the pixels, `first` in the rest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Mix {
    pub(crate) first: u8,
    pub(crate) second: u8,
    /// `0..=255`; `0` means the colour is rendered with `first` alone.
    pub(crate) share: u8,
}

/// Maps 8-bit colours onto the two-entry mix that renders them best —
/// what an ordered dither thresholds per position.
///
/// For a colour `c` with nearest entry `p`, each of `p`'s nearest
/// neighbours `q` is tried: the point of the segment `p → q` closest to
/// `c` is `p + t (q − p)` with `t = (c − p)·(q − p) / |q − p|²` clamped
/// to `0..=1`. Rendering `c` as that mix leaves a colour error (the
/// distance from `c` to the point) and adds a two-colour pattern whose
/// variance is `t (1 − t) |q − p|²`. The neighbour minimising
/// `error² + PATTERN_WEIGHT × variance` is taken, against the plain
/// nearest entry (`t = 0`, no pattern). A colour whose nearest entry is
/// within the metric's visibility floor (and so any colour that is itself
/// a table entry) never dithers — a pattern there would cost bytes for a
/// difference nobody sees — and a colour between two close entries mixes
/// them in proportion. Distances, projections and neighbour lists
/// are all taken in the mapper's metric space; neighbouring entries are
/// close enough for a mix to be near-linear there.
pub(crate) struct PairMapper {
    mapper: Mapper,
    metric: Metric,
    /// The table entries as points of the metric space.
    at: Vec<[f32; 3]>,
    /// `MIX_NEIGHBOURS` nearest other entries of each entry, padded with
    /// the entry itself.
    neighbours: Vec<[u8; MIX_NEIGHBOURS]>,
    memo: Vec<u64>,
    /// Mix strength in percent (`100` = the full proportion).
    strength: u32,
}

impl PairMapper {
    pub(crate) fn new(palette: &[Rgb], metric: Metric, strength: u32) -> Self {
        let at: Vec<[f32; 3]> = palette
            .iter()
            .map(|c| metric.point([c.r, c.g, c.b]))
            .collect();
        let neighbours = (0..at.len())
            .map(|i| {
                let mut others: Vec<(f32, usize)> = (0..at.len())
                    .filter(|&j| j != i && palette[j] != palette[i])
                    .map(|j| (squared_distance(at[i], at[j]), j))
                    .collect();
                // Only the nearest few are wanted: partition, then order
                // just those.
                let by_distance =
                    |a: &(f32, usize), b: &(f32, usize)| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1));
                if others.len() > MIX_NEIGHBOURS {
                    others.select_nth_unstable_by(MIX_NEIGHBOURS - 1, by_distance);
                    others.truncate(MIX_NEIGHBOURS);
                }
                others.sort_by(by_distance);
                std::array::from_fn(|k| others.get(k).map_or(i, |o| o.1) as u8)
            })
            .collect();
        PairMapper {
            mapper: Mapper::new(palette, metric),
            metric,
            at,
            neighbours,
            memo: vec![0; 1 << MEMO_BITS],
            strength: strength.min(100),
        }
    }

    /// The plain nearest entry (no mixing).
    #[inline]
    pub(crate) fn nearest(&mut self, rgb: [u8; 3]) -> u8 {
        self.mapper.map(rgb)
    }

    #[inline]
    pub(crate) fn mix(&mut self, rgb: [u8; 3]) -> Mix {
        let key = u32::from(rgb[0]) << 16 | u32::from(rgb[1]) << 8 | u32::from(rgb[2]);
        let slot = (key.wrapping_mul(0x9E37_79B1) >> (32 - MEMO_BITS)) as usize;
        let held = self.memo[slot];
        if held >> 24 == (1 << 24 | u64::from(key)) {
            return Mix {
                first: held as u8,
                second: (held >> 8) as u8,
                share: (held >> 16) as u8,
            };
        }
        let mix = self.solve(rgb);
        self.memo[slot] = (1 << 24 | u64::from(key)) << 24
            | u64::from(mix.share) << 16
            | u64::from(mix.second) << 8
            | u64::from(mix.first);
        mix
    }

    fn solve(&mut self, rgb: [u8; 3]) -> Mix {
        let first = self.mapper.map(rgb);
        let plain = Mix {
            first,
            second: first,
            share: 0,
        };
        if self.at.is_empty() || self.strength == 0 {
            return plain;
        }
        let c = self.metric.point(rgb);
        let p = self.at[first as usize];
        let to_c = [c[0] - p[0], c[1] - p[1], c[2] - p[2]];
        let mut best_score = squared_distance(c, p);
        if best_score <= self.metric.visibility_floor_squared() {
            return plain;
        }
        let mut best = plain;
        for &n in &self.neighbours[first as usize] {
            if n == first {
                break;
            }
            let q = self.at[n as usize];
            let seg = [q[0] - p[0], q[1] - p[1], q[2] - p[2]];
            let len2 = seg[0] * seg[0] + seg[1] * seg[1] + seg[2] * seg[2];
            let t =
                ((to_c[0] * seg[0] + to_c[1] * seg[1] + to_c[2] * seg[2]) / len2).clamp(0.0, 1.0);
            let at = [p[0] + t * seg[0], p[1] + t * seg[1], p[2] + t * seg[2]];
            let score = squared_distance(c, at) + PATTERN_WEIGHT * t * (1.0 - t) * len2;
            if score < best_score {
                best_score = score;
                let share = (t * 256.0 * self.strength as f32 / 100.0) as u32;
                best = Mix {
                    first,
                    second: n,
                    share: share.min(255) as u8,
                };
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(state: &mut u32) -> u32 {
        *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *state >> 8
    }

    #[test]
    fn matches_a_linear_scan_including_ties() {
        let mut s = 7u32;
        for round in 0..40 {
            let n = 1 + (lcg(&mut s) % 256) as usize;
            // A coarse grid makes equal distances (ties) common.
            let step = if round % 2 == 0 { 1 } else { 32 };
            let pts: Vec<[f32; 3]> = (0..n)
                .map(|_| std::array::from_fn(|_| ((lcg(&mut s) % 256) / step * step) as f32))
                .collect();
            let search = Nearest::new(&pts);
            for _ in 0..300 {
                let q: [f32; 3] =
                    std::array::from_fn(|_| ((lcg(&mut s) % 256) / step * step) as f32);
                let mut want = (0usize, f32::INFINITY);
                for (i, p) in pts.iter().enumerate() {
                    let d = squared_distance(*p, q);
                    if d < want.1 {
                        want = (i, d);
                    }
                }
                assert_eq!(search.find(q), want);
                let guess = (lcg(&mut s) as usize) % (n + 2);
                assert_eq!(search.find_near(q, guess), want, "guess {guess}");
            }
            // The grid path: repeated queries so that cells get built,
            // including points outside the bounding box.
            let mut search = Nearest::new(&pts);
            for i in 0..(GRID_AFTER + 3000) {
                let base = lcg(&mut s);
                let q: [f32; 3] = std::array::from_fn(|k| {
                    let v = ((base >> (k * 5)) % 24) as f32 * 11.0 - 4.0;
                    if i % 97 == 0 { v * 3.0 - 100.0 } else { v }
                });
                let mut want = (0usize, f32::INFINITY);
                for (i, p) in pts.iter().enumerate() {
                    let d = squared_distance(*p, q);
                    if d < want.1 {
                        want = (i, d);
                    }
                }
                assert_eq!(
                    search.find_fast(q, i as usize % (n + 1)),
                    want,
                    "grid, {n} points"
                );
            }
        }
    }

    #[test]
    fn empty_set_maps_to_zero() {
        assert_eq!(Nearest::new(&[]).find([1.0, 2.0, 3.0]).0, 0);
        assert_eq!(Mapper::new(&[], Metric::Lab).map([9, 9, 9]), 0);
    }

    #[test]
    fn mixes_are_proportional_and_exact_colours_stay_plain() {
        let pal = [
            Rgb::new(0, 0, 0),
            Rgb::new(100, 100, 100),
            Rgb::new(255, 0, 0),
        ];
        let mut m = PairMapper::new(&pal, Metric::Rgb, 100);
        // A table colour never mixes.
        assert_eq!(m.mix([100, 100, 100]).share, 0);
        // A quarter of the way from black to grey: 25 % grey.
        let q = m.mix([25, 25, 25]);
        assert_eq!((q.first, q.second, q.share), (0, 1, 64));
        assert_eq!(m.mix([25, 25, 25]), q, "memo answer");
        // Past the midpoint the nearer entry is the base.
        let r = m.mix([75, 75, 75]);
        assert_eq!((r.first, r.second, r.share), (1, 0, 64));
        // Half strength halves the share.
        let mut half = PairMapper::new(&pal, Metric::Rgb, 50);
        assert_eq!(half.mix([25, 25, 25]).share, 32);
        assert_eq!(
            PairMapper::new(&[], Metric::Rgb, 100).mix([1, 2, 3]).share,
            0
        );
    }

    #[test]
    fn memo_returns_the_search_answer() {
        let pal: Vec<Rgb> = (0..200u32)
            .map(|i| Rgb::new((i * 7) as u8, (i * 13) as u8, (i * 29) as u8))
            .collect();
        for metric in [Metric::Rgb, Metric::Lab] {
            let mut m = Mapper::new(&pal, metric);
            let mut s = 3u32;
            for _ in 0..5000 {
                let c = [
                    lcg(&mut s) as u8,
                    lcg(&mut s) as u8,
                    (lcg(&mut s) % 4) as u8,
                ];
                let direct = m.nearest.find(metric.point(c)).0 as u8;
                assert_eq!(m.map(c), direct);
                assert_eq!(m.map(c), direct);
            }
        }
    }
}
