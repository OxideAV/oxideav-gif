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
  square pixels = raw 49).
- Global / Local Color Tables (§19, §21)
- Image Descriptor + Table-Based Image Data (§20, §22)
- Variable-Length-Code LZW compression (Appendix F)
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
  graphic-rendering block too).
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
