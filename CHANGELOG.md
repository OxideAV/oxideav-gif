# Changelog

## [Unreleased]

### Fixed
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
