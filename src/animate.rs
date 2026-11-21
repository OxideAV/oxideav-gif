//! Animation assembly: from the pictures an animation should show to
//! the smallest set of §20 images that shows them.
//!
//! The input is a sequence of *targets* — the full logical screen as it
//! must look while each frame is displayed — with a delay each. The
//! output draws every target by changing as little of the screen as
//! possible:
//!
//! * **Duplicate frames** — a target equal to the one before it draws
//!   nothing; its delay is added to the previous image's (§23.c.vii).
//! * **Changed rectangle** — each image covers only the bounding box of
//!   the pixels that differ from what is already on the screen
//!   (§20.c.ii–v place an image anywhere on the logical screen).
//! * **Transparent deltas** — inside that rectangle, pixels that do not
//!   change carry the §23.c.viii Transparency Index ("the corresponding
//!   pixel of the display device is not modified"), so an image is long
//!   runs of one index with the changes in between.
//! * **Disposal** — an image whose pixels stay on screen is "do not
//!   dispose" (§23.c.iv value 1). A screen position that is opaque in
//!   one target and transparent in the next cannot be undone by drawing,
//!   so no image is ever left standing on such a position: an image that
//!   covers one is "restore to previous" (value 3) and vanishes with its
//!   frame. "Restore to background" is never written: what a reader
//!   paints for it depends on whether it honours the Background Color
//!   Index, and nothing here relies on that.
//! * **Colour tables** — when every colour of the animation fits one
//!   table it is the Global Color Table and the encoding is exact.
//!   Otherwise a global table is clustered from the pixels that change,
//!   and each image elects between it and a Local Color Table clustered
//!   from its own changed pixels: the local table wins when it renders
//!   the image visibly better. An image that uses only a few entries of
//!   a large table elects a small local table when the shorter LZW codes
//!   save more bytes than the table costs.
//! * **Temporal stability** — a pixel whose source colour has not moved
//!   (by more than [`EncodeOptions::temporal_tolerance`]) since it was
//!   last drawn is not drawn again, whatever the dither: static regions
//!   keep the exact indices they were given, so they neither shimmer nor
//!   cost bytes.

use crate::cluster::{Census, Sample};
use crate::colorspace::Metric;
use crate::encoder::{ColorTableMode, EncodeOptions};
use crate::error::{Error, Result};
use crate::image::{Block, DisposalMethod, GifFile, GifFrameData, GraphicControl, Rgb, Version};
use crate::lzw::{ClearPolicy, LzwEncoder};
use crate::nearest::Nearest;
use crate::quantize::{DitherInput, PaletteSearch, assign_indices_with, palette_from_census};

/// A screen pixel: `0` when transparent, else `0xFF00_0000 | R G B`.
pub(crate) type Pixel = u32;

const TRANSPARENT: Pixel = 0;

#[inline]
pub(crate) fn pixel(rgba: [u8; 4]) -> Pixel {
    if rgba[3] < crate::quantize::ALPHA_OPAQUE_THRESHOLD {
        TRANSPARENT
    } else {
        0xFF00_0000 | u32::from(rgba[0]) << 16 | u32::from(rgba[1]) << 8 | u32::from(rgba[2])
    }
}

#[inline]
fn rgb_of(px: Pixel) -> [u8; 3] {
    [(px >> 16) as u8, (px >> 8) as u8, px as u8]
}

#[inline]
fn opaque_pixel(c: Rgb) -> Pixel {
    0xFF00_0000 | u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
}

/// What an animation frame carries besides its picture.
#[derive(Clone, Copy)]
pub(crate) struct Timing {
    pub(crate) delay_centis: u16,
    pub(crate) user_input: bool,
}

/// Follows the screen through a sequence of full-screen RGBA pictures
/// drawn in turn with their §23.c.iv disposal: a picture's transparent
/// pixels show what the previous frames left; "restore to background"
/// leaves an empty (transparent) screen, "restore to previous" the
/// screen as it was before the picture.
pub(crate) struct ScreenTrack {
    left: Vec<Pixel>,
}

impl ScreenTrack {
    pub(crate) fn new(pixel_count: usize) -> Self {
        ScreenTrack {
            left: vec![TRANSPARENT; pixel_count],
        }
    }

    /// The screen while `rgba` is displayed.
    pub(crate) fn show(&mut self, rgba: &[u8], disposal: DisposalMethod) -> Vec<Pixel> {
        let shown: Vec<Pixel> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(&self.left)
            .map(|(&px, &under)| match pixel(px) {
                TRANSPARENT => under,
                over => over,
            })
            .collect();
        match disposal {
            DisposalMethod::None | DisposalMethod::Keep => self.left.clone_from(&shown),
            DisposalMethod::RestoreBackground => self.left.fill(TRANSPARENT),
            DisposalMethod::RestorePrevious => {}
        }
        shown
    }
}

/// A colour table under construction: the opaque entries, plus whether
/// one more slot (the last) is the Transparency Index.
#[derive(Clone)]
struct Table {
    colors: Vec<Rgb>,
    transparent: bool,
    /// Every colour mapped onto this table is one of its entries.
    exact: bool,
}

impl Table {
    fn transparent_index(&self) -> Option<u8> {
        self.transparent.then_some(self.colors.len() as u8)
    }

    fn entries(&self) -> Vec<Rgb> {
        let mut out = self.colors.clone();
        if self.transparent {
            out.push(Rgb::new(0, 0, 0));
        }
        out
    }
}

struct Rect {
    left: usize,
    top: usize,
    width: usize,
    height: usize,
}

/// Bounding box of the set positions of `mask` (a `width`-wide grid).
fn bounding_box(mask: &[bool], width: usize) -> Option<Rect> {
    let mut rows = mask.chunks_exact(width).enumerate();
    let top = rows.find(|(_, r)| r.contains(&true))?.0;
    let bottom = mask
        .chunks_exact(width)
        .rposition(|r| r.contains(&true))
        .unwrap_or(top);
    let (mut left, mut right) = (width, 0);
    for row in mask.chunks_exact(width).take(bottom + 1).skip(top) {
        if let Some(first) = row.iter().position(|&m| m) {
            left = left.min(first);
            right = right.max(row.iter().rposition(|&m| m).unwrap_or(first));
        }
    }
    Some(Rect {
        left,
        top,
        width: right - left + 1,
        height: bottom - top + 1,
    })
}

/// Squared error (in visibility-floor units, summed over an image's
/// drawn pixels) a Local Color Table must remove to be elected over the
/// global one: about two thousand pixels each brought one
/// just-noticeable difference closer.
const LOCAL_TABLE_GAIN: f64 = 2048.0;

/// Bits needed to index a table of `n` entries, floored at the LZW
/// minimum of 2.
fn code_size_for(n: usize) -> u8 {
    let mut bits = 1u8;
    while (1usize << bits) < n {
        bits += 1;
    }
    bits.max(2)
}

/// Entries a table of `n` colours occupies on the wire (the next power
/// of two, at least 2).
fn wire_entries(n: usize) -> usize {
    n.max(2).next_power_of_two()
}

/// Weighted mean squared distance from each census point to its nearest
/// table entry, in `metric`.
fn table_error(samples: &[Sample], colors: &[Rgb], metric: Metric) -> f64 {
    if samples.is_empty() || colors.is_empty() {
        return 0.0;
    }
    let points: Vec<[f32; 3]> = colors
        .iter()
        .map(|c| metric.point([c.r, c.g, c.b]))
        .collect();
    let search = Nearest::new(&points);
    let (mut err, mut weight) = (0.0f64, 0.0f64);
    for s in samples {
        err += f64::from(s.weight) * f64::from(search.find(s.at).1);
        weight += f64::from(s.weight);
    }
    err / weight.max(f64::MIN_POSITIVE)
}

/// Build the animation that shows `targets` (each `width × height`
/// screen pixels) for the delays in `timings`.
///
/// With `lossless`, `Ok(None)` is returned instead of a stream in which
/// any pixel would differ from its target.
pub(crate) fn build(
    targets: Vec<Vec<Pixel>>,
    timings: Vec<Timing>,
    size: (u16, u16),
    opts: &EncodeOptions,
    lossless: bool,
) -> Result<Option<GifFile>> {
    Ok(build_tracked(targets, timings, size, opts, lossless)?.map(|b| b.file))
}

/// A built animation and, for each of its images, the index of the first
/// input frame it shows (later duplicates were merged into it).
pub(crate) struct Built {
    pub(crate) file: GifFile,
    pub(crate) first_source: Vec<usize>,
}

/// [`build`], also reporting which input frames each image stands for.
pub(crate) fn build_tracked(
    targets: Vec<Vec<Pixel>>,
    timings: Vec<Timing>,
    (width, height): (u16, u16),
    opts: &EncodeOptions,
    lossless: bool,
) -> Result<Option<Built>> {
    let (w, h) = (width as usize, height as usize);
    let pixel_count = w * h;
    if pixel_count == 0 || targets.is_empty() || targets.len() != timings.len() {
        return Err(Error::invalid_input(
            "GIF animation: needs at least one frame of non-zero size",
        ));
    }
    if targets.iter().any(|t| t.len() != pixel_count) {
        return Err(Error::invalid_input(
            "GIF animation: every frame must cover the logical screen",
        ));
    }

    // ---- duplicate frames become longer delays --------------------------
    let mut frames: Vec<(Vec<Pixel>, Timing)> = Vec::with_capacity(targets.len());
    let mut first_source: Vec<usize> = Vec::with_capacity(targets.len());
    for (source, (target, timing)) in targets.into_iter().zip(timings).enumerate() {
        if let Some((shown, held)) = frames.last_mut()
            && *shown == target
            && !held.user_input
            && !timing.user_input
            && let Some(sum) = held.delay_centis.checked_add(timing.delay_centis)
        {
            held.delay_centis = sum;
            continue;
        }
        frames.push((target, timing));
        first_source.push(source);
    }

    // ---- positions no image may be left standing on ---------------------
    let mut vanishing = vec![false; pixel_count];
    let mut any_vanishing = false;
    for pair in frames.windows(2) {
        for ((flag, &before), &after) in vanishing.iter_mut().zip(&pair[0].0).zip(&pair[1].0) {
            if before != TRANSPARENT && after == TRANSPARENT {
                *flag = true;
                any_vanishing = true;
            }
        }
    }

    // ---- the global table -----------------------------------------------
    let tolerance = if lossless {
        0
    } else {
        i32::from(opts.temporal_tolerance)
    };
    let quant = opts.quantize;
    let metric = match quant.metric {
        crate::quantize::ColorMetric::Rgb => Metric::Rgb,
        _ => Metric::Lab,
    };
    let budget = quant.max_colors.clamp(1, crate::quantize::MAX_PALETTE);
    let any_alpha = frames.iter().any(|(t, _)| t.contains(&TRANSPARENT));

    // Census of what will be drawn, to a first approximation: the first
    // screen, then every pixel that differs from the frame before.
    let mut census = Census::new();
    for (i, (target, _)) in frames.iter().enumerate() {
        if i == 0 {
            for &px in target.iter().filter(|&&px| px != TRANSPARENT) {
                census.add(rgb_of(px));
            }
        } else {
            for (&px, &before) in target.iter().zip(&frames[i - 1].0) {
                if px != TRANSPARENT && px != before {
                    census.add(rgb_of(px));
                }
            }
        }
    }
    // One slot of the budget is the Transparency Index whenever the
    // pictures themselves have transparent pixels.
    let colour_budget = (budget - usize::from(any_alpha)).max(1);
    let mut global = if let Some(colors) = census.exact(colour_budget) {
        // Everything fits one table. Unchanged pixels are skipped through
        // a transparent slot when there is room for one; otherwise they
        // are redrawn as they are.
        let room = colors.len() < budget.max(2);
        Table {
            colors: colors
                .into_iter()
                .map(|[r, g, b]| Rgb::new(r, g, b))
                .collect(),
            transparent: any_alpha || (frames.len() > 1 && room),
            exact: true,
        }
    } else {
        let transparent = any_alpha || frames.len() > 1;
        Table {
            colors: palette_from_census(
                &census,
                (budget - usize::from(transparent)).max(1),
                metric,
                quant.palette_refine_iterations,
            ),
            transparent,
            exact: false,
        }
    };
    if global.colors.is_empty() {
        global.colors.push(Rgb::new(0, 0, 0));
    }
    let mut global_search = PaletteSearch::new(&global.colors, &quant);
    let single = frames.len() == 1;

    // ---- one image per frame --------------------------------------------
    let mut screen: Vec<Pixel> = vec![TRANSPARENT; pixel_count];
    // The source colour each screen pixel was drawn from.
    let mut screen_source: Vec<Pixel> = vec![TRANSPARENT; pixel_count];
    let mut need = vec![false; pixel_count];
    let mut lzw = LzwEncoder::new();
    let mut blocks: Vec<Block> = Vec::with_capacity(frames.len() + 1);
    let mut global_used = false;
    let mut largest_table = 1usize;

    for (i, (target, timing)) in frames.iter().enumerate() {
        // Which pixels must be drawn.
        let mut covers_vanishing = false;
        for (p, flag) in need.iter_mut().enumerate() {
            let want = target[p];
            *flag = if want == TRANSPARENT {
                false
            } else {
                covers_vanishing |= vanishing[p];
                let have = screen[p];
                have == TRANSPARENT || !close(want, screen_source[p], tolerance)
            };
        }
        let rect = if i == 0 {
            // The first image defines the screen for readers that paint
            // uncovered positions with a background colour.
            Rect {
                left: 0,
                top: 0,
                width: w,
                height: h,
            }
        } else {
            bounding_box(&need, w).unwrap_or(Rect {
                left: 0,
                top: 0,
                width: 1,
                height: 1,
            })
        };
        let disposal = if single {
            DisposalMethod::None
        } else if any_vanishing && covers_vanishing {
            DisposalMethod::RestorePrevious
        } else {
            DisposalMethod::Keep
        };

        // The rectangle's pixels and which of them are drawn.
        let area = rect.width * rect.height;
        let mut samples: Vec<[u8; 3]> = Vec::with_capacity(area);
        let mut skip: Vec<bool> = Vec::with_capacity(area);
        for y in rect.top..rect.top + rect.height {
            let row = y * w + rect.left;
            for p in row..row + rect.width {
                samples.push(rgb_of(target[p]));
                skip.push(!need[p]);
            }
        }

        // Elect the table.
        let any_skip = skip.contains(&true);
        let mut frame_census = Census::new();
        for (c, _) in samples.iter().zip(&skip).filter(|(_, s)| !**s) {
            frame_census.add(*c);
        }
        let local_wanted = match opts.color_table {
            ColorTableMode::Global => None,
            ColorTableMode::Local => {
                Some(local_table(&frame_census, budget, any_skip, metric, opts))
            }
            ColorTableMode::Auto => {
                if global.exact || frame_census.is_empty() {
                    None
                } else {
                    // A table of the image's own is elected when the
                    // global one leaves a visible error and the local
                    // one removes enough of it, summed over the drawn
                    // pixels, to be worth the bytes of the table.
                    let points = frame_census.samples(metric);
                    let drawn: f64 = points.iter().map(|s| f64::from(s.weight)).sum();
                    let floor = f64::from(metric.visibility_floor_squared());
                    let global_error = table_error(&points, &global.colors, metric);
                    if global_error <= floor / 4.0 {
                        None
                    } else {
                        let local = local_table(&frame_census, budget, any_skip, metric, opts);
                        let local_error = table_error(&points, &local.colors, metric);
                        let gain = (global_error - local_error) * drawn;
                        (global_error > 1.05 * local_error && gain >= LOCAL_TABLE_GAIN * floor)
                            .then_some(local)
                    }
                }
            }
        };
        let mut local_search;
        let (table, search) = match &local_wanted {
            Some(local) => {
                local_search = PaletteSearch::new(&local.colors, &quant);
                (local, &mut local_search)
            }
            None => (&global, &mut global_search),
        };
        if lossless && !table.exact {
            return Ok(None);
        }

        // Map the drawn pixels; skipped ones take the transparent slot,
        // or — in a table without one — are redrawn as they stand.
        let transparent_index = table.transparent_index();
        let mut indices = if transparent_index.is_some() {
            assign_indices_with(
                &quant,
                DitherInput {
                    samples: &samples,
                    width: rect.width,
                    height: rect.height,
                    palette: &table.colors,
                    is_transparent: &skip,
                    transparent_index,
                    origin: (rect.left, rect.top),
                },
                search,
            )
        } else {
            samples.iter().map(|&c| search.nearest(c)).collect()
        };

        // A drawn pixel that came out as the colour already on screen
        // can be skipped instead. Runs of the transparent index compress
        // well, isolated ones in the middle of a texture do not, so the
        // variant is kept only when its LZW stream is the shorter one.
        if let Some(t) = transparent_index
            && i > 0
        {
            let mut variant = indices.clone();
            let mut coincidences = 0usize;
            let mut at = 0;
            for y in rect.top..rect.top + rect.height {
                let row = y * w + rect.left;
                for &on_screen in &screen[row..row + rect.width] {
                    if !skip[at] && opaque_pixel(table.colors[indices[at] as usize]) == on_screen {
                        variant[at] = t;
                        coincidences += 1;
                    }
                    at += 1;
                }
            }
            if coincidences > 0 {
                let bits = code_size_for(table.colors.len() + 1);
                let shape = (rect.width, rect.height);
                let size = |lzw: &mut LzwEncoder, plane: &[u8]| {
                    packed_size(lzw, bits, plane, opts.interlace, shape)
                };
                if size(&mut lzw, &variant) < size(&mut lzw, &indices) {
                    indices = variant;
                }
            }
        }

        // Record what the screen now shows.
        let keeps = disposal != DisposalMethod::RestorePrevious;
        let mut uses_transparent = false;
        let mut at = 0;
        for y in rect.top..rect.top + rect.height {
            let row = y * w + rect.left;
            for p in row..row + rect.width {
                if transparent_index == Some(indices[at]) {
                    uses_transparent = true;
                } else if keeps {
                    screen[p] = opaque_pixel(table.colors[indices[at] as usize]);
                    screen_source[p] = target[p];
                }
                at += 1;
            }
        }

        // A small local table when few entries of a large one are used.
        let mut entries = table.entries();
        let mut local_palette = local_wanted.is_some().then(|| entries.clone());
        let mut transparent_written = transparent_index.filter(|_| uses_transparent);
        if opts.color_table != ColorTableMode::Global
            && let Some((compact, remapped, transparent)) = compact_table(
                &entries,
                &indices,
                transparent_written,
                local_palette.is_some(),
                opts.interlace,
                (rect.width, rect.height),
                &mut lzw,
            )
        {
            entries = compact;
            indices = remapped;
            transparent_written = transparent;
            local_palette = Some(entries.clone());
        }
        if local_palette.is_none() {
            global_used = true;
        }
        largest_table = largest_table.max(entries.len());

        let wants_control = !single
            || transparent_written.is_some()
            || timing.delay_centis != 0
            || timing.user_input;
        let graphic_control = wants_control.then_some(GraphicControl {
            disposal,
            user_input: timing.user_input,
            transparent_index: transparent_written,
            delay_centis: timing.delay_centis,
        });
        blocks.push(Block::Image(GifFrameData {
            left: rect.left as u16,
            top: rect.top as u16,
            width: rect.width as u16,
            height: rect.height as u16,
            local_palette,
            palette_sorted: false,
            interlaced: opts.interlace,
            indices,
            graphic_control,
        }));
    }

    let global_palette =
        (opts.color_table != ColorTableMode::Local && global_used).then(|| global.entries());
    if let Some(g) = &global_palette {
        largest_table = largest_table.max(g.len());
    }
    let file = GifFile {
        version: Version::Gif89a,
        screen_width: width,
        screen_height: height,
        color_resolution: (wire_entries(largest_table).trailing_zeros() as u8).clamp(1, 8) - 1,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette,
        blocks,
    };
    Ok(Some(Built { file, first_source }))
}

/// Rebuild `file` as inter-frame deltas showing exactly the same screens
/// for the same times, keeping its other blocks.
///
/// `None` when the stream is not one this can restructure without risk:
/// it has no image, has a §25 Plain Text block, does not compose, uses
/// "restore to background" while a Background Color is defined (readers
/// disagree on what that paints, so the frames are left as they are), or
/// needs more colours in one image than a table holds.
pub(crate) fn reoptimize(file: &GifFile) -> Option<GifFile> {
    let mut timings = Vec::new();
    for block in &file.blocks {
        match block {
            Block::Image(frame) => {
                let gce = frame.graphic_control;
                if gce.is_some_and(|g| g.disposal == DisposalMethod::RestoreBackground)
                    && file.background_color().is_some()
                {
                    return None;
                }
                timings.push(Timing {
                    delay_centis: gce.map_or(0, |g| g.delay_centis),
                    user_input: gce.is_some_and(|g| g.user_input),
                });
            }
            Block::PlainText { .. } => return None,
            _ => {}
        }
    }
    if timings.is_empty() {
        return None;
    }
    let composed = crate::compose::compose(file).ok()?;
    let screens = |frames: &[crate::compose::ComposedFrame]| -> Vec<Vec<Pixel>> {
        frames
            .iter()
            .map(|f| {
                f.canvas
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&px| pixel(px))
                    .collect()
            })
            .collect()
    };
    let targets = screens(&composed);
    let opts = EncodeOptions::default().with_interlace(file.all_frames_interlaced());
    let size = (file.screen_width, file.screen_height);
    let Built {
        file: mut rebuilt,
        first_source,
    } = build_tracked(targets.clone(), timings, size, &opts, true).ok()??;

    // Stream-level fields and every non-image block carry over; a block
    // that stood before an image stands before the image that now shows
    // that frame.
    rebuilt.color_resolution = file.color_resolution;
    rebuilt.pixel_aspect_ratio = file.pixel_aspect_ratio;
    rebuilt.background_index = match &rebuilt.global_palette {
        Some(table) if usize::from(file.background_index) < table.len() => file.background_index,
        _ => 0,
    };
    let mut images = std::mem::take(&mut rebuilt.blocks).into_iter().peekable();
    let mut emitted = 0usize;
    let mut source_images = 0usize;
    for block in &file.blocks {
        match block {
            Block::Image(_) => source_images += 1,
            other => {
                while emitted < first_source.len() && first_source[emitted] < source_images {
                    rebuilt.blocks.extend(images.next());
                    emitted += 1;
                }
                rebuilt.blocks.push(other.clone());
            }
        }
    }
    rebuilt.blocks.extend(images);
    rebuilt.version = file.version;
    rebuilt.upgrade_version_if_needed();

    // The safety net: every input frame must find its exact screen.
    let recomposed = screens(&crate::compose::compose(&rebuilt).ok()?);
    if recomposed.len() != first_source.len() {
        return None;
    }
    let mut image = 0usize;
    for (source, target) in targets.iter().enumerate() {
        while image + 1 < first_source.len() && first_source[image + 1] <= source {
            image += 1;
        }
        if recomposed[image] != *target {
            return None;
        }
    }
    Some(rebuilt)
}

/// Whether `a` is within `tolerance` of `b` on every channel (both
/// opaque).
#[inline]
fn close(a: Pixel, b: Pixel, tolerance: i32) -> bool {
    if a == b {
        return true;
    }
    if tolerance == 0 || b == TRANSPARENT {
        return false;
    }
    let (a, b) = (rgb_of(a), rgb_of(b));
    (0..3).all(|c| (i32::from(a[c]) - i32::from(b[c])).abs() <= tolerance)
}

/// A table clustered from one image's drawn pixels, with a transparent
/// slot when the image has pixels to skip.
fn local_table(
    census: &Census,
    budget: usize,
    transparent: bool,
    metric: Metric,
    opts: &EncodeOptions,
) -> Table {
    let colour_budget = (budget - usize::from(transparent)).max(1);
    let exact = census.exact(colour_budget);
    let is_exact = exact.is_some();
    let mut colors: Vec<Rgb> = match exact {
        Some(colors) => colors
            .into_iter()
            .map(|[r, g, b]| Rgb::new(r, g, b))
            .collect(),
        None => palette_from_census(
            census,
            colour_budget,
            metric,
            opts.quantize.palette_refine_iterations,
        ),
    };
    if colors.is_empty() {
        colors.push(Rgb::new(0, 0, 0));
    }
    Table {
        colors,
        transparent,
        exact: is_exact,
    }
}

/// Try the table that holds only the entries `indices` uses. Returns it
/// (with the remapped indices and Transparency Index) when it makes the
/// image smaller: its LZW stream under the shorter codes, plus the table
/// itself when the image would otherwise have used the global one, is
/// compared with the stream under the full table.
fn compact_table(
    entries: &[Rgb],
    indices: &[u8],
    transparent: Option<u8>,
    already_local: bool,
    interlace: bool,
    (width, height): (usize, usize),
    lzw: &mut LzwEncoder,
) -> Option<(Vec<Rgb>, Vec<u8>, Option<u8>)> {
    let mut used = [false; 256];
    for &i in indices {
        used[i as usize] = true;
    }
    let used_count = used.iter().filter(|&&u| u).count();
    let full_bits = code_size_for(entries.len());
    let compact_bits = code_size_for(used_count);
    if compact_bits >= full_bits && !(already_local && used_count < entries.len()) {
        return None;
    }
    let mut remap = [0u8; 256];
    let mut compact = Vec::with_capacity(used_count);
    for (i, _) in used.iter().enumerate().filter(|(_, u)| **u) {
        remap[i] = compact.len() as u8;
        compact.push(entries[i]);
    }
    let remapped: Vec<u8> = indices.iter().map(|&i| remap[i as usize]).collect();
    let transparent = transparent.map(|t| remap[t as usize]);
    if already_local {
        // Dropping unused entries of a local table never costs anything.
        return Some((compact, remapped, transparent));
    }
    let shape = (width, height);
    let with_full = packed_size(lzw, full_bits, indices, interlace, shape);
    let with_compact = packed_size(lzw, compact_bits, &remapped, interlace, shape)
        .saturating_add(3 * wire_entries(used_count));
    (with_compact < with_full).then_some((compact, remapped, transparent))
}

/// Bytes of the LZW stream of an index plane, in the row order it will
/// be stored in.
fn packed_size(
    lzw: &mut LzwEncoder,
    bits: u8,
    plane: &[u8],
    interlace: bool,
    (width, height): (usize, usize),
) -> usize {
    let stored: Vec<u8>;
    let rows: &[u8] = if interlace {
        stored = crate::interlace::interlace_row_order(height as u16)
            .iter()
            .flat_map(|&r| {
                plane[r as usize * width..(r as usize + 1) * width]
                    .iter()
                    .copied()
            })
            .collect();
        &stored
    } else {
        plane
    };
    lzw.encode_frame_with(bits, rows, ClearPolicy::Monitored)
        .map_or(usize::MAX, |b| b.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounding_box_is_tight() {
        let mut mask = vec![false; 6 * 5];
        assert!(bounding_box(&mask, 6).is_none());
        mask[6 + 2] = true;
        mask[3 * 6 + 4] = true;
        let r = bounding_box(&mask, 6).unwrap();
        assert_eq!((r.left, r.top, r.width, r.height), (2, 1, 3, 3));
    }

    #[test]
    fn pictures_compose_into_screens() {
        let a = [10u8, 0, 0, 255, 20, 0, 0, 255];
        let b = [0u8, 0, 0, 0, 30, 0, 0, 255];
        let c = [0u8, 0, 0, 0, 0, 0, 0, 0];
        let mut track = ScreenTrack::new(2);
        let first = track.show(&a, DisposalMethod::Keep);
        let second = track.show(&b, DisposalMethod::RestoreBackground);
        let third = track.show(&c, DisposalMethod::RestorePrevious);
        let fourth = track.show(&a, DisposalMethod::None);
        assert_eq!(first, vec![pixel([10, 0, 0, 255]), pixel([20, 0, 0, 255])]);
        assert_eq!(second, vec![pixel([10, 0, 0, 255]), pixel([30, 0, 0, 255])]);
        assert_eq!(third, vec![TRANSPARENT, TRANSPARENT]);
        assert_eq!(fourth, first);
    }

    #[test]
    fn closeness_is_per_channel() {
        let a = pixel([10, 20, 30, 255]);
        assert!(close(a, a, 0));
        assert!(!close(a, pixel([11, 20, 30, 255]), 0));
        assert!(close(a, pixel([12, 18, 30, 255]), 2));
        assert!(!close(a, pixel([13, 20, 30, 255]), 2));
        assert!(!close(a, TRANSPARENT, 255));
    }
}
