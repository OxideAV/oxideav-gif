# Changelog

## [Unreleased]

### Added

- Shared-palette multi-frame quantisation. `quantize::quantize_frames_shared`
  pools every frame's opaque pixels and runs a single median cut over the
  union, returning a `SharedQuantized { palette, frame_indices,
  transparent_index }` whose one palette every frame's §22 index plane
  references. `GifImage::from_rgba_frames_shared_palette` builds the
  animation around it: the shared palette is installed as one §18 Global
  Color Table and no frame carries a §21 Local Color Table, so the encoded
  stream is smaller (one table, not N) and a viewer never re-loads a
  per-frame table between frames. A single §23.c.viii Transparency Index is
  reserved across the whole animation when any frame has transparent
  pixels; `opts.dither` applies per frame against the shared palette. The
  per-frame-LCT `from_rgba_frames` path is unchanged. 5 new `quantize`
  unit tests (empty/length-mismatch errors, union coverage, budget clamp
  across the union, shared transparent slot) + 2
  `tests/quantize_animation.rs` integration tests (one-GCT/no-LCT
  round-trip + compose, and a size comparison proving the shared encode is
  no larger than per-frame LCTs).

- Floyd–Steinberg error-diffusion dithering for the truecolor encode
  path. The `quantize` module gains a `Dither` strategy enum
  (`None` / `FloydSteinberg`) and a `QuantizeOptions { max_colors,
  dither }` bundle, plus `quantize_rgb_with_options` /
  `quantize_rgba_with_options` entry points. Palette *selection* is still
  median cut; the dither only changes the §22 index-plane assignment,
  diffusing each pixel's quantisation error onto its not-yet-visited
  neighbours (7/16 east, 3/16 south-west, 5/16 south, 1/16 south-east) so
  a smooth gradient that would band under a coarse palette breaks into a
  stippled mix of the two nearest entries that averages back to the source
  colour over a small neighbourhood. A general image-processing technique;
  the GIF spec only constrains the ≤256-entry / index-plane output shape,
  which is unchanged. Transparent pixels neither receive nor propagate
  error (they are never displayed). `GifImage::from_rgba_frame_with_options`
  / `from_rgba_frames_with_options` expose the choice at the constructor
  level. The bare `quantize_*` / `from_rgba_*` functions keep the flat
  nearest-entry default, so existing callers are unaffected. 7 new
  `quantize` unit tests (dither-none-equals-plain, in-range indices,
  solid-image no-op, banding broken into a mix, block-averaged error
  reduction on a gradient, transparent-slot exclusion) + 2
  `tests/quantize_animation.rs` integration tests (dithered still and
  dithered animation round-trip + compose/Playback parity).

### Changed

- Quantiser index-plane assignment is now nearest-entry rather than
  box-of-origin. After median cut averages each colour box to one palette
  entry, the `quantize` module maps every sample to its true nearest
  palette entry (squared-Euclidean RGB) instead of keeping the box it was
  partitioned into. A sample near a box boundary can be closer to a
  neighbouring box's average than to its own; the remap removes that
  residual error at no change to the selected palette. Exact-colour inputs
  (≤ budget distinct colours) are unaffected — each colour is still its
  own box average — so the lossless round-trip path is byte-stable. The
  index plane is now provably the per-pixel argmin over the final palette.

### Added

- Truecolor RGBA → GIF encode path. A new `quantize` module reduces
  arbitrary 24-bit RGB to a conformant ≤256-entry §19/§21 colour table +
  §22 index plane using **median cut** (a general, format-independent
  quantiser — the GIF spec only constrains the ≤256-entry/index-plane
  output shape). `quantize::quantize_rgba` folds GIF's lack of per-pixel
  alpha down to the single §23.c.viii Transparency Index (sub-threshold-
  alpha pixels route to one reserved slot returned as
  `Quantized::transparent_index`); `quantize::quantize_rgb` is the
  no-alpha entry point; `quantize::nearest_index` maps a colour to an
  existing palette. Two public `GifImage` constructors layer on top:
  `from_rgba_frame` builds a single-image §18 Logical Screen with a §19
  Global Color Table (attaching a §23 GCE with the reserved transparency
  index when needed, staying GIF87a when fully opaque), and
  `from_rgba_frames` builds a multi-frame animation — each frame
  quantised to its own §21 Local Color Table, each carrying a §23 GCE
  with its §23.c.vii delay + §23.c.iv disposal, and `loop_count`
  threading the NETSCAPE2.0 §26 looping Application Extension. The
  registry `GifEncoder` now uses this path: a fully-opaque ≤256-colour
  frame keeps its exact palette (lossless), a >256-colour or transparent
  frame is quantised instead of rejected (previously the encoder errored
  on any input with more than 256 distinct colours). 24 new tests (11
  `quantize` unit + 8 `image` constructor unit + 3 `registry` unit re-
  pinned/added + 5 `tests/quantize_animation.rs` integration: single
  truecolor still lossless round-trip, high-colour median-cut within the
  §19 limit, multi-frame R→G→B animation compose + lazy-Playback parity,
  animated transparency show-through, and repeated-palette hoist into a
  GCT).

- Non-fatal conformance reporting: `GifImage::conformance_report()`
  walks an in-memory image against the Appendix-B grammar and the
  surrounding §7–§26 field rules and returns a `ConformanceReport` —
  the diagnostic counterpart to `encode`'s fatal validation. It is a
  *superset* of the encoder's checks: alongside the §7 version, §19/§21
  colour-table-size, and §20/§22 indices-length rules `encode` also
  rejects, it surfaces the placement / range / recommendation
  departures the encoder *tolerates* — §20.a images escaping the
  Logical Screen, §18.c.vii Background Color Index past the GCT,
  §22/Appendix-F pixel indices past the active palette (one issue per
  frame, not per pixel), §23.c.viii Transparent Color Index past the
  palette, §25.c.x/xi Plain Text fg/bg indices past the active palette
  (and the no-active-table case), and the §23.e.ii User-Input-without-
  Delay *recommendation*. Each `ConformanceIssue` carries a
  machine-comparable `ConformanceRule`, a `ConformanceSeverity`
  (`Error` for spec *requirements*, `Recommendation` for spec
  *recommendations*), the offending `block_index` (or `None` for a
  Logical-Screen-Descriptor / Global-Color-Table stream-level issue),
  and a spec-cited `detail` string. `ConformanceReport` exposes
  `is_clean()` / `has_errors()` / `errors()` / `recommendations()` /
  `count(severity)` so a strict validator can gate on errors while a
  lint surfaces both tiers. `ConformanceIssue`, `ConformanceReport`,
  `ConformanceRule`, and `ConformanceSeverity` are re-exported at the
  crate root. New `conformance` unit tests pin every rule on hand-built
  images; a new `conformance_report` integration test pins the two
  cross-cutting properties — decoder/builder output reports no errors,
  and the report agrees with `encode` on the rules they share while
  reaching beyond it on placement / range / recommendation departures.
- `GifImage::validate_strict()` hard-gate convenience over the
  diagnostic report: returns `Ok(())` when no `ConformanceSeverity::Error`
  is found (recommendation-level departures tolerated), else
  `Error::InvalidInput` carrying every error issue one-per-line. Because
  the report is a superset of `encode`'s fatal checks,
  `validate_strict().is_ok()` implies `encode` accepts the image but not
  conversely. `Display` impls on `ConformanceSeverity`,
  `ConformanceIssue` (`error [block 2]: §20.a: …`), and
  `ConformanceReport` (one issue per line; `conformant: no issues` when
  clean) make the report log/surface-ready. New unit tests pin the
  Display shapes, the recommendation-tolerant strict pass, and the
  all-errors-collected strict failure; a new `conformance_report`
  integration test gates `validate_strict` end-to-end through decode +
  mutation.

### Changed

- `app_ext::LoopControl::from_application` and
  `AnimextsLoopControl::from_application` now resync past unrecognised
  bytes instead of abandoning the whole block at the first unknown
  sub-block ID. Because the decoder collapses the §15 sub-block
  boundaries into one flat `Application::data` buffer, the parser
  re-frames the payload by scanning for the known *Looping* (`0x01`) and
  *Buffering* (`0x02`) sub-block IDs; the previous "bail on first unknown
  ID" rule silently dropped a *Looping* loop count whenever a sub-block
  this parser does not recognise (e.g. a future NETSCAPE2.0 control or an
  encoder-private hint) preceded it. The scan now advances a single byte
  past anything it cannot frame and keeps looking, so the *Looping* count
  is recovered regardless of sub-block order or interleaved unknown
  sub-blocks. Each field is captured at its first complete occurrence (a
  later stray match cannot overwrite a resolved value), so the typed view
  stays stable and the existing `to_application` round-trips are
  unaffected. New `app_ext` unit tests pin loop-recovery-after-unknown,
  buffer-then-loop ordering, first-occurrence-wins, trailing-unknown, and
  the empty-payload default-view shapes.

### Added

- §25 Plain Text Extension grid-geometry accessors on `PlainText`.
  `grid_columns()` / `grid_rows()` decode the §25.c Text Grid
  Width/Height against the Character Cell Width/Height into whole-cell
  counts (floor; §25.a "fractional cells must be discarded", and `0`
  when the cell dimension is `0`); `grid_cell_count()` is their product.
  `rendered_char_count()` is `min(text.len(), grid_cell_count())` — the
  §25.a "rendered until the end of data is reached or the character grid
  is filled" draw count — with `text_overflows_grid()` and
  `has_empty_cells()` as the over/under-fill edge queries.
  `GifImage::all_plain_texts_fit_grid()` is the stream-level rollup
  confirming no block drops textual data off the bottom/right of its
  grid.
- §26 Application Extension namespace classification. The new
  `app_ext::ApplicationKind` enum classifies an `Application` block by
  its §26.c.iv / §26.c.v identifier + authentication-code key into the
  five recognised ecosystem namespaces (`Netscape`, `Animexts`, `Xmp`,
  `Icc`, `Exif`) or `Unknown`, following each typed view's own matching
  rule (auth-code-sensitive for all but EXIF, which matches on
  identifier only). `ApplicationKind::classify()` / `is_recognized()`
  are the core entry points; `Application::kind()` /
  `Application::is_recognized()` are the per-block shorthands. Stream
  rollups on `GifImage`: `application_kinds()` pairs every §26 block
  with its classification in source order,
  `unrecognized_application_extensions()` filters to the vendor-private
  (`Unknown`) blocks a re-encoding pipeline must preserve verbatim, and
  `find_application(identifier, auth_code)` is the general full-key
  lookup (distinct from the EXIF identifier-only `exif()` and the typed
  `loop_count()` / `xmp_packet()` / `icc_profile()` views). Classification
  is by namespace, not payload, so a NETSCAPE2.0 block with no recognised
  sub-block still classifies `Netscape`. New `app_ext` unit tests pin
  every-known-namespace, wrong-auth-code → `Unknown`, namespace-not-payload;
  a new `app_ext_roundtrip` integration test confirms classification +
  lookup survive encode → decode.
- §12 "Blocks, Extensions and Scope" classification + §11 palette-loader
  recognition. `Block::class()` returns the §12 `BlockClass`
  (`GraphicRendering` for §20 Image / §25 Plain Text, `SpecialPurpose`
  for §24 Comment / §26 Application; `Control` modelled for completeness
  — Header / §18 LSD / §23 GCE / §27 Trailer are structural fields or
  attached, never free-standing `Block`s). `Block::is_graphic_rendering()`
  and `is_special_purpose()` are the boolean forms;
  `GifImage::graphic_rendering_block_count()` (§20 + §25) and
  `special_purpose_block_count()` (§24 + §26) are the stream-level
  rollups, partitioning the block list exactly (no §12 Control block is
  ever a list entry). `GifImage::is_palette_loader_stream()` recognises
  the §11 "About Color Tables" table-install shape — a §18 Global Color
  Table present with no graphic-rendering block (§12-transparent Comment
  / Application blocks do not disqualify it), the "Header, Logical Screen
  Descriptor, a Global Color Table and the GIF Trailer" stream §11
  describes for loading a decoder with a palette ahead of subsequent
  tableless Data Streams. `BlockClass` is re-exported at the crate root.
  New `image` unit tests pin the §12 taxonomy, the rollup partition, and
  the §11 loader recognition (GCT-only, GCT+metadata-only,
  image/plain-text disqualification, no-GCT negative).

- `fuzz/fuzz_targets/lzw.rs` — dedicated Appendix F LZW codec fuzz
  harness (round 318, depth-mode fuzz). The decoder-facing harnesses
  (`decode`, `decode_panic_free`, `decode_lenient_panic_free`,
  `roundtrip`) reach `lzw::decode` only *through* the §17/§18/§20
  container parser, which constrains `min_code_size` to the §22.c.i
  byte, the compressed bytes to re-assembled §15 sub-blocks, and
  `expected_pixels` to exactly the Image Descriptor's `width × height`.
  This harness drives `lzw::decode` / `lzw::encode` directly with
  fuzzer-controlled parameters: the full `u8` `min_code_size` range
  (out-of-[2,8] values must `Err`, never panic on the
  `1 << min_code_size` shift), a 32-bit `expected_pixels` selector (a
  value near `u32::MAX` against a tiny payload forces the
  `expected_pixels.min(src.len() × MAX_TABLE_SIZE)` allocation clamp),
  and an arbitrary compressed bitstream. Surfaces §F.4 code-width
  growth on encoder-impossible code sequences, the KwKwK first-code /
  uninitialised-prefix branch, the over-dictionary (`code > next_code`)
  rejection, §F.1 Clear / §F.2 EOI at arbitrary positions, the
  no-EOI-before-end `Err`, and the deferred-clear 4096-entry saturation
  regime. Also asserts `lzw::decode(lzw::encode(x)) == x` on every
  index buffer the encoder accepts and that `LzwEncoder::encode_frame`
  is byte-identical to the free `lzw::encode`. Six tracked seeds in
  `fuzz/seed_corpus/lzw/` (two well-formed mcs=2 streams from the
  `lzw::decode` unit fixtures + four adversarial parameter perturbations)
  anchor each path; regenerable via `tools/seedgen.py`. Cleared 437K
  runs in 46 s with zero finds.

- `GifImage::blocks_indefinitely_for_user_input()` +
  `GraphicControl::waits_for_user_input_indefinitely()` — surface the
  §23.e.ii "wait for user input indefinitely" corner. Per §23.e.ii, when
  the §23.c.v User Input Flag is set with no §23.c.vii Delay Time
  (`delay_centis == 0`), "the decoder should wait for user input
  indefinitely" — an unbounded wait that a purely time-driven playback
  loop cannot serve. The per-GCE predicate flags exactly that pair; the
  stream-level any-block query is a strictly stronger condition than
  `requires_user_input()` (a stream can require user input yet never
  block indefinitely if every user-input GCE also carries a non-zero
  Delay Time, which §23.c.vii makes a bounded "user input *or* delay
  expiry, whichever first" wait). When the new query is `true`,
  `total_play_duration()` cannot bound the run and a renderer must be
  input-aware. Round 315.

- `encode_with_options()` + `EncodeOptions` + `LzwStrategy` — the
  top-level encoder now exposes the Appendix-F table-full strategy as a
  caller-selectable knob. `EncodeOptions { lzw_strategy }` chooses
  between `LzwStrategy::DeferredClear` (the byte-stable historical
  default: freeze the full 4096-entry dictionary and keep emitting 12-bit
  codes against it to end-of-image) and `LzwStrategy::ClearOnFull`
  (emit a Clear and rebuild the dictionary the instant the table fills,
  so it re-adapts to later content). Before this, the size-minimising
  `ClearOnFull` path added in round 299 was only reachable on the raw
  `lzw::encode_with_clear_on_full` free function — never through the GIF
  encoder; now every §20 Image frame's LZW stream routes through the
  selected strategy. `encode(image)` is unchanged and equals
  `encode_with_options(image, EncodeOptions::default())` byte-for-byte.
  Both strategies decode to identical pixels (`lzw::decode` honours a
  mid-stream §F.1 Clear) and emit byte-identical streams for any frame
  whose dictionary never reaches the 4096-entry ceiling — the choice
  only changes the bytes for table-filling frames, where `ClearOnFull`
  is strictly smaller on a regime-changing raster (pinned in
  `encoder` unit tests on a 192×192 two-regime fixture). Round 305.

- `lzw::encode_with_clear_on_full()` — an opt-in companion to
  `lzw::encode` implementing the *clear-on-full* table strategy from
  Appendix F's cover sheet. Where `lzw::encode` follows the *deferred
  clear* rule (freeze the 4096-entry dictionary at its maximum size and
  keep emitting 12-bit codes against it until end-of-image), this
  function emits a Clear code (value `2^min_code_size`, written at the
  current 12-bit width) and rebuilds the dictionary from the §F.3
  initial state the instant the table fills. The frozen-table path stops
  learning new patterns past the 4096-entry point, so on a large raster
  whose later content differs from its early content it codes that
  content against a dictionary tuned to the wrong regime; re-adapting
  typically yields a smaller stream. Both functions decode to identical
  pixels — `decode` already honours a mid-stream Clear (§F.1 "reset
  table state") — and emit byte-identical output for any raster that
  never fills the table, so the existing `encode` output stays
  byte-stable and this is purely an additive encoder size/speed
  trade-off. New lzw unit tests pin: multi-fill round-trip correctness
  through `decode`, byte-identity with `encode` below the table-full
  ceiling, a regime-change input where the re-adapting path is ~3.7 %
  smaller (166 077 → 159 854 B), and the shared empty-raster /
  out-of-palette / out-of-range-`min_code_size` reject paths. Round 299.

- `GifImage::all_blocks_fit_screen()` / `out_of_bounds_block_count()` —
  read-only §20.a / §25.a validation accessors. The spec makes "each
  image must fit within the boundaries of the Logical Screen" a hard
  requirement (not a recommendation), and `compose()` / `Playback`
  already reject an escaping placement with an error. These accessors
  surface the same check up front without rendering: `all_blocks_fit_
  screen()` is `true` when every §20 Image and §25 Plain Text grid's
  right edge (`left + width`) is within the §18.b Logical Screen Width
  and its bottom edge (`top + height`) within the Logical Screen
  Height; `out_of_bounds_block_count()` is the complementary count of
  escaping graphic-rendering blocks (zero exactly when the boolean is
  `true`). Edge sums widen to `u32` so a placement at the 65 535
  coordinate ceiling cannot wrap. §24 Comment / §26 Application
  Extensions have no placement and never contribute; a stream with no
  graphic-rendering block conforms vacuously.

- `GifImage::optimize_frame_rects()` — encoder-side inter-frame rect
  optimisation, the §20-placement companion to
  `optimize_color_tables()`. Re-runs the §23 disposal-method state
  machine over the block list and crops every §20 Image frame to the
  bounding rectangle of the pixels it actually changes on the composed
  logical screen: §20.c.ii–v give each Image its own `(left, top,
  width, height)` placement, so pixels outside the rectangle are
  simply never overwritten and the prior canvas shows through.
  Shrinks the §22 pixel payload (and the Appendix F LZW stream with
  it) while keeping `compose()` / `Playback` output byte-identical.
  Eligibility follows §23.c.iv: disposal values 0/1 (no disposal / do
  not dispose) are rect-independent and croppable; value 3 (restore to
  previous) is also safe — the pixels a cropped frame no longer
  overwrites already equal the pre-render canvas the disposal
  restores; value 2 (restore to background) is never cropped because
  "the area used by the graphic must be restored to the background
  color" and shrinking the rect would shrink the cleared region.
  §23.c.viii transparent pixels ("the corresponding pixel of the
  display device is not modified") and pixels re-drawing the colour
  already on the canvas count as unchanged; an exact-duplicate frame
  shrinks to a 1×1 rect at its original top-left (§20 has no zero-area
  image). Plan-then-apply: a stream that doesn't compose (placement
  escaping the §18 screen, missing palette, out-of-range index) is
  left completely unmodified, and the pass is idempotent (second call
  returns `0`). New `tests/optimize_frame_rects.rs` (8 tests) pins the
  changed-patch crop + encoded-size win + re-decode round-trip, the
  duplicate-frame 1×1 crop, the RestoreBackground exclusion, the
  RestorePrevious crop, the transparent-overlay opaque-bbox crop, §25
  Plain Text blocks participating in the state machine but never being
  modified, the non-composing-stream no-op, and a 24-seed randomized
  compose-equivalence + idempotency + size-monotonicity property. The
  end-to-end `decode` fuzz harness now *asserts* `compose(before) ==
  compose(after)` through `optimize_frame_rects` on every decodable
  input (219 k executions in 60 s locally, crash-free). Round 280.
- `GifImage::frame_transparent_indices()`, `transparent_index_count()`,
  `uses_transparent_index()`, and `all_frames_transparent()` —
  stream-level §23.c.viii Transparent Index roll-up, the
  transparent-index-side companion to the §23.c.iv Disposal Method
  family from round 266. `frame_transparent_indices()` yields the
  `Option<u8>` Transparent Index per graphic-rendering block (§20 Image
  **and** §25 Plain Text — both carry a §23-attachable GCE per §23.d)
  in source order; a block whose GCE leaves the §23.c.vi Transparency
  Flag clear (value `0`, "Transparent Index is not given") or carries
  no GCE at all contributes `None`, exactly the "no transparency for
  this block" case since §23.c.viii makes the index "present if and
  only if the Transparency Flag is set to 1".
  `transparent_index_count()` is the count of blocks that *do* give an
  index — `count == frame_count()` flags a stream where every frame
  reserves a transparent slot, a strict mid-range value flags a mixed
  stream. `uses_transparent_index(index)` is the per-slot any-block
  query (§21.a precedence: the index addresses the active table, LCT
  superseding GCT) so a palette-optimisation pass can check whether a
  slot is ever treated as transparent before reclaiming it.
  `all_frames_transparent()` is the every-block form, vacuously `true`
  for a metadata-only stream, matching the shape of
  `all_frames_use_disposal()` / `all_frames_interlaced()`. §24 Comment
  / §26 Application metadata blocks produce no rendered output and carry
  no Transparent Index; they are skipped, matching the
  `frame_delays()` / `frame_disposals()` / `has_transparency()` spine.
  Four new unit tests pin the source-order spine across §20 + §25
  blocks (no-GCE and flag-clear both mapping to `None`), the
  count-only-flagged mapping, the specific-slot match vs the opaque
  miss, the every-block / vacuously-true semantics, and the
  has_transparency / count cross-check. Total unit tests 215 → 219.
  Round 273.
- `GifImage::frame_disposals()`, `requires_canvas_snapshot()`,
  `uses_disposal()`, and `all_frames_use_disposal()` — stream-level
  §23.c.iv Disposal Method roll-up. `frame_disposals()` yields the
  `DisposalMethod` per graphic-rendering block (§20 Image **and** §25
  Plain Text — both carry §23-attachable Disposal Method per §23.d)
  in source order; a block with no attached GCE contributes
  `DisposalMethod::None` per §23.c.iv value `0` "No disposal specified"
  (the spec's "decoder is not required to take any action" default).
  `requires_canvas_snapshot()` is `true` when any block selects the
  §23.c.iv `RestorePrevious` (`3`) mode — the §23.e.i mode that
  "imposes severe demands on the decoder to store the section of the
  graphic that needs to be saved" — so a renderer can skip
  pre-allocating the snapshot buffer for streams that never use it
  rather than walking every frame's `GraphicControl::disposal`.
  `uses_disposal(method)` and `all_frames_use_disposal(method)` are
  the any-block / every-block queries; both treat the no-GCE case as
  `DisposalMethod::None`, and the every-block form is vacuously `true`
  for a zero-graphic-rendering-block stream, matching the shape of
  `all_frames_interlaced()` and `all_frames_palettes_sorted()`. §24
  Comment / §26 Application metadata blocks produce no rendered output
  and so carry no Disposal Method; they are skipped, matching the
  spine shared with `frame_delays()` / `has_transparency()` /
  `requires_user_input()`. Five new unit tests pin the source-order
  spine across §20 + §25 blocks, the no-GCE-counts-as-None mapping, the
  RestorePrevious-anywhere `requires_canvas_snapshot` semantics, the
  any-block / every-block / vacuously-true forms, and the
  `frame_disposals` vs `frames_with_graphic_control` cross-check.
  Total unit tests 210 → 215. Round 266.
- `Frame::local_color_table_size_field()` and
  `Frame::local_color_table_entry_count()` — typed accessors for the
  §20.c.ix "Size of Local Color Table" 3-bit field and its `2^(N+1)`
  on-disk entry count. Per §20.c.ix the field stores the smallest `N`
  in `0..=7` such that `2^(N+1)` is ≥ the LCT entry count; the §21 LCT
  then carries `3 × 2^(N+1)` bytes on disk. A 2-entry LCT encodes as
  `0`; a 256-entry LCT pins the field at `7`; mid-range counts round
  up — a 5-entry LCT slots into the 8-entry slot and encodes as `2`,
  matching the §18.c.vi rounding the encoder already applies. Both
  accessors return `None` when no LCT is attached (§20.c.vi flag clear,
  §20.c.ix undefined) so a caller never confuses the "absent LCT" case
  with a real "2-entry LCT" (encoded field `0`). Paired stream-level
  rollups: `GifImage::frames_with_local_color_table_size()` and
  `GifImage::frames_with_local_color_table_entry_count()` iterate every
  §20 Image block paired with its §20.c.ix field value / on-disk entry
  count in source order; `GifImage::max_local_color_table_size_field()`
  returns the largest §20.c.ix field across every LCT-carrying §20
  Image (`None` when no §20 frame attaches an LCT), so a decoder
  allocating a reusable scratch LCT buffer can size it once up front at
  `2^(max + 1)` entries rather than re-allocating per-frame. Only
  `Block::Image` entries contribute (§24 Comment / §25 Plain Text / §26
  Application carry no §20.c.ix at all). Nine new unit tests pin the
  round-up table across the full `1..=256` palette range, the
  None-without-LCT contract, the source-order pairing, the largest
  pick, non-image-block skipping, and the round-trip
  `entry_count == 1 << (size_field + 1)` consistency. Total unit tests
  201 → 210. Round 249.
- `GifImage::interlaced_frame_count()`, `has_interlaced_frames()`, and
  `all_frames_interlaced()` — stream-level §20.c.vii Interlace Flag
  queries. Counterpart to the §18.c.v / §20.c.viii Sort Flag accessors
  from round 246. Per §20.c.vii the Interlace Flag is a per-image
  property (a single stream may mix interlaced and progressive
  frames); the decoder already presents every frame de-interlaced into
  a row-major raster regardless, but `Frame::interlaced` preserves the
  on-disk flag for round-trip. The three new stream-level accessors
  roll that bit up so a renderer that wants to know whether the stream
  relies on the Appendix E four-pass row reordering — for example, to
  enable progressive-display of partial decoded data — can gate on a
  single query rather than walking every `Frame::interlaced`. Only
  `Block::Image` entries contribute (§24 Comment / §25 Plain Text /
  §26 Application have no Interlace Flag at all);
  `all_frames_interlaced()` is vacuously `true` for a zero-frame
  stream per `Iterator::all`'s empty-input contract, matching
  `all_frames_palettes_sorted()`'s shape. Nine new unit tests pin the
  counts-image-only / metadata-only-stream / all-progressive /
  any-interlaced / vacuous-all / mixed-progressive-interlaced /
  skip-Comment-and-PlainText-blocks semantics. Total unit tests
  191 → 200. Round 247.
- `GifImage::plain_texts()` — stream-level typed iterator yielding each
  §25 Plain Text Extension paired with its attached §23 Graphic Control
  Extension (`(&PlainText, Option<GraphicControl>)`) in source order.
  The §25 companion to `frames_with_graphic_control()`: where that
  accessor surfaces the §20 Image side of the §23.d "this block can
  modify the Image Descriptor Block and the Plain Text Extension"
  attachment, `plain_texts()` surfaces the §25 side, so callers walking
  "every Plain Text block and the GCE that controls it" don't have to
  re-derive the §23 → §25 pairing from `GifImage::blocks`. The shared
  timing / rendering-flag spine (`frame_delays()` /
  `has_transparency()` / `requires_user_input()`) already walks both
  §20 and §25 graphic-rendering blocks together; the new accessor is
  the §25-only typed entry point. Paired with two §25.e recommendation
  queries: `plain_texts_are_printable()` reports whether every payload
  byte of every §25 block sits in §25.e's recommended `0x20..=0xF7`
  range (anything outside would be substituted with a Space by a §25.e
  conforming renderer); `plain_texts_grid_fits_cells()` reports
  whether every §25.c text grid is an integer number of character
  cells across and down (`width % cell_width == 0` and
  `height % cell_height == 0`, with a zero cell dimension treated as
  non-conforming since it collapses the §25 grid layout entirely) so
  no glyph would be silently cropped at the right or bottom edge.
  Both queries treat §25.e as a *recommendation* — the encoder itself
  never enforces it — so a strict authoring pipeline can gate emission
  on them while the decoder continues to accept conforming and
  non-conforming streams alike. Ten new unit tests pin the
  pairing-with-GCE / zero-PlainText / printable-edge-0x20-0xF7 /
  below-range-0x1F / above-range-0xF8 / vacuous-printable / integer-
  cell-fit / fractional-width / fractional-height / zero-cell-dim /
  vacuous-grid-fit / GCE-pairing-matches-`frame_delays` semantics.
  Total unit tests 181 → 191. Round 236.
- `lzw::LzwEncoder` reusable-state encoder. Holds the Appendix F
  `(prefix_code, next_byte) → code` dictionary across many
  `encode_frame` calls so the ~2 MiB zero-init lands once at
  construction instead of once per frame. The per-frame reset walks
  an explicit `touched_keys` log (≤ 4094 entries; bounded by
  `MAX_TABLE_SIZE - first_entry`) and clears just the slots that
  were written rather than memsetting the whole table. Output is
  byte-identical to the stateless `lzw::encode` free function — same
  Clear emission, same width-bump rule, same EOI emission — pinned
  by `lzw_encoder_matches_free_function_byte_for_byte` over the
  spec corners (empty raster, hand-derived 4-colour fixture, 8-bit
  table-overflow stress, KwKwK monotone run, 16-colour width-bump
  stairstep). Four further unit tests pin the reset-between-frames
  state hygiene (`lzw_encoder_resets_dictionary_between_frames`),
  the error-path reset that keeps a hostile input from corrupting
  subsequent frames (`lzw_encoder_recovers_dictionary_after_error`),
  the saturated-then-tiny-frame transition through the deferred-
  clear regime (`lzw_encoder_resets_after_dictionary_overflow`),
  and the `Default` equivalence (`lzw_encoder_default_matches_new`).
  A dedicated `bench_lzw_encoder_reuse_anim_100x_64x64` Criterion
  scenario threads 100 frames through a single `LzwEncoder` and
  measures ~1.37 ms / 285 MiB/s versus ~2.44 ms / 160 MiB/s for the
  free-function path — a ~44 % wall-time drop on the call-out
  scenario `BENCHMARKS.md` flagged after round 194 as "useful
  baseline for any future amortise-the-encode-dictionary-across-
  frames optimisation". The free function path is unchanged
  (within noise of the round-194 baseline on `16x16` / `256x256` /
  `1024x1024` / `anim/100x_64x64`). LZW unit tests 13 → 18.
  Round 230.
- `GifImage::has_sorted_global_palette()` /
  `GifImage::frames_with_sorted_palette()` /
  `GifImage::all_frames_palettes_sorted()` — three accessors over the
  §18.c.v Global Color Table Sort Flag and the §20.c.viii Local Color
  Table Sort Flag. Both spec sections define the Sort Flag identically
  ("Ordered by decreasing importance, most important color first") so
  that a palette-display-constrained decoder "may use an initial
  segment of the table to render the graphic." The decoder already
  parsed the flag into [`GifImage::global_palette_sorted`] /
  [`Frame::palette_sorted`]; these accessors surface it at the
  semantic level a renderer cares about — *is this active palette
  truncatable in place?* `has_sorted_global_palette()` is the
  GCT-level query, conservative on §18.c.iii (returns `false` when no
  GCT is attached, since §18.c.v is undefined in that case).
  `frames_with_sorted_palette()` is the per-frame iterator: yields
  `(&Frame, Option<&[Rgb]>, bool)` following the same §21.a precedence
  as `frames_with_palette()` (LCT supersedes GCT for both the palette
  *and* its Sort Flag), and reports `(_, None, false)` for the §13 /
  §21 no-active-table fallback. `all_frames_palettes_sorted()`
  short-circuits the stream-level question in a single query (vacuous
  `true` for a zero-frame stream; `false` the moment any active palette
  is missing or has its Sort Flag clear). Thirteen new unit tests pin
  the GCT-present-and-sorted, no-GCT-with-flag-set, GCT-absent,
  LCT-supersedes-GCT-sort, GCT-fallback, no-active-table,
  LCT-clear-overrides-sorted-GCT, non-Image-block skip, mixed-LCT-GCT
  all-sorted, single-unsorted-flip, zero-frame-vacuous,
  no-active-palette-flips-false, and frame-handle-extends-
  `frames_with_palette` semantics. Total unit tests 163 → 176. Round
  224.
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

### Fixed

- §23.d / §23.c.viii — a §23 Graphic Control Extension attached to a
  §25 Plain Text block now applies its Transparency Index to plain-text
  rendering, where previously it was ignored. §23.d states the GCE "can
  modify ... the Plain Text Extension", and §23.c.viii says a pixel
  whose index equals the Transparency Index "is not modified" on the
  display device. A plain-text cell foreground or background pixel
  whose Global-Color-Table index matches the transparency index is now
  skipped (the prior canvas shows through) in `compose()`, the lazy
  `Playback` iterator, and the `optimize_frame_rects()` crop-planning
  walk — matching how §20 image transparency already behaved. Four new
  unit tests (transparent background / transparent foreground /
  non-matching-index opaque, plus an eager-vs-lazy parity check) pin
  the three cases. Round 289.

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
