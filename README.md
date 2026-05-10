# oxideav-gif

[![Crates.io](https://img.shields.io/crates/v/oxideav-gif.svg)](https://crates.io/crates/oxideav-gif)
[![docs.rs](https://docs.rs/oxideav-gif/badge.svg)](https://docs.rs/oxideav-gif)
[![CI](https://github.com/OxideAV/oxideav-gif/actions/workflows/ci.yml/badge.svg)](https://github.com/OxideAV/oxideav-gif/actions/workflows/ci.yml)

Pure-Rust GIF (87a / 89a) decoder + encoder, hand-written from
CompuServe's *Graphics Interchange Format Version 89a* (July 1990) and
the original *Graphics Interchange Format* (June 1987) specs.

No third-party GIF library code (giflib, libnsgif, Pillow, etc.) was
read or referenced during implementation — the codec was written from
the spec PDFs alone, with cross-validation against `magick` and
`giftext` as black-box subprocess validators.

## What's implemented

* File header (§17), Logical Screen Descriptor (§18),
  Global / Local Color Tables (§19, §21).
* Image Descriptor (§20) + Table-Based Image Data (§22) — including
  the four-pass interlace order (§E).
* Variable-Length-Code LZW compression and decompression
  (Appendix F) with the deferred-clear-code rule from the cover
  sheet.
* Graphic Control (§23), Comment (§24), Plain Text (§25), and
  Application (§26) extensions.
* Trailer (§27).
* Multi-image / animation files: all `<Table-Based Image>` blocks in
  the data stream are surfaced through `GifImage::frames`. A naive
  per-frame compositor lives at `GifImage::composite_frame_rgba` and
  respects transparency + Disposal Method.

## Not (yet) implemented

* Real-time animation playback machinery — the per-frame data and the
  `composite_frame_rgba` helper are exposed, but there's no render
  loop.
* NETSCAPE2.0 looping extension parsing as a structured concept —
  it's preserved verbatim through `GifBlock::Application` for callers
  that want to interpret it (the byte container is spec'd at §26;
  the loop-count payload semantics are not).
* On-line Capabilities Dialogue (§G) — that's an interactive
  protocol layer, not part of any GIF file.

## Standalone vs registry-integrated

`oxideav-core` is gated behind the default-on `registry` Cargo
feature. Image-library consumers can depend on the crate with
`default-features = false` to get a `decode_gif` / `encode_gif` API
plus crate-local `GifImage` / `GifError` types and never reference
`oxideav-core`.

```toml
# In an oxideav-framework consumer (default):
oxideav-gif = "0.0.10"

# In a standalone image-library consumer:
oxideav-gif = { version = "0.0.10", default-features = false }
```

## Spec references

The implementation cites section numbers from the W3C-mirrored
CompuServe spec at every block's parse / emit site. Fetched copies
of the spec text live in
`oxideav-workspace/docs/image/gif/{gif87a-spec.txt,gif89a-spec.txt}`.

The LZW state-machine implementation references the
**Welch (1984)** paper *A Technique for High-Performance Data
Compression* (IEEE Computer 17(6)) for the algorithm framing — the
GIF spec itself describes only the GIF-specific deviations
(§F items 1–4) and the bit packing layout, not the underlying
dictionary algorithm.

## License

Apache-2.0. The spec text grants royalty-free use of the GIF format
in computer software; the codec implementation is original work
licensed under Apache-2.0.
