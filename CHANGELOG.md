# Changelog

## [Unreleased]

### Added

- `GifImage::frames_with_graphic_control()` — §23-side companion to
  the existing `frames_with_palette()` accessor. Yields each §20 Image
  Descriptor block paired with its attached §23 Graphic Control
  Extension (`Option<GraphicControl>`; `None` when no GCE preceded the
  image per §23.a "at most one Graphic Control Extension may precede
  a graphic rendering block"), in source order. Lets callers walk
  "every image and the GCE that controls it" without re-deriving the
  §23 → §20 attachment from `GifImage::blocks` themselves. Mirrors the
  §20-only shape of `frames_with_palette()` so the two accessors
  compose naturally; §25 Plain Text blocks remain reachable via the
  shared timing / rendering-flag spine (`frame_delays()` /
  `has_transparency()` / `requires_user_input()`). Six new unit tests
  pin the no-GCE / attached-GCE / per-frame-independence / non-image-
  block-skipping / `*const Frame`-handle-matches-`frames()` /
  delay-matches-`frame_delays()` semantics. Total unit tests 157 →
  163. Round 218.
- `tests/compose_disposal.rs` per-frame compositor edge-case sweep —
  the prior file covered each §23.c.iv disposal value (0–3) with one
  canonical non-overlapping fixture (4 tests). This sweep adds 7
  targeted scenarios for behaviour §23 leaves implicit:
  transparent-index (§23.c.viii) show-through, restore-to-background
  with no §18 Global Color Table (falls back to fully transparent
  black per §18.c.iii), restore-to-background only clearing the
  disposing frame's own rect rather than the entire prior-frame
  footprint, restore-to-previous capturing show-through state in its
  snapshot (not a pristine pre-everything canvas), nested
  RestorePrevious chains where each frame's pre-render snapshot is
  independent, restore-to-background on a full-screen frame wiping
  the whole canvas, and the implicit
  `ComposedFrame::delay_centis`-reports-the-disposing-frame's-own-delay
  contract (defaulting to 0 when no §23 GCE is attached). Compositor
  test count 4 → 11. Round 213.
- `GifImage::color_resolution_bits()` / `GifImage::original_palette_color_count()`
  / `GifImage::frame_count()` — three direct accessors over Logical Screen
  Descriptor §18 fields. `color_resolution_bits()` adds 1 to the §18.c.iv raw
  3-bit field (per §18.c.iv's "Number of bits per primary color available to
  the original image, minus 1") returning `1..=8`;
  `original_palette_color_count()` is the derived `2^(3 × bits)` source-palette
  colour count (`8..=16_777_216`), letting a renderer pick a display mode
  against the source palette's *richness*, not the per-frame palette
  truncation. `frame_count()` returns the number of §20 Image blocks (§24
  Comment / §25 Plain Text / §26 Application Extensions excluded) for the
  common "how many images" call site without forcing the caller to count the
  `frames()` iterator. Five new unit tests pin the bits-per-primary range,
  high-bit masking, the eight-row §18.c.iv colour-count table, image-block-only
  counting against a mixed block list, and the metadata-only zero case. Total
  unit tests 152 → 157. Round 207.
- `fuzz/fuzz_targets/plain_text.rs` dedicated §25 Plain Text Extension
  `cargo-fuzz` harness — synthesises `Block::PlainText` blocks (with
  optional §23 GCE attachment) directly via the in-process API,
  drives `encode` → `decode` (strict + lenient + cover-frame) →
  `compose` → `Playback` on the result, and asserts panic-freedom
  across §25.c.viii/ix `cell_width = 0`, §25.c.x/xi out-of-palette
  fg/bg-index clamping, multi-sub-block §15 text payload splitting,
  §25.e font fallback on non-ASCII bytes, and the §23.f.i snapshot/
  revert path when a `RestorePrevious` GCE is attached to a Plain
  Text block. Verified clean over 200 000 iterations seeded from
  three §25-walking seed inputs (round 200).
- `benches/lzw.rs` Criterion harness exercising the Appendix F LZW
  codec pair in isolation across four sizes (16×16 / 256×256 /
  1024×1024 / 100×(64×64)); baseline numbers + matrix recorded in
  `BENCHMARKS.md` (round 194).

## [0.0.11](https://github.com/OxideAV/oxideav-gif/compare/v0.0.10...v0.0.11) - 2026-05-29

### Other

- GifImage::has_transparency / requires_user_input §23 flag queries
- GifImage::frames_with_palette §21 active-table iterator
- GifImage::background_color accessor for §18.c.vii resolution
- tracked spec-derived seed corpus for the daily cargo-fuzz harnesses
- fix divide-by-zero in encode harness background-index reduction
- pin reproducible builds + verified zero-crash baseline (round 134)
- criterion harnesses + encoder fuzz target (round 129)
- fuzz/decode end-to-end harness + LZW pre-alloc DoS fix
- AnimationBuilder fluent encode-side animation assembler
- animation-timing accessors on GifImage
- Add §18.c.viii Pixel Aspect Ratio accessors on GifImage
- Round 89: §24 Comment Extension accessors on GifImage
- Enforce §7 Required Version on encode + add version-upgrade helper
- Add Plain Text rendering, ANIMEXTS1.0 view, and lenient decoder
- Add EXIF, GCT/LCT optimisation, cover-frame fast-path, LZW end-of-input fix
- Add lazy playback iterator with NETSCAPE2.0 loop-count semantics
- Add structured parsing for ecosystem GIF Application Extensions
- restore oxideav_core::register! macro (workspace registry contract)
- Add multi-frame compositor implementing GIF89a §23 disposal-method semantics
- Initial implementation of GIF87a / GIF89a codec
- Initial commit (clean-room rebuild — round 2)

### Added
- `GifImage::has_transparency()` / `GifImage::requires_user_input()` —
  stream-level boolean queries over the §23 Graphic Control Extension
  rendering flags. `has_transparency()` is true iff some
  graphic-rendering block (§20 Image **or** §25 Plain Text) carries a
  GCE with the §23.c.vi Transparency Flag set (a §23.c.viii Transparent
  Index given) — a renderer deciding whether to allocate an alpha
  channel can gate on this rather than walking every frame's
  `transparent_index`. `requires_user_input()` is true iff some GCE sets
  the §23.c.v User Input Flag, telling an interactive viewer whether it
  needs an input-aware playback loop at all. Both return `false` for a
  GCE-less still (every GIF87a) and for streams whose GCEs leave the
  flag clear. The two accessors plus `frame_delays()` now share a
  private `graphic_rendering_controls()` iterator (the §23.d
  "graphic-rendering block" spine, skipping §24 Comment / §26
  Application blocks that carry no GCE). Two new unit tests pin the
  no-GCE / flag-clear / flag-set / Plain-Text-block cases; total unit
  tests 150 → 152. Round 188.
- `GifImage::frames_with_palette()` — iterator that yields each
  image-bearing block paired with the colour table the decoder
  should render it against, applying the §21.a precedence rule
  ("If present, this color table temporarily becomes the active
  color table and the following image should be processed using
  it"): a frame's Local Color Table supersedes the §18 Global
  Color Table; when the LCT flag is clear the GCT applies; when
  neither is present the second tuple element is `None` (the §13
  / §21 fallback "a Data Stream which does not contain either a
  Global Color Table or a Local Color Table"). The yielded slice
  borrows from `self` so callers walking frames + palette together
  do not need to clone the palette or hand-roll the precedence
  lookup. Five new unit tests pin GCT-fallback / LCT-precedence /
  no-table / non-Image-block-skipping / `*const Frame`-handle-
  matches-`frames()` semantics; total unit tests 145 → 150. Round
  181.
- `GifImage::background_color` / `GifImage::background_color_rgba` —
  public accessors that resolve the §18.c.vii Background Color Index
  against the §18.c.ii Global Color Table. `background_color` returns
  `Option<Rgb>` (`None` when no Global Color Table is attached or when
  `background_index` falls past the end of the GCT — the conservative
  reading of §18.c.vii's "should be ignored" clause); the `_rgba`
  variant is the alpha-extended `[u8; 4]` form (`[r, g, b, 0xFF]` or
  fully-transparent black) used by the §23 dispose-to-background
  canvas clear. Consolidates the two existing private resolvers
  (`compose::compose`'s inline `match` and `playback::compute_background_rgba`)
  into one §18.c.vii implementation that the eager `compose` path and
  the lazy `Playback` iterator both call through. Round 175.
- `fuzz/seed_corpus/{decode,decode_panic_free,decode_lenient_panic_free}/` —
  tracked, audit-grade seed inputs for the daily cargo-fuzz harnesses
  (round 153). Five payloads per target: the 1×1 GIF87a minimal and
  2×2 GIF89a + GCE fixtures (byte-for-byte copies of the spec walks
  in `tests/spec_fixtures.rs`), plus three malformed inputs that
  hit the spec's classic problem areas — truncated §27 trailer
  (EOF on trailer state machine), §22.c.i LZW min-code-size = 12
  (illegal per Appendix F's 12-bit code-width clamp), and a §26
  Application Extension whose sub-block length byte claims 0x42
  follow-on bytes when only 3 exist before the §16 terminator. The
  `fuzz/corpus/` flywheel stays gitignored; the seed corpus is a
  separate tracked tree under `fuzz/seed_corpus/<target>/` so a fresh
  clone can `cp -n fuzz/seed_corpus/<target>/* fuzz/corpus/<target>/`
  before invoking `cargo fuzz run`. Verified locally: each well-formed
  seed decodes via both `decode()` and `decode_lenient()`; each
  malformed seed surfaces `Err(_)` on `decode()` (no panic) and a
  recovered `Ok(_)` on `decode_lenient()` (no panic). See
  `fuzz/seed_corpus/README.md` for the seed inventory.
- `tools/seedgen.py` — reproducible Python generator for the
  `fuzz/seed_corpus/` payloads. Pure-Python (no external deps), no
  GIF library invoked; each seed is a literal walk of the GIF89a
  spec sections it exercises. Idempotent — re-running on a populated
  tree only writes files whose SHA-1 isn't already on disk.
- `benches/decode.rs`, `benches/encode.rs`, `benches/roundtrip.rs` —
  Criterion bench harnesses for the decode / encode / build → encode →
  decode hot paths. Mirrors the per-crate shape that `oxideav-cinepak`,
  `oxideav-tta`, `oxideav-magicyuv`, `oxideav-h264`, `oxideav-pixfmt`
  already track so future LZW / sub-block / disposal-state-machine
  optimisation rounds can A/B against the round-129 baseline. 14 bench
  scenarios across the three files; each synthesises its inputs on the
  fly via `AnimationBuilder` + `encode`, no committed fixture files.
  Run with `cargo bench -p oxideav-gif --bench <name>`.
- `fuzz/fuzz_targets/encode.rs` — encode-side end-to-end cargo-fuzz
  harness, sibling to the decode-side `fuzz/fuzz_targets/decode.rs`
  from r126. Builds a `GifImage` out of arbitrary fuzz bytes via
  `AnimationBuilder` (rect placement, palette size, frame count, loop
  behaviour all derived from the input), then runs
  `encode` → `decode` → `decode_lenient` → `decode_first_frame` →
  `compose` → `Playback::frames` / `looping_frames` on the result.
  Reaches builder-only encoder configurations (mismatched-palette-size
  frames, sub-screen frames at non-zero origins, ANIMEXTS / NETSCAPE
  loop blocks, multi-frame disposal sequences) that the
  decoder-output-only fuzzer can't construct. Bounds canvas at
  `screen_w × screen_h ≤ 1 Mpx` (256 × 256 cap) and frame count at
  `MAX_FUZZ_FRAMES = 16` to keep fuzzer RSS bounded.

### Fixed
- `fuzz/fuzz_targets/encode.rs` background-index reduction no longer
  divides by zero. The cap `palette_size.min(256) as u8` wrapped the
  maximum legal palette size (256) to `0u8`, so `data[5] % 0` panicked
  whenever the fuzzer picked `palette_size == 256` — a harness bug, not
  a `src/` bug, surfaced by the daily scheduled `Fuzz` run. The modulus
  is now computed in `u16` (`palette_size.min(256)`, always `1..=256`)
  before the index is narrowed back to `u8`, keeping the result a valid
  `< palette_size` background index. 60 s re-run of all five targets is
  crash-free.
- LZW decoder no longer pre-allocates `width × height` bytes from the
  §20.c Image Descriptor before reading any compressed bytes. A hostile
  65535 × 65535 declaration used to trick `Vec::with_capacity` into
  reserving ~4 GiB and aborting the process — a trivial decode-side DoS
  reachable from any GIF parser. The up-front reservation is now
  clamped to `compressed_bytes_len * MAX_TABLE_SIZE` (a generous-but-
  bounded ceiling on legitimate LZW expansion), regression-tested in
  `lzw::tests::decode_caps_upfront_allocation_by_input_length`.

### Added
- `fuzz/fuzz_targets/decode.rs` — end-to-end decode-side cargo-fuzz
  harness that exercises every spec-classic problem area (LZW code
  table growth, §23 Graphic Control / §26 Application Extension
  nesting, §15 sub-block chains, §23.c.iv frame-disposal arithmetic,
  §19/§21/§23.c.viii palette + transparency handling, NETSCAPE2.0
  loop-count) through one input. Caps downstream work at
  `screen_width × screen_height ≤ 1 Mpx` and `MAX_PLAYBACK_FRAMES = 64`
  to keep fuzzer RSS bounded; surfaced the LZW decode-side OOM above
  on its first multi-minute run.
- `builder::AnimationBuilder` — fluent encode-side assembler for an
  animated `GifImage`. `AnimationBuilder::new(width, height, palette)`
  targets a §18 Logical Screen sharing one Global Color Table;
  `add_full_frame(indices, delay_centis, disposal)` appends a
  full-screen frame and `add_placed_frame(left, top, w, h, …)` a
  sub-rectangle, each attaching a §23 Graphic Control Extension that
  carries the §23.c.vii Delay Time and §23.c.iv Disposal Method.
  `loop_forever()` / `loop_count(n)` / `play_once()` select the
  looping behaviour, emitting a NETSCAPE2.0 *Looping* Application
  Extension (§26) ahead of the first frame for the non-"play once"
  cases per the de-facto convention in
  `docs/image/gif/netscape2.0-loop-extension.md`; `play_once` emits no
  block. `background_index(i)` threads the §18.c.vii Background Color
  Index. `build()` validates placement (frame rectangles must fit the
  Logical Screen, matching the compositor's bounds check), index counts
  (`len == width × height`), palette-index range, the §19 1..=256
  palette-size limit, and a non-empty frame list, then returns a
  `Version::Gif89a` `GifImage` ready for `encode`. The result's
  `frame_delays()` / `single_pass_duration()` / `total_play_duration()`
  / `loop_count()` report back exactly what was set — the builder is
  the encode-side counterpart to the timeline accessors — and a build →
  `encode` → `decode` round-trip is value-stable.
- Animation-timing accessors on `GifImage`. `frame_delays()` is the
  source-order iterator over each graphic-rendering block's §23.c.vii
  Delay Time as a `core::time::Duration` (1 centi-second = 10 ms; a
  block with no Graphic Control Extension, or a Delay Time of 0,
  contributes `Duration::ZERO`); it covers both §20 Images and §25
  Plain Text blocks, matching the playback iterator's per-frame delay
  exactly. `is_animated()` returns `true` only when the stream carries
  more than one graphic-rendering block (a single-frame still is not
  "animated" even with a NETSCAPE loop block). `single_pass_duration()`
  sums one pass through the timeline; `total_play_duration()` multiplies
  that by the NETSCAPE2.0 / ANIMEXTS1.0 pass count (no loop block →
  1 pass, `Some(n)` → `n + 1` passes per the documented de-facto
  convention, `Some(0)` → loops forever → returns `None`), with
  saturating arithmetic guarding the unreachable `Duration` overflow.
- §18.c.viii Pixel Aspect Ratio accessors on `GifImage`. The decoder
  already stored the raw byte (`pixel_aspect_ratio`); these helpers
  apply the §18.c.viii formula. `pixel_aspect_ratio_value()` decodes
  the raw byte into the pixel width ÷ height ratio via
  `Aspect Ratio = (Pixel Aspect Ratio + 15) / 64`, returning `None`
  for the raw value `0` ("no aspect ratio information is given").
  `raw_pixel_aspect_ratio_for(ratio)` is the inverse, mapping a
  desired ratio back to the raw byte (`round(ratio × 64) − 15`) and
  returning `None` for ratios outside the spec's representable span
  (the widest pixel ~4:1 at raw 255 down to the tallest 1:4 at raw 1;
  square pixels are raw 49). The two are exact inverses across the
  whole `1..=255` raw range.
- §24 Comment Extension accessors on `GifImage`. `comments()` is the
  source-order iterator over every `Block::Comment` payload (mirrors
  the existing `application_extensions()` accessor);
  `concatenated_comment()` returns every payload joined with a single
  LF as a single buffer, returning `None` when no Comment Extension is
  present so callers can distinguish "no comments" from "one empty
  comment". `comments_are_7bit_ascii()` and
  `comments_in_recommended_position()` surface the §24.e.i / §24.e.ii
  *recommendations* (ASCII-only payload; leading-or-trailing position
  relative to graphic-rendering blocks) as boolean queries — the
  encoder itself does not enforce a recommendation, so consumers that
  want to refuse non-conforming streams gate on these checks
  themselves.
- §7 "Required Version" enforcement on encode. The encoder now
  refuses to emit a `GIF87a`-labeled stream that contains any
  89a-only block (§23 Graphic Control, §24 Comment, §25 Plain Text,
  §26 Application Extension) with an `InvalidInput` naming the
  offending block kind. Two new accessors on `GifImage`:
  `required_version()` (the earliest version that covers every
  contained block, per the per-block "Required Version" table) and
  `upgrade_version_if_needed()` (bumps the declared version to the
  required minimum in one call). The upgrade helper never
  *down*grades — a caller's explicit choice of `Gif89a` for an
  87a-compatible payload is preserved. Also exposes
  `Block::required_version()` for callers that want per-block
  granularity, plus `Ord` / `PartialOrd` impls on `Version` so
  `version.max(required)` works in arithmetic contexts.
- `font` module — clean-room minimal 8×8 stylised bitmap font for
  ASCII 0x20..=0x7E. `font::glyph` returns the 8-byte bitmap (MSB =
  leftmost pixel) for the given code point and falls back to the
  all-zero space glyph for anything outside the supported range,
  matching the §25.e fallback rule.
- §25 Plain Text Extension glyph rendering in `compose` and
  `playback`. Plain Text blocks now participate in the §23
  disposal-method state machine and render against the active Global
  Color Table using the new `font` module; previously they were
  no-ops on the canvas. Blocks without an active GCT remain no-ops
  per §25.a ("This block requires a Global Color Table to be
  available").
- `app_ext::AnimextsLoopControl` + the corresponding
  `ANIMEXTS_IDENTIFIER` / `ANIMEXTS_AUTH_CODE` constants — typed view
  over the legacy ANIMEXTS1.0 Application Extension, which reuses the
  NETSCAPE2.0 *Looping* sub-block layout under a different
  identifier+auth. `GifImage::loop_count()` now falls back to
  ANIMEXTS1.0 when NETSCAPE2.0 is absent, so consumers don't need to
  pick between the two.
- `decode_lenient` error-recovery decoder. Skips malformed sub-blocks,
  corrupt extensions, and rejected image frames by scanning forward
  to the next §20 Image Separator / §27 Trailer instead of returning
  an error. The strict `decode` entry point stays the default
  byte-stable round-trip behaviour; the new entry point is opt-in for
  viewers / thumbnailers / recovery tools.
- `fuzz_targets/decode_lenient_panic_free.rs` — companion fuzz target
  asserting `decode_lenient` returns on arbitrary input without
  panicking. 200k iterations clean locally.
- `app_ext::ExifMetadata` + `GifImage::exif()` — typed view over the
  `Exif    ` Application Extension (Exif 2.3 §4.7.2). Mirrors the
  XMP / ICC pass-through pattern: identifier match yields the raw
  TIFF EXIF blob; the 3-byte authentication code is preserved on
  the typed struct so a decode → re-encode round-trip is byte-stable
  even when the producer used a non-default auth code.
- `GifImage::optimize_color_tables()` — encoder helper that hoists a
  shared per-frame Local Color Table into the §18 Global Color
  Table when every image frame carries the same palette. Saves
  `3 × 2^(size_bits + 1)` bytes per frame. Refuses to hoist when
  palettes differ (no-op, stream byte-identical to baseline).
- `decode_first_frame()` cover-frame fast-path that short-circuits
  at the first image-bearing block and discards every trailing
  block. Useful for thumbnail-style consumers that don't need the
  full animation timeline.

### Fixed
- LZW codec width-bump symmetry at end-of-input. The encoder now
  performs a phantom dictionary extension on its final-prefix
  emission, mirroring the decoder's own entry add. Without this,
  rasters whose penultimate in-loop assignment lands exactly on
  `2^W − 2` (e.g., a 16-pixel monochrome image at
  `min_code_size = 3`) wrote EOI at the old width while the decoder
  read at the new one. The fix is internal — encoded byte streams
  for the unaffected case are byte-identical to before.

- `playback` module — lazy `Playback` / `FrameIter` /
  `LoopingFrameIter` iterators that walk the §23 disposal-method
  state machine one frame at a time. Yields `PlaybackFrame`
  (`RgbaCanvas` + `Duration`) so a downstream player can `sleep` on
  the §23.c.vii delay directly. `LoopingFrameIter` honours the
  NETSCAPE2.0 *Looping* sub-block: no extension → 1 pass,
  `loop_count = 0` → infinite, `loop_count = N` → `N + 1` total
  passes per the de-facto convention. Works with the registry feature
  on or off.
- `app_ext` module — typed parsers + constructors for the three
  ecosystem-defined Application Extensions that ride on top of GIF89a
  §26: NETSCAPE2.0 *Looping* + *Buffering* sub-blocks
  (`LoopControl`), the Adobe XMP packet (`XmpPacket`), and the ICC
  colour profile (`IccProfile`). These layer on top of the raw
  `Block::Application` representation — the raw block stays in
  `GifImage::blocks` for byte-stable round-trip.
- `GifImage::loop_count()` / `netscape_buffer_hint()` /
  `xmp_packet()` / `icc_profile()` / `application_extensions()`
  convenience accessors.
- Initial implementation per GIF87a / GIF89a specs.
- `compose` module — multi-frame compositor that walks every image
  block and applies the GIF89a §23 disposal-method state machine
  (None / Keep / RestoreBackground / RestorePrevious), honouring the
  §23.c.viii transparent index and the §18 Logical Screen background
  colour. Returns a `Vec<ComposedFrame>` of `RgbaCanvas` + per-frame
  delay.
- `registry` module + default-on `registry` Cargo feature wiring the
  GIF codec into `oxideav-core`'s `RuntimeContext` via the
  `oxideav_core::register!` macro. Exposes `GifDecoder` / `GifEncoder`
  trait impls + `register` / `register_codecs` / `register_containers`
  entry points. The macro-generated `__oxideav_entry` is re-exported at
  the crate root so `oxideav-meta`'s `register_all` can dispatch to it
  (this is the "workspace registry contract" — see workspace memory
  `project_register_macro_dispatch_contract`). Standalone consumers can
  opt out with `default-features = false` and the `oxideav-core` dep
  drops out of the build entirely.
