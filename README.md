# oxideav-gif

Pure-Rust decoder and encoder for the GIF87a and GIF89a image formats.

## Status

Implements every block type defined by the CompuServe specifications:

- Header (§17), Logical Screen Descriptor (§18), Trailer (§27)
- Global / Local Color Tables (§19, §21)
- Image Descriptor + Table-Based Image Data (§20, §22)
- Variable-Length-Code LZW compression (Appendix F)
- Four-pass interlace transform (Appendix E)
- Graphic Control Extension (§23) — disposal method, user-input flag,
  transparent index, delay time
- Comment Extension (§24)
- Plain Text Extension (§25)
- Application Extension (§26)
- Multi-frame compositing onto the §18 Logical Screen using the §23
  disposal-method state machine. `compose()` returns the eager
  `Vec<ComposedFrame>`; `Playback::frames()` is the lazy iterator
  form (one canvas per call). Both cover all four defined disposal
  values (None / Keep / RestoreBackground / RestorePrevious) and the
  §23.c.viii transparent-index handling.
- Animation playback iterator `Playback::looping_frames()` that
  honours the NETSCAPE2.0 *Looping* sub-block: no extension plays one
  pass, `loop_count = 0` loops forever, `loop_count = N` plays
  `N + 1` total passes per the de-facto convention documented in
  `docs/image/gif/netscape2.0-loop-extension.md`. Each yielded
  `PlaybackFrame` carries its delay as a `Duration` for ergonomic
  `thread::sleep` calls.
- Structured views over the three ecosystem-defined Application
  Extensions (`app_ext` module) — NETSCAPE2.0 looping +
  buffering sub-blocks, the Adobe XMP packet (`XMP Data`), and ICC
  colour profile (`ICCRGBG1`). `GifImage::loop_count()` /
  `xmp_packet()` / `icc_profile()` are convenience accessors over
  the same raw `Block::Application` data, which stays in
  `GifImage::blocks` for byte-stable round-trip.

## Not implemented

- Other vendor Application Extensions (EXIF, ANIMEXTS1.0, etc.) —
  these stay surfaced as raw `Block::Application` data with the
  identifier, authentication code, and concatenated payload bytes.
- Plain Text Extension glyph rendering — the spec leaves font choice to
  the decoder, so `compose` treats Plain Text blocks as no-ops on the
  canvas.

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
