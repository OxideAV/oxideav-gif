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

## Not implemented

- Frame compositing onto a virtual screen using the disposal-method
  semantics. The graphic-control extension is preserved verbatim and
  attached to the appropriate frame; combining successive frames into
  a final pixel buffer is left to a higher layer.
- Interpretation of higher-level Application Extensions (loop control,
  XMP, ICC, EXIF, etc.) — these are exposed as raw `Application`
  blocks with their identifier, authentication code, and concatenated
  payload bytes.

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
