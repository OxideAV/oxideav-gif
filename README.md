# oxideav-gif

Pure-Rust **GIF** image + animation codec and container — GIF87a /
GIF89a decode + encode, variable-width LZW (2–12 bit), all disposal
modes, transparency, NETSCAPE2.0 loop extension.

Zero C dependencies, no FFI, no `*-sys` crates.

Originally part of the [oxideav](https://github.com/KarpelesLab/oxideav)
framework; extracted to its own crate for independent publication.

## Usage

```toml
[dependencies]
oxideav-gif = "0.0.3"
```

Plugs into both the [`oxideav-codec`](https://crates.io/crates/oxideav-codec)
codec registry and the
[`oxideav-container`](https://crates.io/crates/oxideav-container)
container registry:

```rust
let mut codecs = oxideav_codec::CodecRegistry::new();
let mut containers = oxideav_container::ContainerRegistry::new();
oxideav_gif::register_codecs(&mut codecs);
oxideav_gif::register_containers(&mut containers);
```

Codec id: `"gif"`; accepted pixel format is `Pal8`. Non-Pal8 input to
the encoder triggers a palette conversion via
[`oxideav-pixfmt`](https://crates.io/crates/oxideav-pixfmt).

Single-image GIFs decode to one `VideoFrame`; animated GIFs to N frames
with PTS in centiseconds (the GIF native unit).

## License

MIT — see [LICENSE](LICENSE).
