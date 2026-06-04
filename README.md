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
- Image Descriptor + Table-Based Image Data (§20, §22)
- Variable-Length-Code LZW compression (Appendix F). The codec pair
  ships in two flavours: the stateless `lzw::encode` / `lzw::decode`
  free functions for one-shot calls, and `lzw::LzwEncoder` which
  reuses its `(prefix, next_byte) → code` dictionary across multiple
  `encode_frame` calls — the ~2 MiB zero-init lands once at
  construction, each subsequent frame walks a `touched_keys` log
  (≤ 4094 entries) rather than memsetting the whole table. Output
  is byte-identical to the free function; reuse cuts the
  100×(64×64) animation-encoder microbench by ~44 % (round 230,
  see `BENCHMARKS.md`).
- Four-pass interlace transform (Appendix E)
- Graphic Control Extension (§23) — disposal method, user-input flag,
  transparent index, delay time
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
  §25.e fallback rule.
- Application Extension (§26)
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
  ANIMEXTS1.0 when NETSCAPE2.0 is absent.
- Encoder Global vs Local Color Table optimisation
  (`GifImage::optimize_color_tables`) — when every image frame
  carries the same palette, hoists it into the §18 Global Color
  Table and clears the now-redundant §21 Local Color Tables, saving
  `3 × 2^(size_bits + 1)` bytes per frame. Pixels are unaffected
  (§21 says a frame with the LCT flag clear uses the §18 GCT).
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

## Fuzzing

`fuzz/fuzz_targets/` ships six `cargo-fuzz` harnesses, all asserting
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
  encode-then-re-decode through every fuzz-derived `GifImage`. Caps the
  composited canvas at 1 Mpx and the looping iterator at 64 frames so a
  `loop_count = Some(0)` (forever) stream doesn't pin the fuzzer.
- `encode` — end-to-end encode-side harness: derives a `GifImage` from
  fuzz bytes via `AnimationBuilder` (rect placement, palette size,
  per-frame disposal, NETSCAPE2.0 / loop-forever behaviour), then
  drives `encode` → `decode` → `decode_lenient` → `decode_first_frame`
  → `compose` → `Playback::frames` / `looping_frames` on the result.
  Reaches encoder configurations the decoder-output-only harness
  can't construct (sub-screen placements, mismatched palette sizes,
  multi-frame disposal sequences). Caps screen at 256×256 and frame
  count at 16.
- `plain_text` (round 200) — dedicated §25 Plain Text Extension
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
no-op render short-circuit. Bootstrap a fresh corpus with
`cp -n fuzz/seed_corpus/<target>/* fuzz/corpus/<target>/`. Seeds are
content-addressed by SHA-1 and regenerable via `tools/seedgen.py`
(pure-Python, no GIF library invoked).

Latest local baseline: the end-to-end `decode` target cleared 345 k
executions in 60 s, `decode_lenient_panic_free` cleared 16 M, and
`encode` cleared 256 k — all crash-free. (The `encode` run followed a
fix to a divide-by-zero in the harness's background-index reduction that
the daily scheduled run had flagged; the bug was in the fuzz target, not
the codec.)

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
validation pass in isolation. The fourth (`lzw`, round 194) measures
the `lzw::encode` / `lzw::decode` pair in isolation across four
sizes — see [`BENCHMARKS.md`](BENCHMARKS.md) for the scenario matrix
and the round-194 baseline numbers. Future LZW /
sub-block-sizing / disposal-state-machine optimisation rounds use
these baselines for A/B comparisons.

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
