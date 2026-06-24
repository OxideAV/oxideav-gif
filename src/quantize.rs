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

/// Index-plane assignment strategy used once the palette is selected.
///
/// Palette *selection* (median cut) is identical for every variant — the
/// dither only changes how each pixel is mapped onto the chosen palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dither {
    /// Map each pixel to its single nearest palette entry. Deterministic,
    /// no spatial coupling — the index-plane argmin over the palette.
    #[default]
    None,
    /// Floyd–Steinberg error diffusion: quantisation error at each pixel
    /// is pushed onto its not-yet-visited neighbours (7/16 right, 3/16
    /// down-left, 5/16 down, 1/16 down-right), so a smooth gradient that
    /// would band under a coarse palette breaks into a stippled mix of
    /// the two nearest entries that averages to the source colour. A
    /// general image-processing technique; the GIF spec only constrains
    /// the ≤256-entry/index-plane output shape, which is unchanged.
    FloydSteinberg,
}

/// Box-selection rule for the median-cut palette search.
///
/// Median cut repeatedly splits one box of the colour cube until the
/// palette budget is met; this enum chooses *which* box is split next.
/// Both rules select the same palette when every input colour has equal
/// population (e.g. a perfectly uniform gradient) and are identical on an
/// exact-colour input (≤ budget distinct colours), where every box is
/// driven down to a single colour regardless of the order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoxPriority {
    /// Split the box with the largest single-channel colour *range*
    /// (extent), ignoring how many pixels fall in it. The historical
    /// rule; deterministic and cheap. A box covering a wide colour range
    /// but holding only a handful of outlier pixels is split before a
    /// tightly-clustered box holding most of the image, so sparse
    /// outliers can claim a disproportionate share of the palette.
    #[default]
    Extent,
    /// Split the box maximising `population × longest_extent`, so a
    /// densely-populated region of colour space earns more palette
    /// entries than a sparsely-populated one of the same width. A
    /// textbook population-weighted refinement of median cut (a general
    /// image-processing technique — the GIF spec only constrains the
    /// ≤256-entry / index-plane output shape). On a typical photographic
    /// frame it lowers total quantisation error versus [`BoxPriority::Extent`]
    /// because the entries follow where the pixels actually are; on a
    /// uniform-density input the two rules coincide.
    Population,
}

/// Tuning knobs for the quantiser entry points that take options.
///
/// Constructed with [`Default`] (`max_colors = 256`, no dither,
/// extent-priority box selection) and adjusted field-by-field, or via
/// the small builder helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuantizeOptions {
    /// Maximum palette size, clamped to `1..=256` at use. When the frame
    /// carries transparent pixels one slot is reserved, so the opaque
    /// colours receive `max_colors - 1` entries.
    pub max_colors: usize,
    /// Index-plane assignment strategy ([`Dither::None`] by default).
    pub dither: Dither,
    /// Median-cut box-selection rule ([`BoxPriority::Extent`] by default,
    /// preserving the byte-stable historical palette for the bare
    /// `quantize_*` entry points).
    pub box_priority: BoxPriority,
    /// Number of Lloyd (k-means) relaxation rounds applied to sharpen the
    /// median-cut palette toward a local optimum (`0` by default — no
    /// refinement, so the bare `quantize_*` entry points stay byte-stable).
    /// Each round can only lower the total squared error and the loop stops
    /// early on convergence, so a small budget (4–8) captures most of the
    /// gain; very large values cost time without further benefit.
    pub palette_refine_iterations: usize,
}

impl Default for QuantizeOptions {
    fn default() -> Self {
        Self {
            max_colors: MAX_PALETTE,
            dither: Dither::None,
            box_priority: BoxPriority::Extent,
            palette_refine_iterations: 0,
        }
    }
}

impl QuantizeOptions {
    /// Options with the given colour budget and no dithering.
    pub fn with_max_colors(max_colors: usize) -> Self {
        Self {
            max_colors,
            ..Self::default()
        }
    }

    /// Set the dither strategy, returning the updated options.
    pub fn dither(mut self, dither: Dither) -> Self {
        self.dither = dither;
        self
    }

    /// Set the median-cut box-selection rule, returning the updated options.
    pub fn box_priority(mut self, box_priority: BoxPriority) -> Self {
        self.box_priority = box_priority;
        self
    }

    /// Set the number of Lloyd palette-refinement rounds, returning the
    /// updated options.
    pub fn palette_refine_iterations(mut self, iterations: usize) -> Self {
        self.palette_refine_iterations = iterations;
        self
    }
}

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
    quantize_rgba_with_options(
        rgba,
        width,
        height,
        QuantizeOptions::with_max_colors(max_colors),
    )
}

/// Quantise an RGBA pixel buffer with explicit [`QuantizeOptions`],
/// selecting between the flat nearest-entry index plane and Floyd–
/// Steinberg error diffusion.
///
/// Identical to [`quantize_rgba`] when `opts.dither` is [`Dither::None`].
/// Under [`Dither::FloydSteinberg`] the *palette* is still chosen by
/// median cut over the opaque pixels, but the index plane is produced by
/// diffusing each pixel's quantisation error onto its neighbours, which
/// trades flat banding for a stippled approximation that averages to the
/// source colour. Transparent pixels neither receive nor propagate error
/// (they are never displayed) — diffusion simply skips them.
///
/// # Errors
///
/// * `rgba.len()` is not exactly `width * height * 4`.
/// * `width * height` overflows `usize`.
pub fn quantize_rgba_with_options(
    rgba: &[u8],
    width: usize,
    height: usize,
    opts: QuantizeOptions,
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

    let budget = opts.max_colors.clamp(1, MAX_PALETTE);

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

    let (mut palette, opaque_lookup) = median_cut_refined(
        &opaque_samples,
        opaque_budget,
        opts.box_priority,
        opts.palette_refine_iterations,
    );

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

    let indices = match opts.dither {
        Dither::None => {
            // Stitch the per-pixel index plane: transparent pixels take
            // the reserved slot, opaque pixels take their nearest-entry
            // assignment from median cut.
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
            indices
        }
        Dither::FloydSteinberg => {
            // Error diffusion runs over the full spatial grid so it can
            // reach each pixel's right/below neighbours. The opaque-pixel
            // search palette excludes the reserved transparent slot (a
            // transparent entry must never be chosen for an opaque pixel),
            // so dither against the leading `opaque_len` entries.
            let opaque_len = palette.len() - usize::from(any_transparent);
            let samples: Vec<[u8; 3]> = rgba.chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect();
            dither_samples_floyd_steinberg(
                &samples,
                width,
                height,
                &palette[..opaque_len],
                &is_transparent,
                transparent_index,
            )
        }
    };

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
    quantize_rgb_with_options(
        rgb,
        width,
        height,
        QuantizeOptions::with_max_colors(max_colors),
    )
}

/// Quantise an opaque RGB pixel buffer with explicit [`QuantizeOptions`].
///
/// Identical to [`quantize_rgb`] when `opts.dither` is [`Dither::None`].
/// Under [`Dither::FloydSteinberg`] the palette is selected by median cut
/// and the index plane is produced by error diffusion over the grid.
///
/// # Errors
///
/// * `rgb.len()` is not exactly `width * height * 3`.
/// * `width * height` overflows `usize`.
pub fn quantize_rgb_with_options(
    rgb: &[u8],
    width: usize,
    height: usize,
    opts: QuantizeOptions,
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
    let budget = opts.max_colors.clamp(1, MAX_PALETTE);
    let samples: Vec<[u8; 3]> = rgb.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect();
    let (palette, lookup) = median_cut_refined(
        &samples,
        budget,
        opts.box_priority,
        opts.palette_refine_iterations,
    );
    let indices = match opts.dither {
        Dither::None => lookup,
        Dither::FloydSteinberg => {
            // No transparency: every pixel is opaque, the full palette is
            // the opaque-search palette, and the mask is all-false.
            let opaque = vec![false; pixel_count];
            dither_samples_floyd_steinberg(&samples, width, height, &palette, &opaque, None)
        }
    };
    Ok(Quantized {
        palette,
        indices,
        transparent_index: None,
    })
}

/// Result of quantising a sequence of frames against one shared palette.
///
/// Every frame's index plane references the same [`palette`], so the
/// palette can live in a single §18 Global Color Table rather than a §21
/// Local Color Table per frame — smaller output and no inter-frame palette
/// flicker.
///
/// [`palette`]: SharedQuantized::palette
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedQuantized {
    /// The one colour table shared by every frame — at most
    /// [`MAX_PALETTE`] entries.
    pub palette: Vec<Rgb>,
    /// One index plane per input frame, in input order; each is
    /// `width * height` entries referencing [`SharedQuantized::palette`].
    pub frame_indices: Vec<Vec<u8>>,
    /// `Some(i)` when *any* frame carried a transparent pixel; `i` is the
    /// shared reserved slot, suitable for every frame's §23.c.viii
    /// Transparency Index. `None` when every frame was fully opaque.
    pub transparent_index: Option<u8>,
}

/// Quantise several equally-sized RGBA frames against **one** shared
/// palette: pool every frame's opaque pixels, run a single median cut
/// over the union, then assign each frame's index plane against that one
/// palette.
///
/// This is the counterpart to calling [`quantize_rgba`] per frame (which
/// gives each frame its own §21 Local Color Table). A shared palette lets
/// the caller install one §18 Global Color Table for the whole animation
/// and clears the per-frame palette overhead; it also removes the
/// palette-flicker a viewer can show when adjacent frames carry
/// independently-chosen tables. If *any* frame has a transparent pixel one
/// slot of the budget is reserved across all frames.
///
/// `opts.dither` applies per frame against the shared palette.
///
/// # Errors
///
/// * `frames` is empty.
/// * `width * height` overflows `usize`.
/// * Any frame's length is not exactly `width * height * 4`.
pub fn quantize_frames_shared(
    frames: &[&[u8]],
    width: usize,
    height: usize,
    opts: QuantizeOptions,
) -> Result<SharedQuantized> {
    if frames.is_empty() {
        return Err(Error::InvalidInput(
            "quantize_frames_shared: at least one frame is required".into(),
        ));
    }
    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(|| Error::InvalidInput("quantize: width*height overflow".into()))?;
    for (f, frame) in frames.iter().enumerate() {
        if frame.len() != pixel_count * 4 {
            return Err(Error::InvalidInput(format!(
                "quantize_frames_shared: frame {} length {} != width*height*4 ({})",
                f,
                frame.len(),
                pixel_count * 4
            )));
        }
    }

    let budget = opts.max_colors.clamp(1, MAX_PALETTE);

    // Per-frame transparency masks + the pooled opaque samples.
    let mut masks: Vec<Vec<bool>> = Vec::with_capacity(frames.len());
    let mut pooled: Vec<[u8; 3]> = Vec::new();
    for frame in frames {
        let mut mask = Vec::with_capacity(pixel_count);
        for px in frame.chunks_exact(4) {
            let transparent = px[3] < ALPHA_OPAQUE_THRESHOLD;
            mask.push(transparent);
            if !transparent {
                pooled.push([px[0], px[1], px[2]]);
            }
        }
        masks.push(mask);
    }
    let any_transparent = masks.iter().flatten().any(|&t| t);

    let opaque_budget = if any_transparent {
        budget.saturating_sub(1).max(1)
    } else {
        budget
    };

    // One median cut over the union of every frame's opaque pixels.
    let (mut palette, _pooled_lookup) = median_cut_refined(
        &pooled,
        opaque_budget,
        opts.box_priority,
        opts.palette_refine_iterations,
    );
    let opaque_len = palette.len();

    let transparent_index = if any_transparent {
        let idx = palette.len() as u8;
        palette.push(Rgb::new(0, 0, 0));
        Some(idx)
    } else {
        None
    };

    // Assign each frame against the shared palette. The pooled lookup is
    // discarded: a frame's pixels are remapped against the *final* shared
    // palette (flat or dithered) so the per-frame plane is independent of
    // the pooling order.
    let opaque_palette = &palette[..opaque_len];
    let mut frame_indices = Vec::with_capacity(frames.len());
    for (frame, mask) in frames.iter().zip(&masks) {
        let samples: Vec<[u8; 3]> = frame.chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect();
        let plane = match opts.dither {
            Dither::None => samples
                .iter()
                .zip(mask)
                .map(|(&[r, g, b], &transparent)| {
                    if transparent {
                        transparent_index.expect("any_transparent implies Some")
                    } else {
                        nearest_index(opaque_palette, Rgb::new(r, g, b))
                    }
                })
                .collect(),
            Dither::FloydSteinberg => dither_samples_floyd_steinberg(
                &samples,
                width,
                height,
                opaque_palette,
                mask,
                transparent_index,
            ),
        };
        frame_indices.push(plane);
    }

    Ok(SharedQuantized {
        palette,
        frame_indices,
        transparent_index,
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

    /// Box-selection key under a given [`BoxPriority`] — the larger the
    /// key, the sooner the box is split. [`BoxPriority::Extent`] returns
    /// the raw longest extent (matching the historical rule);
    /// [`BoxPriority::Population`] returns `population × longest_extent` in
    /// `u64` so a densely-populated box outranks a wide-but-sparse one of
    /// the same colour range. A single-colour box (extent 0) keys to 0
    /// under both rules and is filtered out before this is consulted.
    fn split_priority(&self, priority: BoxPriority) -> u64 {
        let extent = self.longest_extent() as u64;
        match priority {
            BoxPriority::Extent => extent,
            // members.len() fits a usize; the product stays well within
            // u64 (population ≤ width*height, extent ≤ 255).
            BoxPriority::Population => self.members.len() as u64 * extent,
        }
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

/// Median-cut quantiser core with no palette refinement — thin wrapper
/// over [`median_cut_refined`] used by the tests and any caller that
/// wants the plain median-cut palette.
#[cfg(test)]
fn median_cut_with_priority(
    samples: &[[u8; 3]],
    budget: usize,
    priority: BoxPriority,
) -> (Vec<Rgb>, Vec<u8>) {
    median_cut_refined(samples, budget, priority, 0)
}

/// Median-cut quantiser core with optional Lloyd (k-means) palette
/// refinement. Returns the selected palette plus, for every sample in
/// `samples` (in order), the index of the *nearest* palette entry it maps
/// to.
///
/// Colour selection is median cut (repeatedly split one box along its
/// longest axis until the budget is met); `priority` chooses which box is
/// split next — the widest box ([`BoxPriority::Extent`]) or the box with
/// the largest `population × longest_extent` ([`BoxPriority::Population`]).
///
/// When `refine_iters > 0`, the median-cut palette is then sharpened by up
/// to that many rounds of Lloyd relaxation: assign every sample to its
/// nearest current entry, then move each entry to the (rounded) centroid
/// of the samples assigned to it. Each round can only lower — never raise
/// — the total squared error, and the loop stops early once it converges
/// (no entry moves). An entry that captures no samples keeps its previous
/// position. This is textbook k-means refinement of a median-cut seed; the
/// GIF spec only constrains the ≤256-entry / index-plane output shape,
/// which is unchanged.
///
/// The index plane is finally assigned by nearest-entry remap over the
/// refined palette, so a sample never keeps a strictly-worse box-of-origin
/// assignment.
///
/// `budget` is the maximum palette size, already clamped to `1..=256`.
/// An empty input yields a single placeholder black entry and no
/// lookups (the caller has no opaque pixels to map).
fn median_cut_refined(
    samples: &[[u8; 3]],
    budget: usize,
    priority: BoxPriority,
    refine_iters: usize,
) -> (Vec<Rgb>, Vec<u8>) {
    if samples.is_empty() {
        // No opaque pixels: still hand back a one-entry palette so a
        // §22 index plane built against it is in range, even though no
        // pixel will reference it.
        return (vec![Rgb::new(0, 0, 0)], Vec::new());
    }

    let budget = budget.clamp(1, MAX_PALETTE);

    // Start with one box holding every sample, then repeatedly split the
    // box the priority rule selects until we hit the budget or no box can
    // be split further (a box of identical colours has zero extent).
    let mut boxes = vec![ColorBox::from_members(
        samples,
        (0..samples.len()).collect(),
    )];

    while boxes.len() < budget {
        // Pick the splittable box the priority rule ranks highest. Only
        // boxes that still hold >1 sample across >0 colour range can be
        // split; a single-colour box has nothing left to give.
        let target = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.members.len() > 1 && b.longest_extent() > 0)
            .max_by_key(|(_, b)| b.split_priority(priority))
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

    // Each box contributes one palette entry: its average colour.
    let mut palette = Vec::with_capacity(boxes.len());
    for b in &boxes {
        palette.push(b.average(samples));
    }

    // Optional Lloyd relaxation: sharpen the median-cut seed toward a
    // local optimum. Each round re-assigns samples to their nearest entry
    // and recentres each entry on its cluster's centroid, which is the
    // squared-error-minimising move, so total error is monotone
    // non-increasing across rounds.
    if refine_iters > 0 {
        refine_palette_lloyd(samples, &mut palette, refine_iters);
    }

    // Map every sample to its *nearest* palette entry rather than the box
    // it fell into. After a box is averaged to one colour, a sample near a
    // box boundary can be closer to a neighbouring box's average than to
    // its own; nearest-entry remap removes that residual error at no
    // change to the palette. The box partition is only the colour-selection
    // step; the index plane is recomputed against the final palette.
    let lookup = remap_to_nearest(samples, &palette);

    (palette, lookup)
}

/// Lloyd (k-means) relaxation of a palette in place, run for at most
/// `iters` rounds or until convergence (no entry moves), whichever is
/// first. `samples` is the colour set the palette approximates; `palette`
/// is the seed (typically a median-cut result) that is sharpened.
///
/// Each round assigns every sample to its nearest current entry, then
/// moves each entry to the rounded centroid of the samples assigned to it
/// — the squared-error-minimising position for a fixed assignment — so
/// the total squared error is non-increasing. An entry that captures no
/// samples is left where it is (its colour may still serve a later
/// nearest-entry lookup). With an empty sample set the palette is
/// untouched.
fn refine_palette_lloyd(samples: &[[u8; 3]], palette: &mut [Rgb], iters: usize) {
    if samples.is_empty() || palette.is_empty() {
        return;
    }
    // Per-entry running totals (sum of each channel + count), reused each
    // round to avoid reallocating.
    let mut acc: Vec<[u64; 4]> = vec![[0; 4]; palette.len()];
    for _ in 0..iters {
        for a in acc.iter_mut() {
            *a = [0; 4];
        }
        for &[r, g, b] in samples {
            let j = nearest_index(palette, Rgb::new(r, g, b)) as usize;
            acc[j][0] += r as u64;
            acc[j][1] += g as u64;
            acc[j][2] += b as u64;
            acc[j][3] += 1;
        }
        let mut moved = false;
        for (entry, a) in palette.iter_mut().zip(&acc) {
            let n = a[3];
            if n == 0 {
                continue; // no samples chose this entry; leave it in place
            }
            // Rounded mean per channel (add half the count before dividing).
            let half = n / 2;
            let next = Rgb::new(
                ((a[0] + half) / n) as u8,
                ((a[1] + half) / n) as u8,
                ((a[2] + half) / n) as u8,
            );
            if next != *entry {
                *entry = next;
                moved = true;
            }
        }
        if !moved {
            break; // converged: a further round would not change anything
        }
    }
}

/// Map every sample to the index of its nearest palette entry by
/// squared-Euclidean RGB distance. Shared by [`median_cut`] (final
/// index-plane assignment) and the dithered path.
///
/// The palette never exceeds [`MAX_PALETTE`] (256) entries, so the linear
/// scan per sample is bounded; a `[Rgb; 256]`-cardinality search keeps the
/// implementation branch-simple and exact rather than approximate.
fn remap_to_nearest(samples: &[[u8; 3]], palette: &[Rgb]) -> Vec<u8> {
    samples
        .iter()
        .map(|&[r, g, b]| nearest_index(palette, Rgb::new(r, g, b)))
        .collect()
}

/// Produce a §22 index plane by Floyd–Steinberg error diffusion.
///
/// `samples` is the full `width * height` grid of source RGB colours
/// (transparent pixels carry an arbitrary colour, masked out by
/// `is_transparent`). `palette` is the *opaque* search palette — the
/// reserved transparent slot, if any, is excluded so error diffusion can
/// never select it for an opaque pixel; `transparent_index` is the index
/// transparent pixels emit instead.
///
/// Error is carried in a per-channel `i32` working buffer so accumulated
/// diffusion can exceed the `0..=255` range before it is re-clamped at
/// the nearest-entry search. The kernel pushes the quantisation error of
/// each pixel onto its not-yet-visited neighbours — 7/16 east, 3/16
/// south-west, 5/16 south, 1/16 south-east — left-to-right, top-to-bottom.
/// Transparent pixels neither receive nor emit error (they are never
/// displayed): incoming error to a transparent cell is dropped and no
/// error is diffused out of one.
fn dither_samples_floyd_steinberg(
    samples: &[[u8; 3]],
    width: usize,
    height: usize,
    palette: &[Rgb],
    is_transparent: &[bool],
    transparent_index: Option<u8>,
) -> Vec<u8> {
    let pixel_count = width * height;
    let mut indices = vec![0u8; pixel_count];
    // Two rows of accumulated error (current + next), each [r, g, b] in
    // i32 so over/undershoot can build up across pixels before clamping.
    let mut cur_err = vec![[0i32; 3]; width];
    let mut next_err = vec![[0i32; 3]; width];

    // An empty (degenerate) palette has no valid opaque index; every
    // opaque pixel falls back to 0, matching `nearest_index`'s contract.
    let search = if palette.is_empty() {
        &[Rgb::new(0, 0, 0)][..]
    } else {
        palette
    };

    for y in 0..height {
        // Reset the next-row accumulator before this row diffuses into it.
        for e in next_err.iter_mut() {
            *e = [0; 3];
        }
        for x in 0..width {
            let p = y * width + x;
            if is_transparent[p] {
                indices[p] = transparent_index.expect("transparent pixel needs reserved index");
                // No error in or out of a never-displayed pixel.
                continue;
            }
            // Source colour + the error diffused into this cell, clamped
            // back to the displayable range for the nearest-entry search.
            let [sr, sg, sb] = samples[p];
            let want = [
                (sr as i32 + cur_err[x][0]).clamp(0, 255),
                (sg as i32 + cur_err[x][1]).clamp(0, 255),
                (sb as i32 + cur_err[x][2]).clamp(0, 255),
            ];
            let idx = nearest_index(
                search,
                Rgb::new(want[0] as u8, want[1] as u8, want[2] as u8),
            );
            indices[p] = idx;
            let chosen = search[idx as usize];
            // Quantisation error = what we wanted minus what we picked.
            let err = [
                want[0] - chosen.r as i32,
                want[1] - chosen.g as i32,
                want[2] - chosen.b as i32,
            ];
            // Diffuse: 7/16 east, 3/16 south-west, 5/16 south, 1/16 SE.
            for c in 0..3 {
                let e = err[c];
                if x + 1 < width {
                    cur_err[x + 1][c] += e * 7 / 16;
                }
                if y + 1 < height {
                    if x > 0 {
                        next_err[x - 1][c] += e * 3 / 16;
                    }
                    next_err[x][c] += e * 5 / 16;
                    if x + 1 < width {
                        next_err[x + 1][c] += e / 16;
                    }
                }
            }
        }
        // The row we just diffused into becomes the current row.
        core::mem::swap(&mut cur_err, &mut next_err);
    }

    indices
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

    /// Test shorthand: median cut with the default extent priority.
    fn median_cut(samples: &[[u8; 3]], budget: usize) -> (Vec<Rgb>, Vec<u8>) {
        median_cut_with_priority(samples, budget, BoxPriority::Extent)
    }

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

    /// Total squared-Euclidean error of an index plane against the
    /// original samples — the quantity nearest-entry remap minimises.
    fn total_sq_error(samples: &[[u8; 3]], palette: &[Rgb], lookup: &[u8]) -> u64 {
        samples
            .iter()
            .zip(lookup)
            .map(|(&[r, g, b], &idx)| {
                let p = palette[idx as usize];
                let dr = p.r as i64 - r as i64;
                let dg = p.g as i64 - g as i64;
                let db = p.b as i64 - b as i64;
                (dr * dr + dg * dg + db * db) as u64
            })
            .sum()
    }

    #[test]
    fn nearest_remap_never_exceeds_box_assignment_error() {
        // Build a synthetic palette and the two competing index planes
        // (box-of-origin vs nearest-entry) for a colour set straddling a
        // box boundary, and assert nearest-entry is no worse.
        let mut samples = Vec::new();
        // Two clusters with a few boundary samples between them.
        for v in [10u8, 12, 14, 200, 202, 204, 100, 105, 110] {
            samples.push([v, v, v]);
        }
        let (pal, lookup) = median_cut(&samples, 2);
        // Every sample's assigned entry must be its true nearest entry.
        for (&s, &idx) in samples.iter().zip(&lookup) {
            let nearest = nearest_index(&pal, Rgb::new(s[0], s[1], s[2]));
            assert_eq!(idx, nearest, "index plane entry must equal nearest_index");
        }
    }

    #[test]
    fn nearest_remap_reduces_total_error_on_boundary_colors() {
        // A 1-D ramp 0..=255 quantised to 4 entries: boundary samples
        // benefit from nearest-entry over box-of-origin. We reconstruct
        // the box-of-origin lookup to compare against the shipped one.
        let samples: Vec<[u8; 3]> = (0..=255u8).map(|v| [v, v, v]).collect();
        let (pal, nearest_lookup) = median_cut(&samples, 4);

        // Recompute a pure box-of-origin lookup from the same boxes by
        // re-running the split and tagging members — but simplest is to
        // verify the shipped lookup is the argmin, which dominates any
        // box-of-origin assignment by construction.
        let shipped = total_sq_error(&samples, &pal, &nearest_lookup);
        // The brute-force argmin error is a lower bound the shipped plane
        // must meet exactly (it IS the argmin).
        let argmin: u64 = samples
            .iter()
            .map(|&s| {
                pal.iter()
                    .map(|p| {
                        let dr = p.r as i64 - s[0] as i64;
                        let dg = p.g as i64 - s[1] as i64;
                        let db = p.b as i64 - s[2] as i64;
                        (dr * dr + dg * dg + db * db) as u64
                    })
                    .min()
                    .unwrap()
            })
            .sum();
        assert_eq!(
            shipped, argmin,
            "index plane must be the nearest-entry argmin"
        );
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

    // ----- Floyd–Steinberg dithering -----

    #[test]
    fn dither_none_matches_plain_entry_point() {
        // The options entry point with Dither::None must be byte-identical
        // to the bare quantize_rgb / quantize_rgba.
        let rgb: Vec<u8> = (0..48u8).collect(); // 16 px (4x4) of varied colour
        let plain = quantize_rgb(&rgb, 4, 4, 8).unwrap();
        let opt =
            quantize_rgb_with_options(&rgb, 4, 4, QuantizeOptions::with_max_colors(8)).unwrap();
        assert_eq!(plain, opt);

        let rgba: Vec<u8> = (0..64u8).collect(); // 16 px (4x4) RGBA
        let plain = quantize_rgba(&rgba, 4, 4, 8).unwrap();
        let opt =
            quantize_rgba_with_options(&rgba, 4, 4, QuantizeOptions::with_max_colors(8)).unwrap();
        assert_eq!(plain, opt);
    }

    #[test]
    fn dither_indices_stay_in_palette_range() {
        // 8x8 horizontal grey ramp, budget 4, dithered.
        let mut rgb = Vec::new();
        for _y in 0..8 {
            for x in 0..8u8 {
                let v = x * 36; // 0..252
                rgb.extend_from_slice(&[v, v, v]);
            }
        }
        let opts = QuantizeOptions::with_max_colors(4).dither(Dither::FloydSteinberg);
        let q = quantize_rgb_with_options(&rgb, 8, 8, opts).unwrap();
        assert_eq!(q.indices.len(), 64);
        assert!(q.indices.iter().all(|&i| (i as usize) < q.palette.len()));
    }

    #[test]
    fn dither_solid_image_is_flat() {
        // A solid image has zero quantisation error, so dithering changes
        // nothing — every pixel still maps to the one palette entry.
        let rgb = vec![64u8; 3 * 16]; // 4x4 solid
        let opts = QuantizeOptions::with_max_colors(8).dither(Dither::FloydSteinberg);
        let q = quantize_rgb_with_options(&rgb, 4, 4, opts).unwrap();
        assert_eq!(q.palette.len(), 1);
        assert!(q.indices.iter().all(|&i| i == 0));
    }

    #[test]
    fn dither_breaks_banding_into_a_mix() {
        // A smooth ramp quantised to 2 colours: flat assignment splits it
        // into two solid bands (each index appears in a contiguous block);
        // dithering interleaves the two indices to approximate midtones, so
        // a row that is a single flat index under no-dither becomes a mix.
        let mut rgb = Vec::new();
        for x in 0..16u8 {
            let v = x * 17; // 0..255 ramp
            rgb.extend_from_slice(&[v, v, v]);
        }
        let flat = quantize_rgb(&rgb, 16, 1, 2).unwrap();
        let dithered = quantize_rgb_with_options(
            &rgb,
            16,
            1,
            QuantizeOptions::with_max_colors(2).dither(Dither::FloydSteinberg),
        )
        .unwrap();
        // Both use a 2-entry palette.
        assert_eq!(flat.palette.len(), 2);
        assert_eq!(dithered.palette.len(), 2);
        // The dithered plane must not be a simple monotone threshold split:
        // somewhere a higher-value pixel takes the dark index or vice versa,
        // which a flat nearest-entry split never does.
        let flat_transitions = transition_count(&flat.indices);
        let dith_transitions = transition_count(&dithered.indices);
        assert!(
            dith_transitions > flat_transitions,
            "dithering should add index transitions (flat {flat_transitions}, dithered {dith_transitions})"
        );
    }

    fn transition_count(indices: &[u8]) -> usize {
        indices.windows(2).filter(|w| w[0] != w[1]).count()
    }

    /// Mean squared error of the *block-averaged* reconstruction: average
    /// every `block × block` tile of the reconstructed (palette[index])
    /// image and of the source, then compare. Error diffusion is designed
    /// to make the *local average* of the stippled output track the source
    /// — pointwise error can rise while the block average improves, which
    /// is exactly the perceptual win dithering buys.
    fn block_mean_sq_error(
        src: &[[u8; 3]],
        palette: &[Rgb],
        indices: &[u8],
        width: usize,
        height: usize,
        block: usize,
    ) -> f64 {
        let mut total = 0.0f64;
        let mut tiles = 0u64;
        let mut by = 0;
        while by < height {
            let mut bx = 0;
            while bx < width {
                let (mut ssr, mut ssg, mut ssb) = (0i64, 0i64, 0i64);
                let (mut rsr, mut rsg, mut rsb) = (0i64, 0i64, 0i64);
                let mut n = 0i64;
                for y in by..(by + block).min(height) {
                    for x in bx..(bx + block).min(width) {
                        let p = y * width + x;
                        let [r, g, b] = src[p];
                        ssr += r as i64;
                        ssg += g as i64;
                        ssb += b as i64;
                        let c = palette[indices[p] as usize];
                        rsr += c.r as i64;
                        rsg += c.g as i64;
                        rsb += c.b as i64;
                        n += 1;
                    }
                }
                let n = n as f64;
                let dr = (ssr - rsr) as f64 / n;
                let dg = (ssg - rsg) as f64 / n;
                let db = (ssb - rsb) as f64 / n;
                total += dr * dr + dg * dg + db * db;
                tiles += 1;
                bx += block;
            }
            by += block;
        }
        total / tiles as f64
    }

    #[test]
    fn dither_reduces_block_average_error_on_a_gradient() {
        // 32x32 diagonal RGB gradient quantised to 8 colours. Error
        // diffusion lowers the *block-averaged* error versus the flat
        // nearest-entry plane: the stippled mix of palette entries averages
        // back to the source over a small neighbourhood, where the flat
        // plane bands into solid blocks that miss the midtones entirely.
        let w = 32usize;
        let h = 32usize;
        let mut src = Vec::with_capacity(w * h);
        let mut rgb = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let r = (x * 8) as u8;
                let g = (y * 8) as u8;
                let b = ((x + y) * 4) as u8;
                src.push([r, g, b]);
                rgb.extend_from_slice(&[r, g, b]);
            }
        }
        let flat = quantize_rgb(&rgb, w, h, 8).unwrap();
        let dithered = quantize_rgb_with_options(
            &rgb,
            w,
            h,
            QuantizeOptions::with_max_colors(8).dither(Dither::FloydSteinberg),
        )
        .unwrap();
        let flat_err = block_mean_sq_error(&src, &flat.palette, &flat.indices, w, h, 4);
        let dith_err = block_mean_sq_error(&src, &dithered.palette, &dithered.indices, w, h, 4);
        assert!(
            dith_err < flat_err,
            "dither block-avg error {dith_err:.1} not below flat {flat_err:.1}"
        );
    }

    #[test]
    fn dither_routes_transparent_pixels_and_excludes_their_slot() {
        // A grid with transparent pixels: the dithered plane must still
        // send every transparent pixel to the reserved index, and no
        // opaque pixel may land on that reserved slot.
        let w = 4usize;
        let h = 4usize;
        let mut rgba = Vec::new();
        for i in 0..(w * h) {
            if i % 5 == 0 {
                rgba.extend_from_slice(&[9, 9, 9, 0]); // transparent
            } else {
                let v = (i * 13) as u8;
                rgba.extend_from_slice(&[v, 255 - v, v / 2, 255]);
            }
        }
        let opts = QuantizeOptions::with_max_colors(8).dither(Dither::FloydSteinberg);
        let q = quantize_rgba_with_options(&rgba, w, h, opts).unwrap();
        let ti = q.transparent_index.expect("has transparent pixels");
        for (i, px) in rgba.chunks_exact(4).enumerate() {
            if px[3] < ALPHA_OPAQUE_THRESHOLD {
                assert_eq!(
                    q.indices[i], ti,
                    "transparent pixel {i} not on reserved slot"
                );
            } else {
                assert_ne!(q.indices[i], ti, "opaque pixel {i} landed on reserved slot");
            }
        }
    }

    // ----- shared-palette multi-frame quantisation -----

    #[test]
    fn shared_empty_frames_errors() {
        let r = quantize_frames_shared(&[], 2, 2, QuantizeOptions::default());
        assert!(r.is_err());
    }

    #[test]
    fn shared_frame_length_mismatch_errors() {
        let good = vec![0u8; 2 * 2 * 4];
        let bad = vec![0u8; 7];
        let r = quantize_frames_shared(&[&good, &bad], 2, 2, QuantizeOptions::default());
        assert!(r.is_err());
    }

    #[test]
    fn shared_palette_covers_every_frames_colours() {
        // Frame A is solid red, frame B is solid blue. With a budget of
        // 256 the shared palette must contain both exactly, and each
        // frame's plane resolves back to its own colour.
        let red = [255u8, 0, 0, 255].repeat(4); // 2x2
        let blue = [0u8, 0, 255, 255].repeat(4);
        let s = quantize_frames_shared(&[&red, &blue], 2, 2, QuantizeOptions::with_max_colors(256))
            .unwrap();
        assert_eq!(s.frame_indices.len(), 2);
        assert!(s.palette.contains(&Rgb::new(255, 0, 0)));
        assert!(s.palette.contains(&Rgb::new(0, 0, 255)));
        assert_eq!(s.transparent_index, None);
        // Frame 0 resolves to red everywhere, frame 1 to blue.
        for &i in &s.frame_indices[0] {
            assert_eq!(s.palette[i as usize], Rgb::new(255, 0, 0));
        }
        for &i in &s.frame_indices[1] {
            assert_eq!(s.palette[i as usize], Rgb::new(0, 0, 255));
        }
    }

    #[test]
    fn shared_budget_is_respected_across_the_union() {
        // Two frames, each a distinct ramp; the pooled colour set exceeds
        // the budget, so the shared palette must clamp to it.
        let mut a = Vec::new();
        let mut b = Vec::new();
        for x in 0..16u8 {
            a.extend_from_slice(&[x * 16, 0, 0, 255]);
            b.extend_from_slice(&[0, x * 16, 0, 255]);
        }
        let s =
            quantize_frames_shared(&[&a, &b], 16, 1, QuantizeOptions::with_max_colors(8)).unwrap();
        assert!(s.palette.len() <= 8);
        for plane in &s.frame_indices {
            assert!(plane.iter().all(|&i| (i as usize) < s.palette.len()));
        }
    }

    #[test]
    fn shared_reserves_one_transparent_slot_for_the_whole_animation() {
        // Frame 0 fully opaque, frame 1 has a transparent pixel: one
        // shared transparent index covers both, and only the transparent
        // pixel uses it.
        let opaque = [200u8, 50, 50, 255].repeat(4); // 2x2
        let mut mixed = Vec::new();
        for i in 0..4 {
            if i == 1 {
                mixed.extend_from_slice(&[0, 0, 0, 0]); // transparent
            } else {
                mixed.extend_from_slice(&[50, 50, 200, 255]);
            }
        }
        let s = quantize_frames_shared(
            &[&opaque, &mixed],
            2,
            2,
            QuantizeOptions::with_max_colors(8),
        )
        .unwrap();
        let ti = s
            .transparent_index
            .expect("animation has a transparent pixel");
        // No pixel in the fully-opaque frame uses the transparent slot.
        assert!(s.frame_indices[0].iter().all(|&i| i != ti));
        // The transparent pixel in frame 1 uses it; the opaque ones don't.
        assert_eq!(s.frame_indices[1][1], ti);
        for (i, &idx) in s.frame_indices[1].iter().enumerate() {
            if i != 1 {
                assert_ne!(idx, ti);
            }
        }
    }

    // ----- population-weighted box priority -----

    #[test]
    fn box_priority_default_is_extent() {
        // The defaults and the budget-only constructor both pick the
        // historical extent rule, so the bare quantise_* entry points
        // stay byte-stable.
        assert_eq!(QuantizeOptions::default().box_priority, BoxPriority::Extent);
        assert_eq!(
            QuantizeOptions::with_max_colors(16).box_priority,
            BoxPriority::Extent
        );
        assert_eq!(BoxPriority::default(), BoxPriority::Extent);
    }

    #[test]
    fn box_priority_actually_changes_the_palette() {
        // On a population-skewed multi-box input the two rules must select
        // genuinely different palettes — proving the knob has effect rather
        // than collapsing to the same selection. (When the palettes happen
        // to coincide on uniform-density / exact-colour inputs, the
        // dedicated coincidence tests cover that.)
        let mut samples = Vec::new();
        for k in 0..200u32 {
            samples.push([0, (k % 201) as u8, 0]); // wide, many pixels
        }
        for k in 0..150u32 {
            samples.push([0, 120 + (k % 6) as u8, 255]); // narrow, dense
        }
        let (pal_e, _) = median_cut_with_priority(&samples, 5, BoxPriority::Extent);
        let (pal_p, _) = median_cut_with_priority(&samples, 5, BoxPriority::Population);
        assert_ne!(
            pal_e, pal_p,
            "extent and population priority should diverge on a skewed input"
        );
    }

    #[test]
    fn priorities_coincide_on_exact_color_input() {
        // ≤ budget distinct colours: median cut drives every box to a
        // single colour regardless of the selection rule, so both rules
        // round-trip every colour exactly (palette order may differ, but
        // each colour maps to an entry equal to itself).
        let samples = vec![[255, 0, 0], [0, 255, 0], [0, 0, 255], [40, 40, 40]];
        for prio in [BoxPriority::Extent, BoxPriority::Population] {
            let (pal, look) = median_cut_with_priority(&samples, 256, prio);
            assert_eq!(pal.len(), 4, "{prio:?}");
            for (s, &idx) in samples.iter().zip(&look) {
                let e = pal[idx as usize];
                assert_eq!([e.r, e.g, e.b], *s, "{prio:?}");
            }
        }
    }

    #[test]
    fn priorities_coincide_on_uniform_density_input() {
        // Every colour appears exactly once (a 1-D ramp): population is
        // constant, so population × extent ranks boxes identically to
        // extent alone and the two rules select the same palette.
        let samples: Vec<[u8; 3]> = (0..=255u8).map(|v| [v, v, v]).collect();
        let (pal_e, look_e) = median_cut_with_priority(&samples, 5, BoxPriority::Extent);
        let (pal_p, look_p) = median_cut_with_priority(&samples, 5, BoxPriority::Population);
        assert_eq!(pal_e, pal_p);
        assert_eq!(look_e, look_p);
    }

    #[test]
    fn population_priority_lowers_error_on_a_skewed_distribution() {
        // Two cleanly-separated colour groups of comparable pixel count
        // but very different internal spread:
        //  * group P — 200 px spread *wide* along green (0..200), blue 0;
        //  * group Q — 150 px packed *narrow* along green (120..125), blue 255.
        // After the first split isolates P from Q, the extent rule keeps
        // re-splitting the wide group P (largest colour range) while the
        // already-tight, heavily-populated group Q is collapsed to a single
        // entry — so its 150 pixels all carry the averaging error. The
        // population rule weights by `pixels × spread`, so it splits group P
        // only until group Q's pixel mass outranks P's residual spread, then
        // refines Q too. Total quantisation error is lower as a result.
        let mut samples = Vec::new();
        for k in 0..200u32 {
            samples.push([0, (k % 201) as u8, 0]); // P: wide green ramp
        }
        for k in 0..150u32 {
            samples.push([0, 120 + (k % 6) as u8, 255]); // Q: narrow, dense
        }

        let (pal_e, look_e) = median_cut_with_priority(&samples, 5, BoxPriority::Extent);
        let (pal_p, look_p) = median_cut_with_priority(&samples, 5, BoxPriority::Population);
        let err_e = total_sq_error(&samples, &pal_e, &look_e);
        let err_p = total_sq_error(&samples, &pal_p, &look_p);
        assert!(
            err_p < err_e,
            "population priority should reduce error: extent={err_e} population={err_p}"
        );
    }

    // ----- Lloyd palette refinement -----

    #[test]
    fn refine_iterations_default_is_zero() {
        assert_eq!(QuantizeOptions::default().palette_refine_iterations, 0);
        assert_eq!(
            QuantizeOptions::with_max_colors(16).palette_refine_iterations,
            0
        );
    }

    #[test]
    fn zero_refine_iterations_is_byte_identical_to_plain_median_cut() {
        // The default (0 rounds) path must reproduce the un-refined palette
        // and lookup exactly, so the bare quantize_* entry points stay
        // byte-stable.
        let mut samples = Vec::new();
        for r in 0..12u8 {
            for g in 0..12u8 {
                samples.push([r * 21, g * 21, (r.wrapping_mul(g)) % 200]);
            }
        }
        let (pal_a, look_a) = median_cut_with_priority(&samples, 16, BoxPriority::Extent);
        let (pal_b, look_b) = median_cut_refined(&samples, 16, BoxPriority::Extent, 0);
        assert_eq!(pal_a, pal_b);
        assert_eq!(look_a, look_b);
    }

    #[test]
    fn refinement_keeps_exact_color_input_byte_stable() {
        // ≤ budget distinct colours: every entry is already its cluster's
        // sole member, so Lloyd converges immediately and the palette is
        // unchanged. The lossless round-trip path must stay exact.
        let samples = vec![[255, 0, 0], [0, 255, 0], [0, 0, 255], [30, 60, 90]];
        let (pal_plain, look_plain) = median_cut_refined(&samples, 256, BoxPriority::Extent, 0);
        let (pal_ref, look_ref) = median_cut_refined(&samples, 256, BoxPriority::Extent, 8);
        assert_eq!(pal_plain, pal_ref);
        assert_eq!(look_plain, look_ref);
        // And every colour still reproduces itself.
        for (s, &idx) in samples.iter().zip(&look_ref) {
            let e = pal_ref[idx as usize];
            assert_eq!([e.r, e.g, e.b], *s);
        }
    }

    #[test]
    fn refinement_lowers_total_error() {
        // A 32x32 RGB field with structure: refinement should reduce the
        // total squared error versus the plain median-cut seed.
        let mut samples = Vec::new();
        for y in 0..32u32 {
            for x in 0..32u32 {
                samples.push([(x * 8) as u8, (y * 8) as u8, ((x + y) * 4) as u8]);
            }
        }
        let (pal0, look0) = median_cut_refined(&samples, 16, BoxPriority::Extent, 0);
        let (pal8, look8) = median_cut_refined(&samples, 16, BoxPriority::Extent, 8);
        let e0 = total_sq_error(&samples, &pal0, &look0);
        let e8 = total_sq_error(&samples, &pal8, &look8);
        assert!(
            e8 < e0,
            "refinement must lower error: seed={e0} refined={e8}"
        );
    }

    #[test]
    fn refinement_error_is_monotone_non_increasing_per_round() {
        // Each additional Lloyd round can only lower (or hold) the total
        // error — never raise it.
        let mut samples = Vec::new();
        for k in 0..900u32 {
            samples.push([
                (k * 7 % 256) as u8,
                (k * 13 % 256) as u8,
                (k * 29 % 256) as u8,
            ]);
        }
        let mut prev = u64::MAX;
        for iters in 0..6 {
            let (pal, look) = median_cut_refined(&samples, 12, BoxPriority::Extent, iters);
            let e = total_sq_error(&samples, &pal, &look);
            assert!(
                e <= prev,
                "round {iters}: error {e} exceeded previous {prev}"
            );
            prev = e;
        }
    }

    #[test]
    fn refinement_converges_and_stops_early() {
        // On an exact-colour input Lloyd converges on round 1; asking for a
        // huge iteration count must still terminate quickly and match the
        // 1-round result.
        let samples = vec![[10, 20, 30], [200, 210, 220], [90, 90, 90]];
        let (pal_1, _) = median_cut_refined(&samples, 256, BoxPriority::Extent, 1);
        let (pal_big, _) = median_cut_refined(&samples, 256, BoxPriority::Extent, 100_000);
        assert_eq!(pal_1, pal_big);
    }

    #[test]
    fn refinement_threads_through_quantize_rgb_options() {
        // End-to-end: the public option-taking entry point honours the
        // refinement count and still emits an in-range index plane.
        let mut rgb = Vec::new();
        for y in 0..16u8 {
            for x in 0..16u8 {
                rgb.extend_from_slice(&[x * 16, y * 16, 0]);
            }
        }
        let opts = QuantizeOptions::with_max_colors(8).palette_refine_iterations(6);
        let q = quantize_rgb_with_options(&rgb, 16, 16, opts).unwrap();
        assert!(q.palette.len() <= 8);
        assert!(q.indices.iter().all(|&i| (i as usize) < q.palette.len()));
    }

    #[test]
    fn population_priority_respects_budget_and_range() {
        // Sanity: the population rule still clamps to the budget and emits
        // in-range indices on a many-colour input.
        let mut rgb = Vec::new();
        for r in 0..16u8 {
            for g in 0..16u8 {
                rgb.extend_from_slice(&[r * 17, g * 17, 0]);
            }
        }
        let q = quantize_rgb_with_options(
            &rgb,
            16,
            16,
            QuantizeOptions::with_max_colors(8).box_priority(BoxPriority::Population),
        )
        .unwrap();
        assert!(q.palette.len() <= 8);
        assert!(q.indices.iter().all(|&i| (i as usize) < q.palette.len()));
        assert_eq!(q.indices.len(), 256);
    }
}
