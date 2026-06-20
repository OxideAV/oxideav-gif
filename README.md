# oxideav-gif

Pure-Rust decoder and encoder for the GIF87a and GIF89a image formats.

## Status

Implements every block type defined by the CompuServe specifications:

- Header (§17), Logical Screen Descriptor (§18), Trailer (§27).
  `GifImage::pixel_aspect_ratio_value()` decodes the §18.c.viii Pixel
  Aspect Ratio byte into the pixel width÷height ratio via
  `(raw + 15) / 64` (raw 0 → `None`, "no aspect ratio information"),
  and `GifImage::raw_pixel_aspect_ratio_for(ratio)` is its exact
  inverse (`None` outside the spec's 1:4 .. ~4:1 representable span;
  square pixels = raw 49). `GifImage::background_color()` resolves
  the §18.c.vii Background Color Index against the §18.c.ii Global
  Color Table (`None` when the GCT flag was zero, when no palette is
  attached, or when the index is past the end of the GCT — the
  conservative reading of §18.c.vii's "should be ignored" clause);
  `GifImage::background_color_rgba()` is the alpha-extended form used
  by the §23 dispose-to-background canvas clear in `compose` /
  `Playback`. `GifImage::color_resolution_bits()` decodes the §18.c.iv
  Color Resolution byte into the bits-per-primary-colour of the *source*
  palette (raw + 1, range `1..=8`); `original_palette_color_count()` is
  the derived `2^(3 × bits)` colour count (`8..=16_777_216`), letting a
  renderer pick a display mode against the source's richness rather
  than the per-frame palette truncation. `GifImage::frame_count()`
  counts §20 Image blocks (no Plain Text, no Comment, no Application)
  for callers that need a single-number "how many images" without
  walking the iterator themselves.
- Global / Local Color Tables (§19, §21).
  `GifImage::frames_with_palette()` yields each §20 image-bearing
  block paired with the colour table the decoder should render it
  against — Local Color Table when present (§21.a: "this color
  table temporarily becomes the active color table"), Global
  Color Table when the LCT flag is clear, `None` when neither
  table is attached (§13 / §21 fallback). The yielded palette
  slice borrows from the `GifImage` so frame-walking consumers do
  not need to clone the palette or hand-roll the precedence
  lookup. `GifImage::frames_with_graphic_control()` is the
  §23-side companion: each §20 Image block paired with its
  attached §23 Graphic Control Extension (`None` when no GCE
  preceded it per §23.a "at most one Graphic Control Extension
  may precede a graphic rendering block"), preserving source
  order so callers walking "every image plus the GCE that
  controls it" don't have to re-derive the §23 → §20 attachment
  from the raw block list. §18.c.v / §20.c.viii Sort Flag
  queries surface the spec's "ordered by decreasing importance"
  guarantee that palette-display-constrained renderers can use
  for initial-segment truncation:
  `GifImage::has_sorted_global_palette()` is the GCT-level query
  (`true` only when a §18 GCT is present *and* its §18.c.v Sort
  Flag is set); `GifImage::frames_with_sorted_palette()` extends
  `frames_with_palette` with the active-table Sort Flag bit (LCT
  Sort Flag when an LCT applies per §21.a, GCT Sort Flag
  otherwise, `false` when neither table is attached);
  `GifImage::all_frames_palettes_sorted()` reports whether the
  whole stream's active palettes are sorted in one query so a
  pipeline can gate initial-segment truncation without walking
  per-frame.
- Image Descriptor + Table-Based Image Data (§20, §22). §20.c.vii
  Interlace Flag stream-level roll-up:
  `GifImage::interlaced_frame_count()` counts §20 Image blocks whose
  Interlace Flag is set, `has_interlaced_frames()` is `true` as soon
  as one image carries it, and `all_frames_interlaced()` is the
  every-frame query (vacuously `true` for zero-frame streams, matching
  `all_frames_palettes_sorted()`'s shape). Lets a progressive-display
  renderer gate on a single query rather than walking every
  `Frame::interlaced`; the decoded raster is always presented
  de-interlaced regardless, so this only affects the policy decision
  on whether to enable an Appendix-E-aware progressive path. §20.c.ix
  Size of Local Color Table surface:
  `Frame::local_color_table_size_field()` returns the 3-bit encoded
  field value (`0..=7`, smallest `N` such that `2^(N+1)` is ≥ the LCT
  entry count) per attached LCT, or `None` when §20.c.vi is clear (and
  the field is undefined); `Frame::local_color_table_entry_count()` is
  the `2^(N+1)` on-disk entry-count companion (range `2..=256`).
  Stream-level rollups:
  `GifImage::frames_with_local_color_table_size()` and
  `frames_with_local_color_table_entry_count()` pair every §20 Image
  with its §20.c.ix field / on-disk entry count in source order;
  `max_local_color_table_size_field()` is the largest §20.c.ix across
  every LCT-carrying §20 Image (`None` when no §20 frame attaches an
  LCT), so a decoder allocating a reusable scratch LCT buffer can size
  it once up front rather than re-allocating per-frame. §20.a / §25.a
  "must fit within the boundaries of the Logical Screen" validation:
  `GifImage::all_blocks_fit_screen()` reports whether every §20 Image
  and §25 Plain Text grid's placement rectangle stays inside the §18
  Logical Screen (right edge `left + width` ≤ Logical Screen Width and
  bottom edge `top + height` ≤ Logical Screen Height; edge sums widen
  to `u32` so a placement at the 65 535 coordinate ceiling cannot
  wrap), and `out_of_bounds_block_count()` is the complementary count
  of escaping graphic-rendering blocks. `compose()` / `Playback`
  already reject an out-of-bounds placement with an error because the
  spec's boundary clause is a hard requirement with no defined
  clipping fall-back; these accessors surface the same check as a
  query so a consumer can validate a decoded or freshly-built stream
  before attempting to render. Streams with no graphic-rendering block
  conform vacuously, matching the shape of `all_frames_interlaced()`.
- §12 "Blocks, Extensions and Scope" classification. `Block::class()`
  returns the §12 `BlockClass` for a block — `GraphicRendering` for a
  §20 Image or §25 Plain Text (labels `0x00..=0x7F` excl. §27 Trailer),
  `SpecialPurpose` for a §24 Comment or §26 Application (labels
  `0xFA..=0xFF`), with the `Control` variant modelled for completeness
  (Header / §18 LSD / §23 GCE / §27 Trailer are structural fields or
  attached, never free-standing `Block`s). `Block::is_graphic_rendering()`
  / `is_special_purpose()` are the boolean forms;
  `GifImage::graphic_rendering_block_count()` (§20 + §25, unlike
  `frame_count()`'s §20-only count) and `special_purpose_block_count()`
  (§24 + §26) are the stream-level rollups — every block partitions into
  exactly one of the two, since no §12 Control block is ever a list
  entry. §11 "About Color Tables" palette-loader recognition:
  `GifImage::is_palette_loader_stream()` is `true` for the §11
  table-install shape (a §18 Global Color Table present with **no**
  graphic-rendering block — §12-transparent Comment / Application blocks
  do not disqualify it), the "Header, Logical Screen Descriptor, a
  Global Color Table and the GIF Trailer" stream §11 describes for
  loading a decoder with a palette ahead of subsequent tableless Data
  Streams. The strict `decode` entry point rejects an image-less stream,
  so this arises from `decode_lenient` or a freshly-built `GifImage`.
- Variable-Length-Code LZW compression (Appendix F). The codec pair
  ships in two flavours: the stateless `lzw::encode` / `lzw::decode`
  free functions for one-shot calls, and `lzw::LzwEncoder` which
  reuses its `(prefix, next_byte) → code` dictionary across multiple
  `encode_frame` calls — the ~2 MiB zero-init lands once at
  construction, each subsequent frame walks a `touched_keys` log
  (≤ 4094 entries) rather than memsetting the whole table. Output
  is byte-identical to the free function; reuse cuts the
  100×(64×64) animation-encoder microbench by ~44 %
  (see `BENCHMARKS.md`). Two table-full strategies are offered, both
  permitted by Appendix F's cover sheet: `lzw::encode` follows the
  *deferred clear* rule (freeze the 4096-entry dictionary and keep
  emitting 12-bit codes until end-of-image), while
  `lzw::encode_with_clear_on_full` emits a Clear code and rebuilds the
  dictionary the instant it fills, so the table re-adapts to later
  content instead of coding it against a frozen prefix set. Both decode
  to identical pixels (`decode` honours the §F.1 mid-stream Clear) and
  emit byte-identical output for rasters that never fill the table; on a
  large regime-changing raster the re-adapting path is ~3.7 % smaller
  in the in-tree property test (166 077 → 159 854 B). The top-level
  encoder exposes the choice: `encode_with_options(image, EncodeOptions
  { lzw_strategy })` drives every §20 Image frame's LZW through the
  selected `LzwStrategy` (`DeferredClear` default / `ClearOnFull`), while
  the bare `encode(image)` keeps the byte-stable deferred-clear default.
  Both decode to identical pixels and emit byte-identical streams for any
  frame whose dictionary never reaches the 4096-entry ceiling.
- Four-pass interlace transform (Appendix E)
- Graphic Control Extension (§23) — disposal method, user-input flag,
  transparent index, delay time. §23.c.iv Disposal Method stream-level
  roll-up: `GifImage::frame_disposals()` yields the GCE Disposal Method
  per graphic-rendering block in source order (no GCE attached →
  `DisposalMethod::None` per §23.c.iv value `0` "No disposal specified"),
  `uses_disposal(method)` / `all_frames_use_disposal(method)` are the
  any-block / every-block queries (vacuously `true` for zero-frame
  streams, matching the surrounding rollup shapes), and
  `requires_canvas_snapshot()` reports whether any block selects
  `RestorePrevious` so a renderer can skip pre-allocating the §23.e.i
  snapshot buffer for streams that never use it.
- Comment Extension (§24), with `GifImage::comments()` iterator and
  `concatenated_comment()` helper for the common "give me every comment
  in one buffer" path. `comments_are_7bit_ascii()` and
  `comments_in_recommended_position()` surface the §24.e.i / §24.e.ii
  *recommendations* (ASCII-only payload; leading-or-trailing position)
  as boolean queries — the encoder itself never enforces a
  recommendation, but consumers can gate on these checks when
  authoring stricter pipelines.
- Plain Text Extension (§25), including glyph rendering against a
  crate-local clean-room 8×8 monospace bitmap font (`font` module).
  §25.e leaves the font choice to the decoder; this crate ships a
  minimal stylised font covering printable ASCII (0x20..=0x7E) and
  falls back to space for anything outside that range, matching the
  §25.e fallback rule. Per §23.d ("This block can modify ... the Plain
  Text Extension"), an attached §23 Graphic Control Extension's
  §23.c.viii Transparency Index is honoured during plain-text
  rendering exactly as for §20 images: a cell foreground or background
  pixel whose Global-Color-Table *index* matches the transparency
  index leaves the display-device pixel unmodified (the prior canvas
  shows through) in both `compose()` and the lazy `Playback`
  iterator. `GifImage::plain_texts()` is the stream-level
  typed iterator: each §25 block paired with its attached §23 Graphic
  Control Extension (`(&PlainText, Option<GraphicControl>)`) in source
  order — the §25 companion to `frames_with_graphic_control()` so
  callers walking "every Plain Text block and the GCE that controls
  it" don't need to re-derive the §23 → §25 attachment from
  `GifImage::blocks`. `plain_texts_are_printable()` reports whether
  every payload byte sits in §25.e's recommended `0x20..=0xF7` range
  (anything outside would be substituted with a Space by a §25.e
  conforming renderer); `plain_texts_grid_fits_cells()` reports
  whether every §25.c grid is an integer number of character cells
  across and down — both surface §25.e *recommendations* as boolean
  queries so a strict authoring pipeline can gate on them, while the
  encoder itself never enforces a recommendation. The `PlainText`
  grid-geometry accessors decode the §25.a cell layout: `grid_columns()`
  / `grid_rows()` floor the §25.c Text Grid Width/Height by the
  Character Cell Width/Height (§25.a "fractional cells must be
  discarded"; `0` on a zero cell dimension), `grid_cell_count()` is
  their product, and `rendered_char_count()` =
  `min(text.len(), grid_cell_count())` is the §25.a "rendered until the
  end of data is reached or the character grid is filled" draw count.
  `text_overflows_grid()` and `has_empty_cells()` name the over- and
  under-fill edges, and `GifImage::all_plain_texts_fit_grid()` rolls the
  overflow query up to the stream so a re-encoding pipeline can confirm
  no Plain Text data is silently dropped before round-tripping.
- Application Extension (§26), with namespace classification.
  `app_ext::ApplicationKind` classifies an Application block by its
  §26.c.iv / §26.c.v identifier + authentication-code key into the five
  recognised ecosystem namespaces (NETSCAPE2.0 / ANIMEXTS1.0 / XMP / ICC
  / EXIF) or `Unknown`, following each typed view's matching rule
  (auth-code-sensitive for all but EXIF, which matches identifier-only).
  `Application::kind()` / `is_recognized()` are the per-block shorthands;
  `GifImage::application_kinds()` pairs every §26 block with its
  classification in source order, `unrecognized_application_extensions()`
  filters to the vendor-private blocks a re-encoding pipeline must
  preserve verbatim, and `find_application(identifier, auth_code)` is the
  general full-key lookup. Classification is by namespace, not payload, so
  a NETSCAPE2.0 block carrying no recognised sub-block still classifies
  `Netscape`.
- Multi-frame compositing onto the §18 Logical Screen using the §23
  disposal-method state machine. `compose()` returns the eager
  `Vec<ComposedFrame>`; `Playback::frames()` is the lazy iterator
  form (one canvas per call). Both cover all four defined disposal
  values (None / Keep / RestoreBackground / RestorePrevious), the
  §23.c.viii transparent-index handling, and Plain Text rendering
  via the same disposal state machine (Plain Text is a §25
  graphic-rendering block too). `tests/compose_disposal.rs` pins
  the spec-implicit corners as well — transparent-index show-through
  over a prior canvas, no-GCT RestoreBackground (falls back to fully
  transparent black per §18.c.iii), RestoreBackground only clearing
  the disposing frame's *own* placement rect (not the entire prior-
  frame footprint), nested RestorePrevious chains where each frame's
  pre-render snapshot is independent, full-screen RestoreBackground
  wiping the entire logical screen, and the `ComposedFrame::
  delay_centis` = disposing-frame's-own-§23.c.vii contract.
- Animation playback iterator `Playback::looping_frames()` that
  honours the NETSCAPE2.0 *Looping* sub-block: no extension plays one
  pass, `loop_count = 0` loops forever, `loop_count = N` plays
  `N + 1` total passes per the de-facto convention documented in
  `docs/image/gif/netscape2.0-loop-extension.md`. Each yielded
  `PlaybackFrame` carries its delay as a `Duration` for ergonomic
  `thread::sleep` calls.
- Animation-timing accessors on `GifImage`. `frame_delays()` iterates
  every graphic-rendering block's §23.c.vii Delay Time as a `Duration`
  (§20 Images and §25 Plain Text both count; no GCE or a 0 delay →
  `Duration::ZERO`), `is_animated()` is true only for multi-frame
  streams, `single_pass_duration()` totals one pass, and
  `total_play_duration()` multiplies that by the NETSCAPE2.0 /
  ANIMEXTS1.0 pass count (`None` for the infinite-loop case). The
  timeline view matches `Playback`'s per-frame delays exactly without
  compositing any pixels.
- Stream-level §23 rendering-flag queries. `has_transparency()` is
  true when any graphic-rendering block's §23 Graphic Control
  Extension sets the §23.c.vi Transparency Flag (a §23.c.viii
  Transparent Index is given), so a renderer can decide whether to
  allocate an alpha channel without walking every frame's
  `transparent_index`; `requires_user_input()` is true when any GCE
  sets the §23.c.v User Input Flag, telling an interactive viewer
  whether it needs an input-aware playback loop. Both share the
  §23.d graphic-rendering-block spine with `frame_delays()`.
  `blocks_indefinitely_for_user_input()` is the §23.e.ii corner that
  `requires_user_input()` alone misses: it is `true` only when some GCE
  sets the User Input Flag *and* leaves the §23.c.vii Delay Time at 0,
  the case §23.e.ii says the decoder "should wait for user input
  indefinitely" (a strictly stronger condition — a user-input GCE that
  pairs the flag with a non-zero Delay Time is the bounded "user input
  or delay expiry, whichever first" wait of §23.c.vii, and so does not
  count). `GraphicControl::waits_for_user_input_indefinitely()` is the
  per-GCE predicate it rolls up. When the stream-level query is `true`,
  `total_play_duration()` cannot bound the run and a renderer must be
  input-aware.
- Fluent animation assembly (`builder::AnimationBuilder`) — the
  encode-side counterpart to the timing accessors.
  `new(width, height, palette)` shares one §18 Global Color Table;
  `add_full_frame` / `add_placed_frame` append frames, each attaching
  a §23 Graphic Control Extension that carries the Delay Time and
  Disposal Method; `loop_forever()` / `loop_count(n)` / `play_once()`
  pick the looping behaviour, emitting a NETSCAPE2.0 *Looping*
  Application Extension ahead of the frames for the looping cases.
  `build()` validates placement (rectangles must fit the Logical
  Screen), index counts, palette-index range, and the §19 1..=256
  palette-size limit, returning a `Gif89a` `GifImage` ready for
  `encode`; the result's timeline accessors read back exactly what was
  set and a build → encode → decode round-trip is value-stable.
- Structured views over the five ecosystem-defined Application
  Extensions (`app_ext` module) — NETSCAPE2.0 looping +
  buffering sub-blocks, the older ANIMEXTS1.0 looping variant (same
  *Looping* sub-block layout under a different identifier+auth, used
  by some pre-Netscape encoders), the Adobe XMP packet (`XMP Data`),
  the ICC colour profile (`ICCRGBG1`), and the EXIF metadata blob
  (`Exif    `, Exif 2.3 §4.7.2). `GifImage::loop_count()` /
  `xmp_packet()` / `icc_profile()` / `exif()` are convenience
  accessors over the same raw `Block::Application` data, which stays
  in `GifImage::blocks` for byte-stable round-trip;
  `GifImage::loop_count()` prefers NETSCAPE2.0 and falls back to
  ANIMEXTS1.0 when NETSCAPE2.0 is absent. Because the decoder collapses
  the §15 sub-block boundaries into one flat `Application::data` buffer,
  `LoopControl::from_application` / `AnimextsLoopControl::from_application`
  re-frame that buffer by scanning for the known *Looping* (`0x01`) and
  *Buffering* (`0x02`) sub-block IDs: an unrecognised sub-block (a future
  NETSCAPE2.0 control or an encoder-private hint) interleaved ahead of the
  *Looping* sub-block is resynced past one byte at a time rather than
  abandoning the block, so the loop count is recovered regardless of
  sub-block order. Each field is captured at its first complete
  occurrence, keeping the typed view stable and `to_application`
  round-trips byte-identical.
- Encoder Global vs Local Color Table optimisation
  (`GifImage::optimize_color_tables`) — when every image frame
  carries the same palette, hoists it into the §18 Global Color
  Table and clears the now-redundant §21 Local Color Tables, saving
  `3 × 2^(size_bits + 1)` bytes per frame. Pixels are unaffected
  (§21 says a frame with the LCT flag clear uses the §18 GCT).
- Encoder inter-frame rect optimisation
  (`GifImage::optimize_frame_rects`) — the §20-placement companion to
  `optimize_color_tables`. Re-runs the §23 disposal-method state
  machine and crops every §20 Image frame to the bounding rectangle of
  the pixels it actually changes on the composed logical screen
  (§20.c.ii–v let a frame cover any sub-rectangle; pixels outside it
  are simply not overwritten), shrinking the §22 pixel payload + LZW
  stream while leaving the composed RGBA output byte-identical.
  Frames whose §23.c.iv disposal is rect-independent are eligible
  (values 0/1, and 3 — the pixels a cropped frame no longer overwrites
  already equal the pre-render canvas it restores); Restore-to-
  background frames are never touched ("the area used by the graphic
  must be restored" — shrinking it would change the cleared region).
  §23.c.viii transparent pixels and pixels that re-draw the colour
  already on the canvas count as unchanged; exact-duplicate frames
  shrink to 1×1; non-composing streams are left unmodified; the pass
  is idempotent. `tests/optimize_frame_rects.rs` pins the per-disposal
  eligibility plus a randomized compose-equivalence property, and the
  `decode` fuzz harness asserts `compose(before) == compose(after)` on
  every decodable fuzz input.
- Truecolor RGBA → GIF encode path (`quantize` module +
  `GifImage::from_rgba_frame` / `from_rgba_frames`). A GIF cannot carry
  truecolor — §19/§21 cap a colour table at 256 entries and §22 stores
  one palette index per pixel — so an encoder fed arbitrary 24-bit RGB
  has to pick a representative palette and map every pixel to it. The
  `quantize` module does that with **median cut** (a general,
  format-independent technique; the spec's only stake is the
  ≤256-entry/index-plane output shape): `quantize_rgba` reduces an RGBA
  buffer, folding GIF's lack of per-pixel alpha down to the single
  §23.c.viii Transparency Index (sub-threshold-alpha pixels route to one
  reserved palette slot returned as `Quantized::transparent_index`);
  `quantize_rgb` is the no-alpha entry point; `nearest_index` maps a
  colour to an existing palette for shared-palette compositing. The §22
  index plane is the per-pixel nearest-entry argmin over the selected
  palette — after each median-cut box is averaged to one entry, every
  pixel is remapped to its true nearest entry rather than keeping the box
  it was partitioned into, so a colour near a box boundary never carries a
  strictly-worse assignment (exact-colour inputs stay byte-stable).
  `QuantizeOptions { max_colors, dither }` adds a `Dither` choice:
  `Dither::FloydSteinberg` diffuses each pixel's quantisation error onto
  its not-yet-visited neighbours (7/16 east, 3/16 south-west, 5/16 south,
  1/16 south-east) so a coarse-palette gradient breaks into a stippled mix
  that averages back to the source over a small neighbourhood — palette
  selection is unchanged, only the index-plane assignment; transparent
  pixels neither receive nor propagate error. `quantize_rgb_with_options`
  / `quantize_rgba_with_options` are the option-taking entry points (the
  bare functions keep the flat nearest-entry default).
  `GifImage::from_rgba_frame` wraps a single frame into a §18 Logical
  Screen with a §19 Global Color Table (attaching a §23 GCE carrying the
  reserved transparency index when the frame has transparent pixels,
  staying GIF87a when fully opaque); `from_rgba_frames` builds a
  multi-frame animation, each frame quantised to its own §21 Local Color
  Table and carrying a §23 GCE with its §23.c.vii delay + §23.c.iv
  disposal, with `loop_count` threading the NETSCAPE2.0 §26 looping
  Application Extension; `from_rgba_frame_with_options` /
  `from_rgba_frames_with_options` thread a `QuantizeOptions` (dither) into
  those paths. `from_rgba_frames_shared_palette` is the one-palette
  variant: `quantize::quantize_frames_shared` pools every frame's opaque
  pixels into a single median cut and installs the result as one §18
  Global Color Table (no per-frame §21 LCTs), reserving one shared
  §23.c.viii Transparency Index across the animation — smaller output than
  N independent tables, with no inter-frame palette flicker. The registry
  `GifEncoder` routes through this: a fully-opaque ≤256-colour frame keeps
  its exact palette (lossless), and a >256-colour or transparent frame is
  quantised instead of rejected.
- `decode_first_frame` cover-frame fast-path that short-circuits at
  the first image-bearing block and skips the per-block dispatch
  for everything that follows. Useful when you only need a static
  thumbnail of an animated stream.
- `decode_lenient` error-recovery decoder that skips corrupted
  sub-blocks, malformed extensions, and partial frames by scanning
  forward to the next §20 Image Separator / §27 Trailer. Use for
  viewers / thumbnailers / recovery tools that prefer "show what
  we can" over "all or nothing"; the strict `decode` entry point
  stays the default for round-trip-stable consumers.
- §7 "Required Version" enforcement on encode. The encoder honours
  the per-block "Required Version" table — §23 Graphic Control,
  §24 Comment, §25 Plain Text, and §26 Application Extensions all
  require 89a — and refuses to emit a `GIF87a`-labeled stream that
  contains any of them. `GifImage::required_version()` returns the
  minimum version that covers the block list, and
  `upgrade_version_if_needed()` bumps the declared version to that
  minimum in one call (it never *down*grades — a caller's explicit
  choice of `Gif89a` for a 87a-compatible payload is preserved).
- Non-fatal conformance reporting. `GifImage::conformance_report()`
  walks an in-memory image against the Appendix-B grammar and the
  §7–§26 field rules and returns a `ConformanceReport` — the diagnostic
  counterpart to `encode`'s fatal validation, and a *superset* of it.
  Alongside the §7 version, §19/§21 colour-table-size, and §20/§22
  indices-length rules `encode` also rejects, the report surfaces the
  placement / range / recommendation departures the encoder tolerates:
  §20.a images escaping the Logical Screen, §18.c.vii Background Color
  Index past the Global Color Table, §22/Appendix-F pixel indices past
  the active palette (one issue per frame, not per pixel), §23.c.viii
  Transparent Color Index past the palette, §25.c.x/xi Plain Text fg/bg
  indices past the active palette (and the no-active-table case), and
  the §23.e.ii User-Input-without-Delay *recommendation*. Each
  `ConformanceIssue` carries a machine-comparable `ConformanceRule`, a
  `ConformanceSeverity` (`Error` for spec requirements, `Recommendation`
  for spec recommendations), the offending `block_index` (or `None` for
  a Logical-Screen-Descriptor / Global-Color-Table stream-level issue),
  and a spec-cited `detail`; `ConformanceReport` exposes `is_clean()` /
  `has_errors()` / `errors()` / `recommendations()` / `count(severity)`
  and a one-issue-per-line `Display`. `GifImage::validate_strict()` is
  the hard-gate convenience: `Ok(())` when no error-level issue is found
  (recommendations tolerated), else an `Error::InvalidInput` listing
  every error. Because the report is a superset of the encoder's checks,
  `validate_strict().is_ok()` implies `encode` accepts the image but not
  conversely.

## Fuzzing

`fuzz/fuzz_targets/` ships seven `cargo-fuzz` harnesses, all asserting
panic-freedom on arbitrary bytes:

- `decode_panic_free` — strict `decode` entry point.
- `decode_lenient_panic_free` — error-recovery `decode_lenient` entry
  point (different resync state machine).
- `roundtrip` — decoder output round-trips through encoder + decoder
  with `assert_eq!` on the resulting `GifImage`.
- `decode` — end-to-end decode-side harness: chains `decode_lenient` +
  `decode_first_frame` + `decode` + `compose` + `Playback::frames` +
  `Playback::looping_frames` + the §26 Application Extension typed
  parsers + the §24 Comment Extension accessors + the §18.c.viii Pixel
  Aspect Ratio decoder + the §7 Required Version inference + an
  encode-then-re-decode through every fuzz-derived `GifImage`. Also
  asserts (not just panic-freedom) that `optimize_frame_rects`
  preserves the composed output exactly on every decodable input —
  a `compose(before) != compose(after)` mismatch fails the run. Caps the
  composited canvas at 1 Mpx and the looping iterator at 64 frames so a
  `loop_count = Some(0)` (forever) stream doesn't pin the fuzzer.
- `encode` — end-to-end encode-side harness: derives a `GifImage` from
  fuzz bytes via `AnimationBuilder` (rect placement, palette size,
  per-frame disposal, NETSCAPE2.0 / loop-forever behaviour), then
  drives `encode` → `decode` → `decode_lenient` → `decode_first_frame`
  → `compose` → `Playback::frames` / `looping_frames` on the result.
  Reaches encoder configurations the decoder-output-only harness
  can't construct (sub-screen placements, mismatched palette sizes,
  multi-frame disposal sequences). Also synthesises small (≤ 32×32) RGBA
  frames from the fuzz bytes and drives the truecolor quantiser paths —
  `quantize_rgba_with_options` and `quantize_frames_shared` under both
  `Dither::None` and `Dither::FloydSteinberg`, plus the
  `from_rgba_frame_with_options` / `from_rgba_frames_shared_palette`
  constructors through `encode` → `decode` — asserting the index plane
  stays in palette range and the decoder accepts every encoded stream.
  Caps screen at 256×256 and frame count at 16.
- `plain_text` — dedicated §25 Plain Text Extension
  harness. `AnimationBuilder` exposes image frames only, and the
  decoder-side harnesses can only emit a `Block::PlainText` when the
  fuzzer stumbles onto the `0x21 0x01 0x0C` extension-introducer
  prefix — so the §25 grammar is effectively unreached on truly
  arbitrary input. This harness builds a `GifImage` whose `blocks`
  are exclusively Plain Text Extensions (each optionally carrying a
  §23 GCE), then drives `encode` → `decode` (strict + lenient +
  cover-frame) → `compose` → `Playback`. Covers the §25.c.viii/ix
  `cell_width = 0` no-op short-circuit, the §25.c.x/xi out-of-palette
  fg/bg-index clamp in `render_plain_text`, multi-sub-block §15
  payload splitting on text > 255 B, the §25.e font-fallback to
  space for non-ASCII bytes, and the §23.f.i snapshot-and-revert
  path when a `RestorePrevious` GCE is attached to a Plain Text
  block (a path the decoder-side fuzzer reaches only by chance).
- `lzw` — dedicated Appendix F LZW codec harness driving
  `lzw::decode` / `lzw::encode` *directly* with fuzzer-controlled
  parameters. Every decoder-facing harness reaches the LZW path only
  through the §17/§18/§20 container parser, which constrains
  `min_code_size` to the §22.c.i byte, the compressed bytes to
  re-assembled §15 sub-blocks, and `expected_pixels` to exactly the
  Image Descriptor's `width × height`. This harness instead slices the
  fuzz input into a raw `min_code_size` (the full `u8` range — values
  outside [2, 8] must `Err`, never panic on the `1 << min_code_size`
  shift), a 32-bit `expected_pixels` selector (a hostile value near
  `u32::MAX` against a tiny payload forces the
  `expected_pixels.min(src.len() × MAX_TABLE_SIZE)` allocation clamp),
  and an arbitrary compressed bitstream. Walks the §F.4 code-width
  growth on code sequences the encoder would never emit, the KwKwK
  self-reference branch on the first non-Clear code, the
  over-dictionary (`code > next_code`) rejection, §F.1 Clear / §F.2 EOI
  at arbitrary positions, the no-EOI-before-end `Err`, and the
  deferred-clear 4096-entry saturation regime. Also asserts
  `decode(encode(x)) == x` on every index buffer the encoder accepts,
  and that `LzwEncoder::encode_frame` matches the free `lzw::encode`.

Each harness keeps a local corpus under `fuzz/corpus/` (gitignored —
the corpus is a per-machine flywheel, not a checked-in artifact).
`fuzz/Cargo.lock` *is* committed so the harness builds reproducibly.

A small set of spec-derived seed inputs lives under
`fuzz/seed_corpus/<target>/` (tracked). The three decoder-facing
targets each get five seeds: the two well-formed fixtures from
`tests/spec_fixtures.rs` (1×1 GIF87a minimal, 2×2 GIF89a + GCE) plus
three malformed inputs hitting classic problem areas — truncated §27
trailer, §22.c.i LZW min-code-size = 12 (illegal per Appendix F's
12-bit clamp), and a §26 Application Extension whose sub-block length
over-claims by 0x42 − 0x03 = 63 bytes. The `plain_text` target gets
three §25-specific seeds — one bare in-bounds block, one with a §23
`RestorePrevious` GCE attached, and one with `cell_width = 0` for the
no-op render short-circuit. The `lzw` target gets six seeds: two
well-formed mcs=2 streams (the 16-pixel `[0,1,2,3]×4` and 1-pixel
fixtures from the `lzw::decode` unit tests), an illegal
`min_code_size = 12`, a hostile `expected_pixels` near `u32::MAX`
against a 2-byte payload (allocation-clamp), an out-of-range first
code (KwKwK / uninitialised-prefix), and a Clear-only stream that ends
before EOI. Bootstrap a fresh corpus with
`cp -n fuzz/seed_corpus/<target>/* fuzz/corpus/<target>/`. Seeds are
content-addressed by SHA-1 and regenerable via `tools/seedgen.py`
(pure-Python, no GIF library invoked).

All seven harnesses run crash-free over long fuzzing sessions
(the end-to-end `decode` target clears hundreds of thousands of
executions per minute with the `optimize_frame_rects`
compose-equivalence assert enabled in the loop; the `lzw` target
cleared 437K runs in 46 s with zero finds at round 318).

## Benchmarking

`benches/` ships four [Criterion](https://github.com/bheisler/criterion.rs)
harnesses driving the decoder, encoder, end-to-end roundtrip, and
direct Appendix F LZW codec-pair hot paths. Each scenario synthesises
its inputs on the fly — no committed fixture files. Run with:

```
cargo bench -p oxideav-gif --bench decode
cargo bench -p oxideav-gif --bench encode
cargo bench -p oxideav-gif --bench roundtrip
cargo bench -p oxideav-gif --bench lzw
```

The first three harnesses together cover 14 scenarios spanning
single-frame stills (320×240 / 64×64), multi-frame animations
(320×240 4f / 64×64 8f), the `decode_lenient` resync path, the
`decode_first_frame` cover-frame fast path, the lazy `Playback::frames`
iterator vs eager `compose`, and the `AnimationBuilder::build`
validation pass in isolation. The fourth (`lzw`) measures
the `lzw::encode` / `lzw::decode` pair in isolation across four
sizes — see [`BENCHMARKS.md`](BENCHMARKS.md) for the scenario matrix
and the baseline numbers.

## Not implemented

- Other vendor Application Extensions beyond the five surfaced via
  `app_ext` — these stay surfaced as raw `Block::Application` data
  with the identifier, authentication code, and concatenated payload
  bytes.

## Specifications

- *Graphics Interchange Format Version 89a* (CompuServe, 1990).
- *GIF — A standard defining a mechanism for the storage and
  transmission of raster-based graphics information* (CompuServe,
  1987).

Both texts are mirrored at `docs/image/gif/` in the workspace.

For the LZW algorithm itself, the canonical reference is T. Welch,
*A Technique for High-Performance Data Compression*, IEEE Computer
17(6):8-19 (June 1984).

## License

Apache-2.0. See `LICENSE`.
