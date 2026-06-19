//! Non-fatal Appendix-B grammar + cross-section conformance reporting
//! for an in-memory [`GifImage`].
//!
//! The [`crate::encoder`] enforces the small set of constraints whose
//! violation makes a stream *un-encodable* (palette length, declared
//! version vs. block content, `indices.len() == width × height`). Those
//! are fatal: [`crate::encode`] returns [`Error::InvalidInput`] before
//! a byte goes to the wire.
//!
//! This module is the complementary *diagnostic* surface: it walks a
//! [`GifImage`] and reports **every** way it departs from the
//! CompuServe Appendix-B grammar and the surrounding §18–§26 field
//! rules — including the **recommendation**-level departures the
//! encoder deliberately tolerates (a frame escaping the §20.a Logical
//! Screen boundary, a §18.c.vii Background Color Index past the end of
//! the Global Color Table, a pixel index that references past the
//! active palette, a §23.e.ii User Input Flag set with no Delay Time,
//! a §25 Plain Text block with no active colour table to resolve its
//! fg/bg indices, …). None of these aborts an encode; all of them are
//! things a *pre-flight linter* wants surfaced.
//!
//! The report never mutates the image and never fails: it returns a
//! `Vec<`[`ConformanceIssue`]`>` (empty when the image is fully
//! conformant). Each issue carries a [`ConformanceSeverity`] so a
//! caller can gate on errors while merely logging recommendations.
//!
//! # Spec references
//!
//! * Appendix B "GIF Grammar" — `<GIF Data Stream> ::= Header
//!   <Logical Screen> <Data>* Trailer`, `<Logical Screen> ::= Logical
//!   Screen Descriptor [Global Color Table]`,
//!   `<Table-Based Image> ::= Image Descriptor [Local Color Table]
//!   Image Data`. The in-memory [`GifImage`] is already shaped to this
//!   grammar (the decoder builds it that way and the §23 GCE is stored
//!   *attached* to the graphic-rendering block it scopes), so the
//!   structural production rules are satisfied by construction. What a
//!   hand-built or mutated [`GifImage`] can still violate is the
//!   *field-level* and *cross-reference* conformance the grammar
//!   leaves to the per-block sections below.
//! * §7 "Version Numbers" — the declared version must cover every
//!   block's Required Version.
//! * §18 "Logical Screen Descriptor" — Color Resolution range
//!   (§18.c.iv), Background Color Index reference (§18.c.vii), Pixel
//!   Aspect Ratio (§18.c.viii, recommendation-level).
//! * §19 / §21 "Color Table" — a colour table holds 2..=256 entries.
//! * §20 "Image Descriptor" — §20.a "Each image must fit within the
//!   boundaries of the Logical Screen"; `indices.len() == width ×
//!   height`.
//! * §22 / Appendix F — pixel indices must reference a present palette.
//! * §23 "Graphic Control Extension" — §23.c.viii Transparent Color
//!   Index reference, §23.e.ii User-Input-without-Delay recommendation.
//! * §25 "Plain Text Extension" — fg/bg index references against the
//!   active colour table.

use crate::image::{Block, GifImage, Rgb};
use core::fmt;

/// How seriously a [`ConformanceIssue`] departs from the spec.
///
/// The split mirrors the CompuServe spec's own language: a field that
/// *must* hold a value ("This field contains the fixed value …", "must
/// fit within the boundaries", "is present if and only if") is an
/// [`Error`](ConformanceSeverity::Error); a clause the spec phrases as
/// a *recommendation* ("it is recommended that …", "the encoder should
/// …") is a [`Recommendation`](ConformanceSeverity::Recommendation).
///
/// A caller building a strict validator gates on
/// [`ConformanceReport::has_errors`]; a caller building a lint surfaces
/// both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConformanceSeverity {
    /// A hard departure from a spec *requirement*. A renderer following
    /// the spec literally cannot process the affected field as written
    /// (e.g. a pixel index that references past the end of the only
    /// available palette, a frame rectangle that escapes the Logical
    /// Screen, a colour table with zero or >256 entries).
    Error,
    /// A departure from a spec *recommendation*. The stream is still
    /// processable, but it ignores guidance the spec offers for
    /// interoperability (e.g. §23.e.ii "the encoder [should] not set
    /// the User Input Flag without a Delay Time specified").
    Recommendation,
}

/// The spec section a [`ConformanceIssue`] is grounded in, kept as a
/// machine-comparable tag (the human string lives in
/// [`ConformanceIssue::detail`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConformanceRule {
    /// §7 — declared version does not cover a block's Required Version.
    VersionTooLow,
    /// §18.c.iv — Color Resolution field outside the 3-bit `0..=7` range.
    ColorResolutionRange,
    /// §18.c.vii — Background Color Index references past the end of the
    /// Global Color Table (only meaningful when a GCT is present).
    BackgroundIndexRange,
    /// §19 / §21 — a colour table has 0 entries or more than 256.
    ColorTableSize,
    /// §20.a — an image's placement rectangle escapes the Logical Screen.
    FrameEscapesScreen,
    /// §20 / §22 — `indices.len() != width × height` for an image.
    FrameIndicesLength,
    /// §22 / Appendix F — a pixel index references past the active
    /// palette, or the image has no active palette at all.
    PixelIndexRange,
    /// §23.c.viii — the Transparent Color Index references past the
    /// active palette.
    TransparentIndexRange,
    /// §23.e.ii — the User Input Flag is set with no Delay Time
    /// (recommendation).
    UserInputWithoutDelay,
    /// §25 — a Plain Text fg/bg index references past the active palette,
    /// or the block has no active palette to resolve them.
    PlainTextIndexRange,
}

/// One conformance departure found while walking a [`GifImage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceIssue {
    /// Which spec rule was departed from.
    pub rule: ConformanceRule,
    /// How seriously (error vs. recommendation).
    pub severity: ConformanceSeverity,
    /// Index into [`GifImage::blocks`] of the offending block, or
    /// `None` for a stream-level issue (Logical Screen Descriptor
    /// fields, Global Color Table).
    pub block_index: Option<usize>,
    /// Human-readable spec-cited description of the departure.
    pub detail: String,
}

impl fmt::Display for ConformanceSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConformanceSeverity::Error => f.write_str("error"),
            ConformanceSeverity::Recommendation => f.write_str("recommendation"),
        }
    }
}

impl fmt::Display for ConformanceIssue {
    /// Renders as `error [block 2]: §20.a: …` (or `error: …` for a
    /// stream-level issue with no block index).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.block_index {
            Some(idx) => write!(f, "{} [block {idx}]: {}", self.severity, self.detail),
            None => write!(f, "{}: {}", self.severity, self.detail),
        }
    }
}

/// The full set of conformance departures for a [`GifImage`], in the
/// order they were discovered (stream-level first, then per-block in
/// source order).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConformanceReport {
    issues: Vec<ConformanceIssue>,
}

impl ConformanceReport {
    /// Every issue found, in discovery order.
    pub fn issues(&self) -> &[ConformanceIssue] {
        &self.issues
    }

    /// `true` when the image is fully conformant — no errors *and* no
    /// recommendations.
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }

    /// `true` when at least one [`ConformanceSeverity::Error`] was found.
    /// Recommendations alone do not make this `true`.
    pub fn has_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|i| i.severity == ConformanceSeverity::Error)
    }

    /// Iterate only the [`ConformanceSeverity::Error`] issues.
    pub fn errors(&self) -> impl Iterator<Item = &ConformanceIssue> {
        self.issues
            .iter()
            .filter(|i| i.severity == ConformanceSeverity::Error)
    }

    /// Iterate only the [`ConformanceSeverity::Recommendation`] issues.
    pub fn recommendations(&self) -> impl Iterator<Item = &ConformanceIssue> {
        self.issues
            .iter()
            .filter(|i| i.severity == ConformanceSeverity::Recommendation)
    }

    /// Number of issues with the given severity.
    pub fn count(&self, severity: ConformanceSeverity) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == severity)
            .count()
    }

    fn push(
        &mut self,
        rule: ConformanceRule,
        severity: ConformanceSeverity,
        block_index: Option<usize>,
        detail: String,
    ) {
        self.issues.push(ConformanceIssue {
            rule,
            severity,
            block_index,
            detail,
        });
    }
}

impl fmt::Display for ConformanceReport {
    /// One issue per line, in discovery order. A clean report renders as
    /// the single line `conformant: no issues`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.issues.is_empty() {
            return f.write_str("conformant: no issues");
        }
        for (i, issue) in self.issues.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{issue}")?;
        }
        Ok(())
    }
}

impl GifImage {
    /// Produce a non-fatal [`ConformanceReport`] for this image,
    /// checking it against the Appendix-B grammar and the §7–§26
    /// field-level rules.
    ///
    /// This never mutates `self` and never fails — it returns the
    /// (possibly empty) list of departures. It is the diagnostic
    /// counterpart to [`crate::encode`]'s fatal validation: a clean
    /// report (`report.is_clean()`) implies `encode` will not reject
    /// the image, but a report with only
    /// [`ConformanceSeverity::Recommendation`] issues *also* encodes
    /// fine — the recommendations are interoperability guidance the
    /// encoder tolerates.
    ///
    /// # Example
    ///
    /// ```
    /// use oxideav_gif::AnimationBuilder;
    /// use oxideav_gif::{DisposalMethod, Rgb};
    ///
    /// let palette = vec![Rgb::new(0xFF, 0, 0), Rgb::new(0, 0xFF, 0)];
    /// let img = AnimationBuilder::new(2, 2, palette)
    ///     .add_full_frame(vec![0, 1, 0, 1], 10, DisposalMethod::None)
    ///     .unwrap()
    ///     .build()
    ///     .unwrap();
    /// assert!(img.conformance_report().is_clean());
    /// ```
    pub fn conformance_report(&self) -> ConformanceReport {
        let mut report = ConformanceReport::default();

        // ---- §7 Version Numbers ----
        let required = self.required_version();
        if required > self.version {
            report.push(
                ConformanceRule::VersionTooLow,
                ConformanceSeverity::Error,
                None,
                format!(
                    "§7: stream declared {:?} but contains a block requiring {:?}",
                    self.version, required
                ),
            );
        }

        // ---- §18.c.iv Color Resolution ----
        if self.color_resolution > 7 {
            report.push(
                ConformanceRule::ColorResolutionRange,
                ConformanceSeverity::Error,
                None,
                format!(
                    "§18.c.iv: Color Resolution {} exceeds the 3-bit field range (0..=7)",
                    self.color_resolution
                ),
            );
        }

        // ---- §19 Global Color Table size + §18.c.vii Background Index ----
        if let Some(gct) = &self.global_palette {
            check_color_table_size(&mut report, gct, None, "§19 Global Color Table");
            // §18.c.vii: the Background Color Index is only meaningful
            // when a Global Color Table is present, and it indexes that
            // table. An index past the table's end has no colour to
            // resolve to.
            if !gct.is_empty() && (self.background_index as usize) >= gct.len() {
                report.push(
                    ConformanceRule::BackgroundIndexRange,
                    ConformanceSeverity::Error,
                    None,
                    format!(
                        "§18.c.vii: Background Color Index {} references past the {}-entry \
                         Global Color Table",
                        self.background_index,
                        gct.len()
                    ),
                );
            }
        }

        // ---- per-block walk ----
        let global = self.global_palette.as_deref();
        for (idx, block) in self.blocks.iter().enumerate() {
            match block {
                Block::Image(frame) => {
                    // §21 Local Color Table size.
                    if let Some(lct) = &frame.local_palette {
                        check_color_table_size(
                            &mut report,
                            lct,
                            Some(idx),
                            "§21 Local Color Table",
                        );
                    }

                    // §20.a "Each image must fit within the boundaries
                    // of the Logical Screen". left + width and top +
                    // height must not exceed the screen extents.
                    let right = frame.left as u32 + frame.width as u32;
                    let bottom = frame.top as u32 + frame.height as u32;
                    if right > self.screen_width as u32 || bottom > self.screen_height as u32 {
                        report.push(
                            ConformanceRule::FrameEscapesScreen,
                            ConformanceSeverity::Error,
                            Some(idx),
                            format!(
                                "§20.a: image rectangle ({},{})+{}×{} escapes the {}×{} \
                                 Logical Screen",
                                frame.left,
                                frame.top,
                                frame.width,
                                frame.height,
                                self.screen_width,
                                self.screen_height
                            ),
                        );
                    }

                    // §20 / §22: indices.len() must equal width × height.
                    let expected = frame.width as usize * frame.height as usize;
                    if frame.indices.len() != expected {
                        report.push(
                            ConformanceRule::FrameIndicesLength,
                            ConformanceSeverity::Error,
                            Some(idx),
                            format!(
                                "§20/§22: image pixel buffer holds {} indices but width×height \
                                 is {}",
                                frame.indices.len(),
                                expected
                            ),
                        );
                    }

                    // §22 / Appendix F: every pixel index must reference
                    // the active palette (Local Color Table per §21.a,
                    // else Global Color Table). With no active palette,
                    // there is nothing to render against.
                    let active = frame.local_palette.as_deref().or(global);
                    check_index_buffer(&mut report, &frame.indices, active, idx);

                    // §23.c.viii: the Transparent Color Index, when
                    // present, indexes the active palette.
                    if let Some(gce) = &frame.graphic_control {
                        check_gce(&mut report, gce, active, idx);
                    }
                }
                Block::PlainText {
                    params,
                    graphic_control,
                } => {
                    // §25 resolves fg/bg indices against the active
                    // colour table. A Plain Text Extension carries no
                    // Local Color Table of its own (§25 has no LCT
                    // field), so the active table is the Global Color
                    // Table.
                    if let Some(gct) = global {
                        if (params.fg_color_index as usize) >= gct.len() {
                            report.push(
                                ConformanceRule::PlainTextIndexRange,
                                ConformanceSeverity::Error,
                                Some(idx),
                                format!(
                                    "§25.c.x: Plain Text foreground index {} references past the \
                                     {}-entry active palette",
                                    params.fg_color_index,
                                    gct.len()
                                ),
                            );
                        }
                        if (params.bg_color_index as usize) >= gct.len() {
                            report.push(
                                ConformanceRule::PlainTextIndexRange,
                                ConformanceSeverity::Error,
                                Some(idx),
                                format!(
                                    "§25.c.xi: Plain Text background index {} references past the \
                                     {}-entry active palette",
                                    params.bg_color_index,
                                    gct.len()
                                ),
                            );
                        }
                    } else {
                        report.push(
                            ConformanceRule::PlainTextIndexRange,
                            ConformanceSeverity::Error,
                            Some(idx),
                            "§25: Plain Text Extension has no active colour table to resolve its \
                             foreground/background indices (no Global Color Table present)"
                                .to_string(),
                        );
                    }

                    if let Some(gce) = graphic_control {
                        check_gce(&mut report, gce, global, idx);
                    }
                }
                // §24 Comment / §26 Application are §12 Special-Purpose
                // blocks: "transparent to the decoding process", they
                // carry no field that references the screen or palette.
                Block::Comment(_) | Block::Application(_) => {}
            }
        }

        report
    }

    /// Gate on the *error*-level conformance issues: returns `Ok(())`
    /// when [`Self::conformance_report`] finds no
    /// [`ConformanceSeverity::Error`], else [`Error::InvalidInput`]
    /// carrying every error issue (one per line, recommendations
    /// excluded).
    ///
    /// This is the hard-gate convenience over the diagnostic
    /// [`Self::conformance_report`]: a caller that wants "reject this
    /// image if it is not spec-conformant, but tolerate
    /// recommendation-level departures" calls this; a caller that wants
    /// the full structured report (including recommendations and the
    /// per-issue `block_index` / `rule`) calls `conformance_report`
    /// directly.
    ///
    /// Note this is a *superset* of [`crate::encode`]'s fatal checks —
    /// it also rejects §20.a placement / §22 pixel-range / §23.c.viii
    /// transparent-index departures that `encode` itself tolerates — so
    /// `validate_strict().is_ok()` implies `encode` accepts the image,
    /// but not conversely.
    ///
    /// # Example
    ///
    /// ```
    /// use oxideav_gif::AnimationBuilder;
    /// use oxideav_gif::{DisposalMethod, Rgb};
    ///
    /// let palette = vec![Rgb::new(0xFF, 0, 0), Rgb::new(0, 0xFF, 0)];
    /// let img = AnimationBuilder::new(2, 2, palette)
    ///     .add_full_frame(vec![0, 1, 0, 1], 10, DisposalMethod::None)
    ///     .unwrap()
    ///     .build()
    ///     .unwrap();
    /// assert!(img.validate_strict().is_ok());
    /// ```
    pub fn validate_strict(&self) -> crate::error::Result<()> {
        let report = self.conformance_report();
        if !report.has_errors() {
            return Ok(());
        }
        let mut msg = String::new();
        for (i, issue) in report.errors().enumerate() {
            if i > 0 {
                msg.push('\n');
            }
            msg.push_str(&issue.to_string());
        }
        Err(crate::error::Error::InvalidInput(msg))
    }
}

/// §19 / §21: a colour table is a sequence of RGB triplets whose on-disk
/// size is a power of two in `2..=256`. The in-memory `Vec<Rgb>` holds
/// the *used* entries (the encoder pads to the next power of two), so the
/// conformance bound is `1..=256` non-empty entries.
fn check_color_table_size(
    report: &mut ConformanceReport,
    table: &[Rgb],
    block_index: Option<usize>,
    label: &str,
) {
    if table.is_empty() {
        report.push(
            ConformanceRule::ColorTableSize,
            ConformanceSeverity::Error,
            block_index,
            format!("{label}: colour table is empty (must hold at least one entry)"),
        );
    } else if table.len() > 256 {
        report.push(
            ConformanceRule::ColorTableSize,
            ConformanceSeverity::Error,
            block_index,
            format!(
                "{label}: colour table holds {} entries; the §19/§21 maximum is 256",
                table.len()
            ),
        );
    }
}

/// §22 / Appendix F: validate that every pixel index references the
/// `active` palette. Reports the single highest offending index (one
/// issue per frame, not one per pixel) so a buffer full of out-of-range
/// indices does not flood the report.
fn check_index_buffer(
    report: &mut ConformanceReport,
    indices: &[u8],
    active: Option<&[Rgb]>,
    block_index: usize,
) {
    match active {
        Some(palette) if !palette.is_empty() => {
            if let Some(&worst) = indices
                .iter()
                .filter(|&&i| i as usize >= palette.len())
                .max()
            {
                report.push(
                    ConformanceRule::PixelIndexRange,
                    ConformanceSeverity::Error,
                    Some(block_index),
                    format!(
                        "§22: a pixel index references past the {}-entry active palette \
                         (highest offending index {worst})",
                        palette.len()
                    ),
                );
            }
        }
        _ => {
            if !indices.is_empty() {
                report.push(
                    ConformanceRule::PixelIndexRange,
                    ConformanceSeverity::Error,
                    Some(block_index),
                    "§22: image has pixel data but no active colour table (neither a Local \
                     Color Table nor a Global Color Table is present)"
                        .to_string(),
                );
            }
        }
    }
}

/// §23 checks shared by §20 Image and §25 Plain Text blocks: the
/// Transparent Color Index range (§23.c.viii) and the §23.e.ii
/// user-input-without-delay recommendation.
fn check_gce(
    report: &mut ConformanceReport,
    gce: &crate::image::GraphicControl,
    active: Option<&[Rgb]>,
    block_index: usize,
) {
    // §23.c.viii: "The index is present if and only if the Transparency
    // Flag is set." When present it indexes the active palette.
    if let Some(t) = gce.transparent_index {
        if let Some(palette) = active {
            if !palette.is_empty() && (t as usize) >= palette.len() {
                report.push(
                    ConformanceRule::TransparentIndexRange,
                    ConformanceSeverity::Error,
                    Some(block_index),
                    format!(
                        "§23.c.viii: Transparent Color Index {t} references past the {}-entry \
                         active palette",
                        palette.len()
                    ),
                );
            }
        }
    }

    // §23.e.ii: "It is recommended that the encoder not set the User
    // Input Flag without a Delay Time specified." This is the
    // wait-indefinitely corner already surfaced by
    // GraphicControl::waits_for_user_input_indefinitely.
    if gce.waits_for_user_input_indefinitely() {
        report.push(
            ConformanceRule::UserInputWithoutDelay,
            ConformanceSeverity::Recommendation,
            Some(block_index),
            "§23.e.ii: User Input Flag is set with no Delay Time; a purely time-driven \
             renderer would block indefinitely"
                .to_string(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{
        Block, DisposalMethod, Frame, GifImage, GraphicControl, PlainText, Version,
    };

    fn rgb(r: u8, g: u8, b: u8) -> Rgb {
        Rgb::new(r, g, b)
    }

    /// A 2×2 single-frame GIF89a with a 2-entry global palette, every
    /// field in range — the conformance baseline.
    fn clean_image() -> GifImage {
        GifImage {
            version: Version::Gif89a,
            screen_width: 2,
            screen_height: 2,
            color_resolution: 1,
            global_palette_sorted: false,
            background_index: 0,
            pixel_aspect_ratio: 0,
            global_palette: Some(vec![rgb(0, 0, 0), rgb(255, 255, 255)]),
            blocks: vec![Block::Image(Frame {
                left: 0,
                top: 0,
                width: 2,
                height: 2,
                local_palette: None,
                palette_sorted: false,
                interlaced: false,
                indices: vec![0, 1, 1, 0],
                graphic_control: None,
            })],
        }
    }

    #[test]
    fn clean_image_reports_no_issues() {
        let report = clean_image().conformance_report();
        assert!(report.is_clean(), "issues: {:?}", report.issues());
        assert!(!report.has_errors());
        assert_eq!(report.count(ConformanceSeverity::Error), 0);
        assert_eq!(report.count(ConformanceSeverity::Recommendation), 0);
    }

    #[test]
    fn frame_escaping_logical_screen_is_an_error() {
        let mut img = clean_image();
        if let Block::Image(f) = &mut img.blocks[0] {
            f.left = 1; // 1 + 2 = 3 > screen width 2
        }
        let report = img.conformance_report();
        assert!(report.has_errors());
        let issue = report
            .errors()
            .find(|i| i.rule == ConformanceRule::FrameEscapesScreen)
            .expect("escape error present");
        assert_eq!(issue.block_index, Some(0));
        assert!(issue.detail.contains("§20.a"));
    }

    #[test]
    fn pixel_index_past_palette_is_an_error() {
        let mut img = clean_image();
        if let Block::Image(f) = &mut img.blocks[0] {
            f.indices = vec![0, 1, 5, 0]; // 5 >= 2-entry palette
        }
        let report = img.conformance_report();
        let issue = report
            .errors()
            .find(|i| i.rule == ConformanceRule::PixelIndexRange)
            .expect("pixel-range error present");
        assert!(issue.detail.contains("highest offending index 5"));
    }

    #[test]
    fn one_pixel_range_issue_per_frame_not_per_pixel() {
        let mut img = clean_image();
        if let Block::Image(f) = &mut img.blocks[0] {
            f.indices = vec![9, 9, 9, 9]; // all out of range
        }
        let report = img.conformance_report();
        assert_eq!(
            report
                .errors()
                .filter(|i| i.rule == ConformanceRule::PixelIndexRange)
                .count(),
            1,
            "a buffer full of out-of-range indices yields exactly one issue"
        );
    }

    #[test]
    fn background_index_past_gct_is_an_error() {
        let mut img = clean_image();
        img.background_index = 7; // 7 >= 2-entry GCT
        let report = img.conformance_report();
        let issue = report
            .errors()
            .find(|i| i.rule == ConformanceRule::BackgroundIndexRange)
            .expect("background-range error present");
        assert_eq!(issue.block_index, None, "LSD field is stream-level");
        assert!(issue.detail.contains("§18.c.vii"));
    }

    #[test]
    fn background_index_without_gct_is_not_checked() {
        // §18.c.vii: the Background Color Index is only meaningful when a
        // GCT is present. With no GCT a wild index is not an issue.
        let mut img = clean_image();
        img.global_palette = None;
        img.background_index = 200;
        // Give the frame a local palette so the pixels still resolve.
        if let Block::Image(f) = &mut img.blocks[0] {
            f.local_palette = Some(vec![rgb(0, 0, 0), rgb(1, 1, 1)]);
        }
        let report = img.conformance_report();
        assert!(
            !report
                .issues()
                .iter()
                .any(|i| i.rule == ConformanceRule::BackgroundIndexRange),
            "no background-range issue without a GCT"
        );
    }

    #[test]
    fn empty_and_oversize_color_tables_are_errors() {
        let mut img = clean_image();
        img.global_palette = Some(vec![]);
        // An empty palette also strips the pixels' active table.
        let report = img.conformance_report();
        assert!(report
            .errors()
            .any(|i| i.rule == ConformanceRule::ColorTableSize));

        let mut big = clean_image();
        big.global_palette = Some(vec![rgb(0, 0, 0); 257]);
        let report = big.conformance_report();
        let issue = report
            .errors()
            .find(|i| i.rule == ConformanceRule::ColorTableSize)
            .expect("oversize table error");
        assert!(issue.detail.contains("257"));
    }

    #[test]
    fn version_too_low_for_89a_block_is_an_error() {
        let mut img = clean_image();
        img.version = Version::Gif87a;
        // Attach a GCE — that lifts the Required Version to 89a.
        if let Block::Image(f) = &mut img.blocks[0] {
            f.graphic_control = Some(GraphicControl {
                disposal: DisposalMethod::None,
                user_input: false,
                transparent_index: None,
                delay_centis: 10,
            });
        }
        let report = img.conformance_report();
        assert!(report
            .errors()
            .any(|i| i.rule == ConformanceRule::VersionTooLow));
    }

    #[test]
    fn transparent_index_past_palette_is_an_error() {
        let mut img = clean_image();
        if let Block::Image(f) = &mut img.blocks[0] {
            f.graphic_control = Some(GraphicControl {
                disposal: DisposalMethod::None,
                user_input: false,
                transparent_index: Some(9), // 9 >= 2-entry palette
                delay_centis: 10,
            });
        }
        let report = img.conformance_report();
        assert!(report
            .errors()
            .any(|i| i.rule == ConformanceRule::TransparentIndexRange));
    }

    #[test]
    fn user_input_without_delay_is_a_recommendation_only() {
        let mut img = clean_image();
        if let Block::Image(f) = &mut img.blocks[0] {
            f.graphic_control = Some(GraphicControl {
                disposal: DisposalMethod::None,
                user_input: true,
                transparent_index: None,
                delay_centis: 0, // no delay → §23.e.ii recommendation
            });
        }
        let report = img.conformance_report();
        // §23.e.ii is the only departure; it is recommendation-level.
        assert!(!report.has_errors(), "issues: {:?}", report.issues());
        assert_eq!(report.count(ConformanceSeverity::Recommendation), 1);
        assert!(report
            .recommendations()
            .any(|i| i.rule == ConformanceRule::UserInputWithoutDelay));
        assert!(!report.is_clean());
    }

    #[test]
    fn plain_text_without_palette_is_an_error() {
        let mut img = clean_image();
        img.global_palette = None;
        // Replace the image with a plain-text block so there is no
        // active table anywhere.
        img.blocks = vec![Block::PlainText {
            params: PlainText {
                left: 0,
                top: 0,
                width: 8,
                height: 8,
                cell_width: 8,
                cell_height: 8,
                fg_color_index: 1,
                bg_color_index: 0,
                text: b"hi".to_vec(),
            },
            graphic_control: None,
        }];
        let report = img.conformance_report();
        assert!(report
            .errors()
            .any(|i| i.rule == ConformanceRule::PlainTextIndexRange));
    }

    #[test]
    fn plain_text_fg_index_past_palette_is_an_error() {
        let mut img = clean_image();
        img.blocks = vec![Block::PlainText {
            params: PlainText {
                left: 0,
                top: 0,
                width: 8,
                height: 8,
                cell_width: 8,
                cell_height: 8,
                fg_color_index: 9, // 9 >= 2-entry GCT
                bg_color_index: 0,
                text: b"hi".to_vec(),
            },
            graphic_control: None,
        }];
        let report = img.conformance_report();
        assert!(report
            .errors()
            .any(|i| i.rule == ConformanceRule::PlainTextIndexRange));
    }

    #[test]
    fn indices_length_mismatch_is_an_error() {
        let mut img = clean_image();
        if let Block::Image(f) = &mut img.blocks[0] {
            f.indices = vec![0, 1]; // 2 != 2×2
        }
        let report = img.conformance_report();
        assert!(report
            .errors()
            .any(|i| i.rule == ConformanceRule::FrameIndicesLength));
    }

    #[test]
    fn color_resolution_out_of_range_is_an_error() {
        let mut img = clean_image();
        img.color_resolution = 8; // 3-bit field max is 7
        let report = img.conformance_report();
        assert!(report
            .errors()
            .any(|i| i.rule == ConformanceRule::ColorResolutionRange));
    }

    #[test]
    fn special_purpose_blocks_carry_no_conformance_load() {
        let mut img = clean_image();
        img.blocks.push(Block::Comment(b"hello".to_vec()));
        img.blocks
            .push(Block::Application(crate::image::Application {
                identifier: *b"NETSCAPE",
                auth_code: *b"2.0",
                data: vec![],
            }));
        let report = img.conformance_report();
        assert!(report.is_clean(), "issues: {:?}", report.issues());
    }

    #[test]
    fn clean_report_displays_as_conformant() {
        let report = clean_image().conformance_report();
        assert_eq!(report.to_string(), "conformant: no issues");
    }

    #[test]
    fn issue_display_includes_severity_and_block() {
        let mut img = clean_image();
        if let Block::Image(f) = &mut img.blocks[0] {
            f.left = 1; // §20.a frame escape on block 0
        }
        let report = img.conformance_report();
        let rendered = report.to_string();
        assert!(rendered.starts_with("error [block 0]: §20.a"), "{rendered}");

        // A stream-level issue renders without a block tag.
        let mut img2 = clean_image();
        img2.color_resolution = 8;
        let report2 = img2.conformance_report();
        let issue = &report2.issues()[0];
        assert!(issue.to_string().starts_with("error: §18.c.iv"));
    }

    #[test]
    fn validate_strict_ok_on_clean_image() {
        assert!(clean_image().validate_strict().is_ok());
    }

    #[test]
    fn validate_strict_passes_recommendation_only_image() {
        // §23.e.ii is the only departure; validate_strict tolerates
        // recommendation-level issues.
        let mut img = clean_image();
        if let Block::Image(f) = &mut img.blocks[0] {
            f.graphic_control = Some(GraphicControl {
                disposal: DisposalMethod::None,
                user_input: true,
                transparent_index: None,
                delay_centis: 0,
            });
        }
        let report = img.conformance_report();
        assert_eq!(report.count(ConformanceSeverity::Recommendation), 1);
        assert!(!report.has_errors());
        assert!(
            img.validate_strict().is_ok(),
            "validate_strict must tolerate recommendation-only reports"
        );
    }

    #[test]
    fn validate_strict_err_collects_every_error() {
        let mut img = clean_image();
        // Two independent errors: bad colour resolution + frame escape.
        img.color_resolution = 8;
        if let Block::Image(f) = &mut img.blocks[0] {
            f.left = 5;
        }
        let err = img.validate_strict().expect_err("two errors present");
        let crate::error::Error::InvalidInput(msg) = err else {
            panic!("expected InvalidInput");
        };
        assert!(msg.contains("§18.c.iv"));
        assert!(msg.contains("§20.a"));
        assert_eq!(msg.lines().count(), 2, "one line per error: {msg}");
        // Recommendations are excluded from the strict-gate message.
        assert!(!msg.contains("§23.e.ii"));
    }
}
