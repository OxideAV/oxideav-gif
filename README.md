# oxideav-gif

Pure-Rust **GIF** image + animation codec and container — GIF87a /
GIF89a decode + encode, variable-width LZW (2–12 bit), all disposal
modes, transparency, NETSCAPE2.0 loop extension. Zero C dependencies.

Part of the [oxideav](https://github.com/OxideAV/oxideav-workspace)
framework but usable standalone.

## Installation

```toml
[dependencies]
oxideav-core = "0.1"
oxideav-codec = "0.1"
oxideav-container = "0.0"
oxideav-gif = "0.0"
```

## Quick use

GIFs carry multiple frames, so the typical path is: open the file as
a container, pull packets, decode them. Every frame's accepted pixel
format is `Pal8`.

```rust
use oxideav_codec::CodecRegistry;
use oxideav_container::ContainerRegistry;
use oxideav_core::{Frame, MediaType};

let mut codecs = CodecRegistry::new();
let mut containers = ContainerRegistry::new();
oxideav_gif::register_codecs(&mut codecs);
oxideav_gif::register_containers(&mut containers);

// Read the whole .gif into memory (or wrap any ReadSeek source).
let input: Box<dyn oxideav_container::ReadSeek> = Box::new(
    std::io::Cursor::new(std::fs::read("anim.gif")?),
);
let mut dmx = containers.open("gif", input)?;
let stream = &dmx.streams()[0];
let mut dec = codecs.make_decoder(&stream.params)?;

loop {
    match dmx.next_packet() {
        Ok(pkt) => {
            dec.send_packet(&pkt)?;
            while let Ok(Frame::Video(vf)) = dec.receive_frame() {
                // vf.format == PixelFormat::Pal8
                // vf.planes[0].data is the index plane (width × height bytes).
                // vf.width / vf.height describe the canvas.
            }
        }
        Err(oxideav_core::Error::Eof) => break,
        Err(e) => return Err(e.into()),
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Encoder

```rust
let mut params = CodecParameters::video(CodecId::new("gif"));
params.width = Some(w);
params.height = Some(h);
params.pixel_format = Some(PixelFormat::Pal8);
let mut enc = codecs.make_encoder(&params)?;
enc.send_frame(&Frame::Video(frame_pal8))?;
let pkt = enc.receive_packet()?;
```

Non-Pal8 input (e.g. RGBA) is handled via
[`oxideav-pixfmt`](https://crates.io/crates/oxideav-pixfmt)'s
`generate_palette` + `convert(..., Pal8, ...)`. In the full job-graph
runtime this conversion is auto-inserted; standalone, you do it
explicitly before `send_frame`.

### Per-frame disposal / transparency

The generic `Encoder` trait only carries pixel data and pts, so GIF's
per-frame metadata (disposal method, transparent colour index) has to be
set on the concrete [`GifEncoder`] type just before `send_frame`:

```rust
use oxideav_gif::GifEncoder;
let mut enc = GifEncoder::new(&params)?;
enc.set_next_disposal(2);                 // restore-to-background
enc.set_next_transparent_index(Some(7));  // palette index 7 is transparent
enc.send_frame(&Frame::Video(frame))?;    // hints consumed + reset
```

Both hints apply to the next frame only and reset to their defaults
(`0` / `None`) afterwards. The `make_encoder` path (used by the registry
and job-graph runtime) always emits disposal = 0 and no transparent
index — adequate for "paint every frame in full over a static canvas",
which is the common animation shape.

### Codec / container IDs

- Codec: `"gif"`; accepted pixel format `Pal8`.
- Container: `"gif"`, matches `.gif` by extension + magic bytes.

Single-image GIFs decode to one `VideoFrame`; animated GIFs to N
frames with PTS in centiseconds (the GIF native unit).

## License

MIT — see [LICENSE](LICENSE).
