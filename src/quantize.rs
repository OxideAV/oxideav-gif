//! Truecolor → palette-indexed reduction for the GIF encoder.
//!
//! The CompuServe specifications constrain the *output* of this
//! module but say nothing about how to produce it: §19 ("Global Color
//! Table") and §21 ("Local Color Table") cap a colour table at 256
//! entries (`2^(N+1)`, `N` in `0..=7`), and §22 ("Table-Based Image
//! Data") stores one palette index per pixel. A GIF therefore cannot
//! carry truecolor; an encoder fed arbitrary 24-bit RGB has to choose
//! a representative ≤256-entry palette and map every pixel to its
//! nearest entry.
//!
//! This module is that reduction. The colour-selection algorithm is
//! **median cut** (a general, format-independent technique: repeatedly
//! split the colour box with the largest spread along its longest axis
//! until the requested number of boxes exists, then average each box to
//! one palette entry). Median cut is a textbook quantiser, not anything
//! GIF-specific — the GIF spec's only stake is that the result is a
//! ≤256-entry §19/§21 table plus a §22 index plane, which every public
//! entry point here guarantees.
//!
//! # Alpha handling
//!
//! GIF has no per-pixel alpha — only a single §23.c.viii Transparency
//! Index. [`quantize_rgba`] folds alpha down to one bit: a pixel whose
//! alpha is below [`ALPHA_OPAQUE_THRESHOLD`] is treated as transparent
//! and routed to a reserved palette slot that the caller can mark as
//! the §23.c.viii Transparent Index; opaque pixels are quantised on
//! their RGB only. [`quantize_rgb`] is the no-alpha entry point for
//! callers that have already resolved transparency.

use crate::error::{Error, Result};
use crate::image::Rgb;

/// Alpha values strictly below this are treated as transparent by
/// [`quantize_rgba`]. GIF's §23.c.viii transparency is a single index,
/// not a coverage value, so any partial coverage collapses to "fully
/// transparent" or "fully opaque" at this cut. 128 puts the boundary at
/// the midpoint of the 8-bit alpha range.
pub const ALPHA_OPAQUE_THRESHOLD: u8 = 128;

/// Largest colour table a §19 / §21 GIF palette can hold (`2^(7+1)`).
pub const MAX_PALETTE: usize = 256;

/// Result of quantising an RGBA frame: a §19/§21 palette, a §22 index
/// plane (`width * height` entries, row-major top-to-bottom), and the
/// reserved §23.c.viii Transparency Index when the input carried any
/// transparent pixel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quantized {
    /// The selected colour table — at most [`MAX_PALETTE`] entries, and
    /// always at least one (a degenerate all-transparent or empty input
    /// still yields a single placeholder entry so the §22 indices are
    /// in range).
    pub palette: Vec<Rgb>,
    /// One palette index per pixel, row-major, length `width * height`.
    pub indices: Vec<u8>,
    /// `Some(i)` when the input had at least one transparent pixel; `i`
    /// is the palette slot every such pixel maps to, suitable for the
    /// §23.c.viii Transparency Index. `None` when the frame was fully
    /// opaque.
    pub transparent_index: Option<u8>,
}

/// Quantise an RGBA pixel buffer to a GIF palette + index plane,
/// reducing to at most `max_colors` palette entries.
///
/// `rgba` is `width * height` 4-byte `[R, G, B, A]` pixels in row-major
/// top-to-bottom order. Pixels whose alpha is below
/// [`ALPHA_OPAQUE_THRESHOLD`] are routed to a single reserved palette
/// slot returned as [`Quantized::transparent_index`]; the remaining
/// opaque pixels are reduced with median cut over their RGB channels.
///
/// `max_colors` is clamped to `1..=256`. When the frame carries
/// transparent pixels one slot of the budget is reserved for them, so
/// the opaque colours get `max_colors - 1` entries.
///
/// # Errors
///
/// * `rgba.len()` is not exactly `width * height * 4`.
/// * `width * height` overflows `usize`.
pub fn quantize_rgba(
    rgba: &[u8],
    width: usize,
    height: usize,
    max_colors: usize,
) -> Result<Quantized> {
    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(|| Error::InvalidInput("quantize: width*height overflow".into()))?;
    if rgba.len() != pixel_count * 4 {
        return Err(Error::InvalidInput(format!(
            "quantize: rgba length {} != width*height*4 ({})",
            rgba.len(),
            pixel_count * 4
        )));
    }

    let budget = max_colors.clamp(1, MAX_PALETTE);

    // Split pixels into opaque (quantised on RGB) and transparent (one
    // shared reserved slot). Collect the opaque samples for median cut;
    // remember each pixel's category so we can stitch the index plane
    // back together in source order afterwards.
    let mut opaque_samples: Vec<[u8; 3]> = Vec::new();
    let mut is_transparent: Vec<bool> = Vec::with_capacity(pixel_count);
    for px in rgba.chunks_exact(4) {
        let transparent = px[3] < ALPHA_OPAQUE_THRESHOLD;
        is_transparent.push(transparent);
        if !transparent {
            opaque_samples.push([px[0], px[1], px[2]]);
        }
    }
    let any_transparent = is_transparent.iter().any(|&t| t);

    // Reserve one slot for the transparent colour if needed. The opaque
    // colours get the remainder of the budget (at least one).
    let opaque_budget = if any_transparent {
        budget.saturating_sub(1).max(1)
    } else {
        budget
    };

    let (mut palette, opaque_lookup) = median_cut(&opaque_samples, opaque_budget);

    // Append the reserved transparent slot last so the opaque indices
    // computed above stay valid. Its RGB is arbitrary (a transparent
    // pixel is never displayed); use black.
    let transparent_index = if any_transparent {
        let idx = palette.len() as u8;
        palette.push(Rgb::new(0, 0, 0));
        Some(idx)
    } else {
        None
    };

    // Stitch the per-pixel index plane: transparent pixels take the
    // reserved slot, opaque pixels take their median-cut assignment.
    let mut indices = Vec::with_capacity(pixel_count);
    let mut opaque_iter = opaque_lookup.into_iter();
    for &transparent in &is_transparent {
        if transparent {
            indices.push(transparent_index.expect("any_transparent implies Some"));
        } else {
            indices.push(
                opaque_iter
                    .next()
                    .expect("one lookup entry per opaque pixel"),
            );
        }
    }

    Ok(Quantized {
        palette,
        indices,
        transparent_index,
    })
}

/// Quantise an opaque RGB pixel buffer to a GIF palette + index plane.
///
/// `rgb` is `width * height` 3-byte `[R, G, B]` pixels, row-major. No
/// transparency is considered; [`Quantized::transparent_index`] is
/// always `None`. `max_colors` is clamped to `1..=256`.
///
/// # Errors
///
/// * `rgb.len()` is not exactly `width * height * 3`.
/// * `width * height` overflows `usize`.
pub fn quantize_rgb(
    rgb: &[u8],
    width: usize,
    height: usize,
    max_colors: usize,
) -> Result<Quantized> {
    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(|| Error::InvalidInput("quantize: width*height overflow".into()))?;
    if rgb.len() != pixel_count * 3 {
        return Err(Error::InvalidInput(format!(
            "quantize: rgb length {} != width*height*3 ({})",
            rgb.len(),
            pixel_count * 3
        )));
    }
    let budget = max_colors.clamp(1, MAX_PALETTE);
    let samples: Vec<[u8; 3]> = rgb.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect();
    let (palette, indices) = median_cut(&samples, budget);
    Ok(Quantized {
        palette,
        indices,
        transparent_index: None,
    })
}

/// One axis-aligned colour box over a slice of the sample list.
struct ColorBox {
    /// Indices into the shared sample array that fall in this box.
    members: Vec<usize>,
    /// Per-channel `(min, max)` extent over the box members, cached so
    /// the split loop doesn't recompute it for every candidate.
    rmin: u8,
    rmax: u8,
    gmin: u8,
    gmax: u8,
    bmin: u8,
    bmax: u8,
}

impl ColorBox {
    fn from_members(samples: &[[u8; 3]], members: Vec<usize>) -> Self {
        let (mut rmin, mut gmin, mut bmin) = (255u8, 255u8, 255u8);
        let (mut rmax, mut gmax, mut bmax) = (0u8, 0u8, 0u8);
        for &i in &members {
            let [r, g, b] = samples[i];
            rmin = rmin.min(r);
            rmax = rmax.max(r);
            gmin = gmin.min(g);
            gmax = gmax.max(g);
            bmin = bmin.min(b);
            bmax = bmax.max(b);
        }
        Self {
            members,
            rmin,
            rmax,
            gmin,
            gmax,
            bmin,
            bmax,
        }
    }

    /// Length of the box's longest channel extent — used to pick which
    /// box to split next (largest spread first) and which axis to split
    /// it on.
    fn longest_extent(&self) -> u16 {
        let r = (self.rmax - self.rmin) as u16;
        let g = (self.gmax - self.gmin) as u16;
        let b = (self.bmax - self.bmin) as u16;
        r.max(g).max(b)
    }

    /// 0 = red, 1 = green, 2 = blue — the channel with the widest extent.
    fn longest_axis(&self) -> usize {
        let r = (self.rmax - self.rmin) as u16;
        let g = (self.gmax - self.gmin) as u16;
        let b = (self.bmax - self.bmin) as u16;
        if r >= g && r >= b {
            0
        } else if g >= b {
            1
        } else {
            2
        }
    }

    /// Average colour of the box members — the palette entry it
    /// contributes. Members is non-empty by construction.
    fn average(&self, samples: &[[u8; 3]]) -> Rgb {
        let (mut sr, mut sg, mut sb) = (0u64, 0u64, 0u64);
        for &i in &self.members {
            let [r, g, b] = samples[i];
            sr += r as u64;
            sg += g as u64;
            sb += b as u64;
        }
        let n = self.members.len() as u64;
        Rgb::new((sr / n) as u8, (sg / n) as u8, (sb / n) as u8)
    }
}

/// Median-cut quantiser core. Returns the selected palette plus, for
/// every sample in `samples` (in order), the index of the palette entry
/// it maps to.
///
/// `budget` is the maximum palette size, already clamped to `1..=256`.
/// An empty input yields a single placeholder black entry and no
/// lookups (the caller has no opaque pixels to map).
fn median_cut(samples: &[[u8; 3]], budget: usize) -> (Vec<Rgb>, Vec<u8>) {
    if samples.is_empty() {
        // No opaque pixels: still hand back a one-entry palette so a
        // §22 index plane built against it is in range, even though no
        // pixel will reference it.
        return (vec![Rgb::new(0, 0, 0)], Vec::new());
    }

    let budget = budget.clamp(1, MAX_PALETTE);

    // Start with one box holding every sample, then repeatedly split the
    // box with the largest extent until we hit the budget or no box can
    // be split further (a box of identical colours has zero extent).
    let mut boxes = vec![ColorBox::from_members(
        samples,
        (0..samples.len()).collect(),
    )];

    while boxes.len() < budget {
        // Pick the splittable box with the largest extent.
        let target = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.members.len() > 1 && b.longest_extent() > 0)
            .max_by_key(|(_, b)| b.longest_extent())
            .map(|(i, _)| i);
        let Some(idx) = target else {
            break; // every box is a single colour; nothing left to split
        };

        let target_box = boxes.swap_remove(idx);
        let axis = target_box.longest_axis();

        // Sort members along the chosen axis and split at the median so
        // each half gets roughly equal pixel mass.
        let mut members = target_box.members;
        members.sort_unstable_by_key(|&i| samples[i][axis]);
        let mid = members.len() / 2;
        let right = members.split_off(mid);
        let left = members;

        boxes.push(ColorBox::from_members(samples, left));
        boxes.push(ColorBox::from_members(samples, right));
    }

    // Each box contributes one palette entry (its average colour); every
    // member maps to that box's index.
    let mut palette = Vec::with_capacity(boxes.len());
    let mut lookup = vec![0u8; samples.len()];
    for (box_idx, b) in boxes.iter().enumerate() {
        palette.push(b.average(samples));
        for &member in &b.members {
            lookup[member] = box_idx as u8;
        }
    }

    (palette, lookup)
}

/// Map a single RGB colour to the nearest entry of an existing palette
/// by squared-Euclidean distance. Returns `0` for an empty palette (no
/// valid index exists; the caller is expected to guard against that).
///
/// Exposed so a caller compositing successive frames against one shared
/// palette can map new pixels without re-running the full quantiser.
pub fn nearest_index(palette: &[Rgb], color: Rgb) -> u8 {
    let mut best = 0usize;
    let mut best_d = u32::MAX;
    for (i, c) in palette.iter().enumerate() {
        let dr = c.r as i32 - color.r as i32;
        let dg = c.g as i32 - color.g as i32;
        let db = c.b as i32 - color.b as i32;
        let d = (dr * dr + dg * dg + db * db) as u32;
        if d < best_d {
            best_d = d;
            best = i;
            if d == 0 {
                break;
            }
        }
    }
    best as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_single_placeholder() {
        let (pal, lookup) = median_cut(&[], 256);
        assert_eq!(pal.len(), 1);
        assert!(lookup.is_empty());
    }

    #[test]
    fn fewer_colors_than_budget_are_all_kept() {
        // Three distinct colours, budget 256: each becomes its own
        // single-member box, so the palette reproduces them exactly.
        let samples = vec![[255, 0, 0], [0, 255, 0], [0, 0, 255]];
        let (pal, lookup) = median_cut(&samples, 256);
        assert_eq!(pal.len(), 3);
        // Each sample maps to a box whose average equals it (single
        // member), so re-reading the palette reproduces the colour.
        for (i, s) in samples.iter().enumerate() {
            let entry = pal[lookup[i] as usize];
            assert_eq!([entry.r, entry.g, entry.b], *s);
        }
    }

    #[test]
    fn budget_is_respected_on_many_colors() {
        // A 16x16 gradient has 256 distinct colours; quantise to 8.
        let mut samples = Vec::new();
        for r in 0..16u8 {
            for g in 0..16u8 {
                samples.push([r * 17, g * 17, 0]);
            }
        }
        let (pal, lookup) = median_cut(&samples, 8);
        assert!(pal.len() <= 8, "palette {} exceeds budget", pal.len());
        assert_eq!(lookup.len(), samples.len());
        for &l in &lookup {
            assert!((l as usize) < pal.len());
        }
    }

    #[test]
    fn quantize_rgb_solid_image_is_single_color() {
        // 4x4 solid teal.
        let mut rgb = Vec::new();
        for _ in 0..16 {
            rgb.extend_from_slice(&[0, 128, 128]);
        }
        let q = quantize_rgb(&rgb, 4, 4, 256).unwrap();
        assert_eq!(q.palette.len(), 1);
        assert_eq!(q.indices.len(), 16);
        assert!(q.indices.iter().all(|&i| i == 0));
        assert_eq!(q.transparent_index, None);
        let c = q.palette[0];
        assert_eq!([c.r, c.g, c.b], [0, 128, 128]);
    }

    #[test]
    fn quantize_rgba_routes_transparent_pixels_to_reserved_slot() {
        // 2x2: opaque red, opaque green, fully transparent, opaque red.
        let rgba = vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            10, 20, 30, 0, // transparent
            255, 0, 0, 255, // red
        ];
        let q = quantize_rgba(&rgba, 2, 2, 256).unwrap();
        let ti = q.transparent_index.expect("has transparent pixel");
        // The transparent pixel (index 2) must map to the reserved slot.
        assert_eq!(q.indices[2], ti);
        // The two opaque reds must share one index, distinct from green.
        assert_eq!(q.indices[0], q.indices[3]);
        assert_ne!(q.indices[0], q.indices[1]);
        // No opaque pixel uses the transparent slot.
        assert_ne!(q.indices[0], ti);
        assert_ne!(q.indices[1], ti);
    }

    #[test]
    fn quantize_rgba_fully_opaque_has_no_transparent_index() {
        let rgba = vec![1, 2, 3, 255, 4, 5, 6, 255];
        let q = quantize_rgba(&rgba, 2, 1, 256).unwrap();
        assert_eq!(q.transparent_index, None);
    }

    #[test]
    fn quantize_rgba_length_mismatch_errors() {
        let rgba = vec![0u8; 7]; // not a multiple of 4 for 2x1
        assert!(quantize_rgba(&rgba, 2, 1, 256).is_err());
    }

    #[test]
    fn nearest_index_picks_closest_entry() {
        let pal = vec![
            Rgb::new(0, 0, 0),
            Rgb::new(255, 255, 255),
            Rgb::new(128, 0, 0),
        ];
        assert_eq!(nearest_index(&pal, Rgb::new(250, 250, 250)), 1);
        assert_eq!(nearest_index(&pal, Rgb::new(120, 5, 5)), 2);
        assert_eq!(nearest_index(&pal, Rgb::new(5, 5, 5)), 0);
    }

    #[test]
    fn nearest_index_exact_match_is_zero_distance() {
        let pal = vec![Rgb::new(10, 20, 30), Rgb::new(40, 50, 60)];
        assert_eq!(nearest_index(&pal, Rgb::new(40, 50, 60)), 1);
    }

    #[test]
    fn budget_clamps_to_one_minimum() {
        let samples = vec![[1, 2, 3], [4, 5, 6]];
        let (pal, _) = median_cut(&samples, 0);
        assert_eq!(pal.len(), 1);
    }

    #[test]
    fn transparent_budget_reserves_one_slot() {
        // 4 opaque distinct colours + 1 transparent, budget 3: opaque
        // gets 2 entries, transparent gets 1, total <= 3.
        let rgba = vec![
            255, 0, 0, 255, //
            0, 255, 0, 255, //
            0, 0, 255, 255, //
            128, 128, 128, 255, //
            9, 9, 9, 0, // transparent
        ];
        let q = quantize_rgba(&rgba, 5, 1, 3).unwrap();
        assert!(q.palette.len() <= 3);
        assert!(q.transparent_index.is_some());
    }
}
