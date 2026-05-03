//! End-to-end cross-codec round-trip test against giflib.
//!
//! Pipeline:
//!
//! 1. Build a deterministic random 640x480 Pal8 frame (random palette,
//!    random per-pixel index, ≤256 colours).
//! 2. Encode it with oxideav-gif → GIF byte string A.
//! 3. Decode A with giflib → palette + indices → resolve to per-pixel RGB.
//! 4. Re-encode those indices + palette with giflib → GIF byte string B.
//! 5. Decode B with oxideav-gif → final Pal8 frame.
//! 6. Assert that the per-pixel RGB resolved via the FINAL frame's palette
//!    matches the per-pixel RGB of the ORIGINAL input.
//!
//! Single frame, no animation. RGB-equality (not byte-equality) because
//! palette ordering and padding differ between giflib and oxideav.
//!
//! giflib is loaded via `dlopen` at first call. The test silently skips
//! when no giflib shared library is installed (matches the cross-decode
//! fuzz harness behaviour). Install it with `brew install giflib`
//! (macOS) or `apt install libgif-dev` (Debian/Ubuntu).
//!
//! NOTE: the giflib shim below is deliberately inlined as a private
//! module — we don't dev-depend on the fuzz crate. The TLS-callback I/O
//! pattern is mirrored from `fuzz/src/lib.rs` because giflib's
//! `GifFileType.UserData` field offset drifts between 5.x and 6.x, so
//! we route both the write and the read callbacks through thread-local
//! storage instead of touching the struct tail.

#![allow(unsafe_code)]

mod common;

use std::io::Cursor;

use oxideav_core::{
    CodecId, CodecParameters, CodecRegistry, ContainerRegistry, Frame, MediaType, PixelFormat,
    StreamInfo, TimeBase, VideoFrame, VideoPlane, WriteSeek,
};
use oxideav_gif::{register_codecs, register_containers, GIF_CODEC_ID};

use common::SharedSink;

const W: u32 = 640;
const H: u32 = 480;
const N_COLORS: usize = 256;

/// Tiny deterministic LCG so the test image is reproducible without
/// pulling `rand` into dev-dependencies. Constants are the Numerical
/// Recipes pair; the generator is fine for synthesising a fuzz-shaped
/// fixture but not for anything cryptographic.
struct Lcg(u32);

impl Lcg {
    fn new(seed: u32) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B1) ^ 0xDEAD_BEEF)
    }
    fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0
    }
    fn next_u8(&mut self) -> u8 {
        (self.next_u32() >> 24) as u8
    }
}

fn build_random_palette(n: usize, rng: &mut Lcg) -> Vec<[u8; 4]> {
    (0..n)
        .map(|_| [rng.next_u8(), rng.next_u8(), rng.next_u8(), 0xFF])
        .collect()
}

fn build_random_indices(w: u32, h: u32, n_colors: usize, rng: &mut Lcg) -> Vec<u8> {
    let total = (w as usize) * (h as usize);
    let mut out = Vec::with_capacity(total);
    for _ in 0..total {
        out.push((rng.next_u32() as usize % n_colors) as u8);
    }
    out
}

fn build_pal8_frame(width: u32, _height: u32, indices: &[u8], palette: &[[u8; 4]]) -> VideoFrame {
    let mut palette_plane = Vec::with_capacity(256 * 4);
    for i in 0..256 {
        if i < palette.len() {
            palette_plane.extend_from_slice(&palette[i]);
        } else {
            palette_plane.extend_from_slice(&[0, 0, 0, 0xFF]);
        }
    }
    VideoFrame {
        pts: Some(0),
        planes: vec![
            VideoPlane {
                stride: width as usize,
                data: indices.to_vec(),
            },
            VideoPlane {
                stride: 256 * 4,
                data: palette_plane,
            },
        ],
    }
}

fn build_params(width: u32, height: u32) -> CodecParameters {
    let mut p = CodecParameters::video(CodecId::new(GIF_CODEC_ID));
    p.media_type = MediaType::Video;
    p.width = Some(width);
    p.height = Some(height);
    p.pixel_format = Some(PixelFormat::Pal8);
    p
}

/// Encode an in-memory Pal8 frame through oxideav-gif (encoder + muxer).
/// Returns the on-disk GIF89a byte string.
fn oxideav_encode(frame_in: VideoFrame, w: u32, h: u32) -> Vec<u8> {
    let mut codecs = CodecRegistry::new();
    register_codecs(&mut codecs);
    let mut containers = ContainerRegistry::new();
    register_containers(&mut containers);

    let params_enc = build_params(w, h);
    let mut encoder = codecs.make_encoder(&params_enc).expect("make_encoder");
    encoder
        .send_frame(&Frame::Video(frame_in))
        .expect("send_frame");
    encoder.flush().expect("flush");
    let pkt = encoder.receive_packet().expect("receive_packet");
    let encoder_params = encoder.output_params().clone();

    let (sink, sink_data) = SharedSink::new();
    {
        let boxed: Box<dyn WriteSeek> = Box::new(sink);
        let si = StreamInfo {
            index: 0,
            time_base: TimeBase::new(1, 100),
            duration: None,
            start_time: Some(0),
            params: encoder_params,
        };
        let mut muxer = containers
            .open_muxer("gif", boxed, std::slice::from_ref(&si))
            .expect("open_muxer");
        muxer.write_header().expect("write_header");
        muxer.write_packet(&pkt).expect("write_packet");
        muxer.write_trailer().expect("write_trailer");
    }
    // Bind the lock guard to a local so the temporary `MutexGuard` is
    // dropped before `sink_data` itself is — otherwise the implicit
    // tail-expression lifetime extends the borrow past `sink_data`'s
    // scope and the borrow-checker rejects it (E0597).
    let out = sink_data.lock().unwrap().clone();
    out
}

/// Decode a GIF byte string through oxideav-gif (demuxer + decoder).
/// Returns the first frame's `(palette_plane, indices)`. The palette
/// plane is the raw second-plane bytes (256 RGBA quads = 1024 bytes);
/// indices is the raw first-plane bytes (`w * h`).
fn oxideav_decode(gif_bytes: &[u8]) -> (u32, u32, Vec<u8>, Vec<u8>) {
    let mut codecs = CodecRegistry::new();
    register_codecs(&mut codecs);
    let mut containers = ContainerRegistry::new();
    register_containers(&mut containers);

    let cursor = Cursor::new(gif_bytes.to_vec());
    let boxed: Box<dyn oxideav_core::ReadSeek> = Box::new(cursor);
    let mut demuxer = containers
        .open_demuxer("gif", boxed, &oxideav_core::NullCodecResolver)
        .expect("open_demuxer");
    let si = demuxer.streams()[0].clone();
    let w = si.params.width.expect("stream width");
    let h = si.params.height.expect("stream height");
    let mut decoder = codecs.make_decoder(&si.params).expect("make_decoder");
    let pkt = demuxer.next_packet().expect("next_packet");
    decoder.send_packet(&pkt).expect("send_packet");
    let frame = match decoder.receive_frame().expect("receive_frame") {
        Frame::Video(v) => v,
        _ => panic!("non-video frame from gif decoder"),
    };
    assert!(
        frame.planes.len() >= 2,
        "decoded GIF frame missing palette plane"
    );
    let indices = frame.planes[0].data.clone();
    let palette = frame.planes[1].data.clone();
    (w, h, palette, indices)
}

#[test]
fn external_roundtrip_oxideav_giflib_giflib_oxideav() {
    if !giflib::available() {
        eprintln!(
            "external_roundtrip: giflib shared library not found; skipping. \
             Install with `brew install giflib` (macOS) or \
             `apt install libgif-dev` (Debian/Ubuntu)."
        );
        return;
    }

    // ---- Step 0: build a deterministic random 640x480 / 256-colour input.
    let mut rng = Lcg::new(0xC0DE_F00D);
    let palette_in = build_random_palette(N_COLORS, &mut rng);
    let indices_in = build_random_indices(W, H, N_COLORS, &mut rng);
    assert_eq!(indices_in.len(), (W as usize) * (H as usize));

    let frame_in = build_pal8_frame(W, H, &indices_in, &palette_in);

    // ---- Step 1: oxideav encode -> GIF bytes A.
    let gif_a = oxideav_encode(frame_in, W, H);
    assert!(gif_a.starts_with(b"GIF89a"), "stage A is not a GIF89a");
    assert_eq!(
        gif_a.last().copied(),
        Some(0x3B),
        "stage A missing GIF trailer"
    );

    // ---- Step 2: giflib decode A -> indices + palette (raw).
    let dec_a = giflib::decode_to_indexed(&gif_a).expect("giflib decode of stage A");
    assert_eq!(dec_a.width, W);
    assert_eq!(dec_a.height, H);
    assert_eq!(
        dec_a.indices.len(),
        (W as usize) * (H as usize),
        "giflib decode produced wrong-sized index plane"
    );

    // Sanity: stage A round-trips RGB-exact through giflib alone (i.e.
    // every pixel in giflib's view matches the input). This also catches
    // the case where giflib's palette padding shifts our indices.
    for i in 0..indices_in.len() {
        let in_rgb = &palette_in[indices_in[i] as usize][..3];
        let mid_idx = dec_a.indices[i] as usize;
        assert!(
            mid_idx < dec_a.palette_rgb.len(),
            "giflib decode index {} exceeds palette length {}",
            mid_idx,
            dec_a.palette_rgb.len()
        );
        let mid_rgb = &dec_a.palette_rgb[mid_idx];
        assert_eq!(
            &mid_rgb[..],
            in_rgb,
            "stage A pixel {} RGB drifted: input={:?} giflib={:?}",
            i,
            in_rgb,
            mid_rgb
        );
    }

    // ---- Step 3: giflib encode -> GIF bytes B.
    let gif_b = giflib::encode_indexed(
        &dec_a.indices,
        dec_a.width,
        dec_a.height,
        &dec_a.palette_rgb,
    )
    .expect("giflib encode of stage B");
    assert!(gif_b.starts_with(b"GIF"), "stage B is not a GIF");
    assert_eq!(
        gif_b.last().copied(),
        Some(0x3B),
        "stage B missing GIF trailer"
    );

    // ---- Step 4: oxideav decode B -> final indices + palette.
    let (out_w, out_h, palette_out_plane, indices_out) = oxideav_decode(&gif_b);
    assert_eq!(out_w, W, "stage D width drifted");
    assert_eq!(out_h, H, "stage D height drifted");
    assert_eq!(
        indices_out.len(),
        indices_in.len(),
        "stage D index plane size drifted"
    );

    // Lift oxideav's palette plane (256 RGBA quads, padded with 0/0/0/FF
    // beyond the actual table) into a [u8; 3]-per-entry view so we can
    // resolve `palette[indices[i]]` to RGB and compare against the
    // original input. Padding the lookup table to 256 entries means we
    // never index out-of-range even if oxideav reports a shorter usable
    // palette.
    assert_eq!(
        palette_out_plane.len(),
        256 * 4,
        "oxideav palette plane is not 256 RGBA quads"
    );
    let palette_out: Vec<[u8; 3]> = (0..256)
        .map(|i| {
            let off = i * 4;
            [
                palette_out_plane[off],
                palette_out_plane[off + 1],
                palette_out_plane[off + 2],
            ]
        })
        .collect();

    // ---- Step 5: assert pixel-equal RGB through both palettes.
    for i in 0..indices_in.len() {
        let in_rgb = &palette_in[indices_in[i] as usize][..3];
        let out_idx = indices_out[i] as usize;
        let out_rgb = &palette_out[out_idx];
        assert_eq!(
            &out_rgb[..],
            in_rgb,
            "final pixel {} RGB mismatch: input={:?} (idx {}) output={:?} (idx {})",
            i,
            in_rgb,
            indices_in[i],
            out_rgb,
            indices_out[i]
        );
    }
}

// ---- Inlined giflib `dlopen` shim ------------------------------------
//
// Mirrors `fuzz/src/lib.rs`. We don't dev-depend on the fuzz crate, so
// the shim is duplicated here verbatim with one small extension:
// `decode_to_indexed` returns the raw palette + indices (rather than the
// pre-resolved RGBA the fuzz harness needs). The four-stage round-trip
// needs the indices to re-feed giflib's encoder for stage B.

mod giflib {
    use libloading::{Library, Symbol};
    use std::cell::RefCell;
    use std::os::raw::{c_int, c_void};
    use std::sync::{Mutex, OnceLock};

    /// Conventional giflib shared-object names the loader will try in
    /// order. Covers macOS (`.dylib`), Linux (versioned + plain `.so`),
    /// and Windows (`.dll`).
    const CANDIDATES: &[&str] = &[
        "libgif.dylib",
        "libgif.7.dylib",
        "libgif.so.7",
        "libgif.so",
        "libgif-7.dll",
    ];

    fn lib() -> Option<&'static Library> {
        static LIB: OnceLock<Option<Library>> = OnceLock::new();
        LIB.get_or_init(|| {
            for name in CANDIDATES {
                // SAFETY: `Library::new` is documented as unsafe because
                // the loaded library may run code at load time. We
                // accept that risk for test tooling — giflib is a
                // well-behaved shared library.
                if let Ok(l) = unsafe { Library::new(name) } {
                    return Some(l);
                }
            }
            None
        })
        .as_ref()
    }

    /// True iff a giflib shared library was successfully loaded.
    pub fn available() -> bool {
        lib().is_some()
    }

    // ---- Shape mirrors of giflib's public structs --------------------
    //
    // We mirror only the fields/types we actually touch. giflib's
    // structs are stable across the 5.x / 6.x ABI for the front members
    // — the trailing `UserData` / `Private` fields shift, which is
    // exactly why we route I/O through TLS instead of `UserData`.

    #[repr(C)]
    struct GifColorType {
        red: u8,
        green: u8,
        blue: u8,
    }

    #[repr(C)]
    struct ColorMapObject {
        color_count: c_int,
        bits_per_pixel: c_int,
        sort_flag: bool,
        colors: *mut GifColorType,
    }

    #[repr(C)]
    struct GifImageDesc {
        left: c_int,
        top: c_int,
        width: c_int,
        height: c_int,
        interlace: bool,
        color_map: *mut ColorMapObject,
    }

    #[repr(C)]
    struct ExtensionBlock {
        _byte_count: c_int,
        _bytes: *mut u8,
        _function: c_int,
    }

    #[repr(C)]
    struct SavedImage {
        image_desc: GifImageDesc,
        raster_bits: *mut u8,
        _ext_block_count: c_int,
        _ext_blocks: *mut ExtensionBlock,
    }

    #[repr(C)]
    struct GifFileType {
        s_width: c_int,
        s_height: c_int,
        _s_color_resolution: c_int,
        _s_background_color: c_int,
        _aspect_byte: u8,
        s_color_map: *mut ColorMapObject,
        image_count: c_int,
        _image: GifImageDesc,
        saved_images: *mut SavedImage,
        // Trailing fields (extension blocks, error, user data, private)
        // are not touched here — see the module note above.
    }

    type OutputFunc = unsafe extern "C" fn(*mut GifFileType, *const u8, c_int) -> c_int;
    type InputFunc = unsafe extern "C" fn(*mut GifFileType, *mut u8, c_int) -> c_int;

    // ---- Encode side -------------------------------------------------

    struct EncodeBuf {
        data: Vec<u8>,
    }

    unsafe extern "C" fn write_callback(
        _gif: *mut GifFileType,
        bytes: *const u8,
        len: c_int,
    ) -> c_int {
        // SAFETY: ENCODE_TLS is set by `encode_indexed` to point at our
        // EncodeBuf for the duration of giflib's writes. UserData on
        // GifFileType could carry the same pointer, but reading it
        // requires hard-coding the struct tail's layout (which moves
        // between giflib 5.x and 6.x); the TLS path is layout-stable
        // and serialised by ENC_LOCK so two encodes can't trample each
        // other on the same thread.
        unsafe {
            let buf = ENCODE_TLS.with(|c| *c.borrow());
            if buf.is_null() {
                return 0;
            }
            let buf = &mut *(buf as *mut EncodeBuf);
            if len < 0 {
                return 0;
            }
            let n = len as usize;
            let slice = std::slice::from_raw_parts(bytes, n);
            buf.data.extend_from_slice(slice);
            len
        }
    }

    thread_local! {
        // Pointer to the EncodeBuf currently driving giflib's writeFunc.
        // Set inside `encode_indexed`, cleared on exit. Must NOT be
        // touched by other threads (each thread gets its own).
        static ENCODE_TLS: RefCell<*mut c_void> = const { RefCell::new(std::ptr::null_mut()) };
    }

    thread_local! {
        // Pointer to the current `&[u8]` slice being read by giflib's
        // readFunc. Cleared on exit. As above, single-threaded only.
        static DECODE_TLS: RefCell<*mut DecodeBuf> = const { RefCell::new(std::ptr::null_mut()) };
    }

    struct DecodeBuf {
        data: *const u8,
        len: usize,
        pos: usize,
    }

    unsafe extern "C" fn read_callback(_gif: *mut GifFileType, out: *mut u8, len: c_int) -> c_int {
        unsafe {
            let buf_ptr = DECODE_TLS.with(|c| *c.borrow());
            if buf_ptr.is_null() || len <= 0 {
                return 0;
            }
            let buf = &mut *buf_ptr;
            let want = len as usize;
            let avail = buf.len.saturating_sub(buf.pos);
            let n = want.min(avail);
            if n == 0 {
                return 0;
            }
            let src = buf.data.add(buf.pos);
            std::ptr::copy_nonoverlapping(src, out, n);
            buf.pos += n;
            n as c_int
        }
    }

    /// Encode a single indexed image (one frame, single color map) via
    /// giflib's `EGifOpen` / `EGifPutScreenDesc` / `EGifPutImageDesc` /
    /// `EGifPutLine` chain. `palette_rgb` is a slice of `[R, G, B]`
    /// triples, padded internally to the next power-of-two ≥ 2. Returns
    /// `None` when giflib isn't loaded, the palette is empty/oversized,
    /// or any giflib call reports failure.
    pub fn encode_indexed(
        indices: &[u8],
        width: u32,
        height: u32,
        palette_rgb: &[[u8; 3]],
    ) -> Option<Vec<u8>> {
        if width == 0 || height == 0 {
            return None;
        }
        let expected = (width as usize).checked_mul(height as usize)?;
        if indices.len() < expected {
            return None;
        }
        if palette_rgb.is_empty() || palette_rgb.len() > 256 {
            return None;
        }
        let l = lib()?;
        type EGifOpenFn =
            unsafe extern "C" fn(*mut c_void, OutputFunc, *mut c_int) -> *mut GifFileType;
        type GifMakeMapObjectFn =
            unsafe extern "C" fn(c_int, *const GifColorType) -> *mut ColorMapObject;
        type GifFreeMapObjectFn = unsafe extern "C" fn(*mut ColorMapObject);
        type EGifPutScreenDescFn = unsafe extern "C" fn(
            *mut GifFileType,
            c_int,
            c_int,
            c_int,
            c_int,
            *const ColorMapObject,
        ) -> c_int;
        type EGifPutImageDescFn = unsafe extern "C" fn(
            *mut GifFileType,
            c_int,
            c_int,
            c_int,
            c_int,
            bool,
            *const ColorMapObject,
        ) -> c_int;
        type EGifPutLineFn = unsafe extern "C" fn(*mut GifFileType, *mut u8, c_int) -> c_int;
        type EGifCloseFileFn = unsafe extern "C" fn(*mut GifFileType, *mut c_int) -> c_int;

        unsafe {
            let e_open: Symbol<EGifOpenFn> = l.get(b"EGifOpen").ok()?;
            let make_map: Symbol<GifMakeMapObjectFn> = l.get(b"GifMakeMapObject").ok()?;
            let free_map: Symbol<GifFreeMapObjectFn> = l.get(b"GifFreeMapObject").ok()?;
            let e_put_screen: Symbol<EGifPutScreenDescFn> = l.get(b"EGifPutScreenDesc").ok()?;
            let e_put_image: Symbol<EGifPutImageDescFn> = l.get(b"EGifPutImageDesc").ok()?;
            let e_put_line: Symbol<EGifPutLineFn> = l.get(b"EGifPutLine").ok()?;
            let e_close: Symbol<EGifCloseFileFn> = l.get(b"EGifCloseFile").ok()?;

            // giflib requires a power-of-two-sized colormap (2,4,...,256).
            let padded = palette_rgb.len().next_power_of_two().clamp(2, 256);
            let mut colors: Vec<GifColorType> = Vec::with_capacity(padded);
            for c in palette_rgb {
                colors.push(GifColorType {
                    red: c[0],
                    green: c[1],
                    blue: c[2],
                });
            }
            for _ in palette_rgb.len()..padded {
                colors.push(GifColorType {
                    red: 0,
                    green: 0,
                    blue: 0,
                });
            }
            let cmap = make_map(padded as c_int, colors.as_ptr());
            if cmap.is_null() {
                return None;
            }

            let mut buf = Box::new(EncodeBuf { data: Vec::new() });
            let buf_ptr: *mut c_void = &mut *buf as *mut EncodeBuf as *mut c_void;
            ENCODE_TLS.with(|c| *c.borrow_mut() = buf_ptr);

            let _guard = TlsGuard;
            // Hold a global encode lock — TLS-driven write callback would
            // race if two encodes ran on the same thread re-entrantly.
            static ENC_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            let _g = ENC_LOCK.get_or_init(|| Mutex::new(())).lock().ok()?;

            let mut err: c_int = 0;
            let gif = e_open(std::ptr::null_mut(), write_callback, &mut err);
            if gif.is_null() {
                free_map(cmap);
                return None;
            }

            // ColorRes 8, background 0, our colormap as the global one.
            if e_put_screen(gif, width as c_int, height as c_int, 8, 0, cmap) != 1 {
                free_map(cmap);
                let _ = e_close(gif, &mut err);
                return None;
            }
            if e_put_image(
                gif,
                0,
                0,
                width as c_int,
                height as c_int,
                false,
                std::ptr::null(),
            ) != 1
            {
                free_map(cmap);
                let _ = e_close(gif, &mut err);
                return None;
            }
            // Write each row separately; giflib mutates the line buffer
            // (it's not declared `const`), so we hand it an owned copy.
            let w = width as usize;
            let mut row: Vec<u8> = vec![0u8; w];
            for y in 0..height as usize {
                row.copy_from_slice(&indices[y * w..y * w + w]);
                if e_put_line(gif, row.as_mut_ptr(), w as c_int) != 1 {
                    free_map(cmap);
                    let _ = e_close(gif, &mut err);
                    return None;
                }
            }
            // EGifCloseFile flushes the trailer + closes the stream. It
            // also frees the colormap when it's owned by the file — but
            // ours was passed in directly, so we free it ourselves.
            if e_close(gif, &mut err) != 1 {
                free_map(cmap);
                return None;
            }
            free_map(cmap);

            Some(std::mem::take(&mut buf.data))
        }
    }

    /// RAII reset of `ENCODE_TLS` to null.
    struct TlsGuard;
    impl Drop for TlsGuard {
        fn drop(&mut self) {
            ENCODE_TLS.with(|c| *c.borrow_mut() = std::ptr::null_mut());
        }
    }
    struct DecTlsGuard;
    impl Drop for DecTlsGuard {
        fn drop(&mut self) {
            DECODE_TLS.with(|c| *c.borrow_mut() = std::ptr::null_mut());
        }
    }

    /// A GIF frame as decoded by giflib, with the raw palette + index
    /// plane preserved so callers can re-feed them into an encoder.
    pub struct DecodedIndexed {
        pub width: u32,
        pub height: u32,
        /// Raw palette: `color_count` RGB triples in giflib's order.
        /// Length is whatever giflib reports (typically the next
        /// power-of-two ≥ the source palette size).
        pub palette_rgb: Vec<[u8; 3]>,
        /// Tightly packed Pal8 indices, length `width * height`.
        pub indices: Vec<u8>,
    }

    /// Decode a GIF byte string to indices + palette via `DGifOpen` +
    /// `DGifSlurp`. Returns the *first* frame only (single-frame
    /// pipeline). Returns `None` on giflib-unavailable, decode failure,
    /// or missing palette.
    pub fn decode_to_indexed(data: &[u8]) -> Option<DecodedIndexed> {
        let l = lib()?;
        type DGifOpenFn =
            unsafe extern "C" fn(*mut c_void, InputFunc, *mut c_int) -> *mut GifFileType;
        type DGifSlurpFn = unsafe extern "C" fn(*mut GifFileType) -> c_int;
        type DGifCloseFileFn = unsafe extern "C" fn(*mut GifFileType, *mut c_int) -> c_int;

        unsafe {
            let d_open: Symbol<DGifOpenFn> = l.get(b"DGifOpen").ok()?;
            let d_slurp: Symbol<DGifSlurpFn> = l.get(b"DGifSlurp").ok()?;
            let d_close: Symbol<DGifCloseFileFn> = l.get(b"DGifCloseFile").ok()?;

            let mut dbuf = Box::new(DecodeBuf {
                data: data.as_ptr(),
                len: data.len(),
                pos: 0,
            });
            let dbuf_ptr: *mut DecodeBuf = &mut *dbuf;
            DECODE_TLS.with(|c| *c.borrow_mut() = dbuf_ptr);
            let _guard = DecTlsGuard;

            // Mirror the encode side: serialise concurrent decodes so
            // the TLS-backed read callback isn't trampled.
            static DEC_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            let _g = DEC_LOCK.get_or_init(|| Mutex::new(())).lock().ok()?;

            let mut err: c_int = 0;
            let gif = d_open(std::ptr::null_mut(), read_callback, &mut err);
            if gif.is_null() {
                return None;
            }
            if d_slurp(gif) != 1 {
                let _ = d_close(gif, &mut err);
                return None;
            }
            let f = &*gif;
            if f.image_count < 1 || f.saved_images.is_null() {
                let _ = d_close(gif, &mut err);
                return None;
            }
            let img = &*f.saved_images;
            let w = img.image_desc.width.max(0) as u32;
            let h = img.image_desc.height.max(0) as u32;
            if w == 0 || h == 0 || img.raster_bits.is_null() {
                let _ = d_close(gif, &mut err);
                return None;
            }
            // Pick the local color map if present, else the global one.
            let cmap_ptr = if img.image_desc.color_map.is_null() {
                f.s_color_map
            } else {
                img.image_desc.color_map
            };
            if cmap_ptr.is_null() {
                let _ = d_close(gif, &mut err);
                return None;
            }
            let cmap = &*cmap_ptr;
            if cmap.colors.is_null() || cmap.color_count <= 0 {
                let _ = d_close(gif, &mut err);
                return None;
            }
            let cmap_len = cmap.color_count as usize;
            let colors_slice = std::slice::from_raw_parts(cmap.colors, cmap_len);
            let palette_rgb: Vec<[u8; 3]> = colors_slice
                .iter()
                .map(|c| [c.red, c.green, c.blue])
                .collect();

            let n = (w as usize) * (h as usize);
            let raster = std::slice::from_raw_parts(img.raster_bits, n);
            let indices: Vec<u8> = raster.to_vec();

            let _ = d_close(gif, &mut err);
            Some(DecodedIndexed {
                width: w,
                height: h,
                palette_rgb,
                indices,
            })
        }
    }
}
