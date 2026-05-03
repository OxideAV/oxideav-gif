//! Runtime giflib interop for the cross-decode fuzz harnesses.
//!
//! giflib is loaded via `dlopen` at first call — there is no
//! `gif-sys`-style build-script dep that would pull giflib source
//! into the workspace's cargo dep tree. Each harness checks
//! [`giflib::available`] up front and `return`s early when the
//! shared library isn't installed, so fuzz binaries built on a host
//! without giflib simply do nothing instead of panicking.
//!
//! Install giflib with `brew install giflib` (macOS) or
//! `apt install libgif-dev` (Debian/Ubuntu). The loader probes the
//! conventional shared-object names for both platforms.

#![allow(unsafe_code)]

pub mod giflib {
    use libloading::{Library, Symbol};
    use std::os::raw::{c_int, c_void};
    use std::sync::{Mutex, OnceLock};

    /// Conventional giflib shared-object names the loader will try
    /// in order. Covers macOS (`.dylib`), Linux (versioned + plain
    /// `.so`), and Windows (`.dll`).
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
                // accept that risk for fuzz tooling — giflib is a
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
    /// Cross-decode fuzz harnesses early-return when this is false so
    /// the binary still runs without an oracle (the assertions just
    /// don't fire).
    pub fn available() -> bool {
        lib().is_some()
    }

    // ---- Shape mirrors of giflib's public structs ------------------------
    //
    // We mirror only the fields/types we actually touch. giflib's structs
    // are stable across the 5.x / 6.x ABI — the ordering and sizes of the
    // members below are part of the documented public API.

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
        // are not touched here.
    }

    type OutputFunc = unsafe extern "C" fn(*mut GifFileType, *const u8, c_int) -> c_int;
    type InputFunc = unsafe extern "C" fn(*mut GifFileType, *mut u8, c_int) -> c_int;

    // ---- Encode side ----------------------------------------------------

    /// Buffer threaded through giflib's writeFunc callback. We chase the
    /// raw pointer back to the `Vec` via `UserData`, which giflib leaves
    /// alone for the caller's use.
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

    use std::cell::RefCell;
    thread_local! {
        // Pointer to the EncodeBuf currently driving giflib's writeFunc.
        // Set inside `encode_indexed`, cleared on exit. Must NOT be
        // touched by other threads (each thread gets its own).
        static ENCODE_TLS: RefCell<*mut c_void> = const { RefCell::new(std::ptr::null_mut()) };
    }

    // Decode side mirror of the encode-side TLS.
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
    /// `EGifPutLine` chain. `palette` is a slice of `[R, G, B]` triples,
    /// padded internally to the next power-of-two ≥ 2. Returns `None`
    /// when giflib isn't loaded, the palette is empty/oversized, or any
    /// giflib call reports failure.
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
            let padded = palette_rgb.len().next_power_of_two().max(2).min(256);
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

            // Lock TLS for the duration of this encode.
            let mut buf = Box::new(EncodeBuf { data: Vec::new() });
            let buf_ptr: *mut c_void = &mut *buf as *mut EncodeBuf as *mut c_void;
            ENCODE_TLS.with(|c| *c.borrow_mut() = buf_ptr);

            let _guard = TlsGuard;
            // Hold a global encode lock — TLS-driven write callback would
            // race if two encodes ran on the same thread re-entrantly.
            // The mutex also limits cross-thread reentrancy in case
            // libfuzzer ever schedules harness invocations in parallel.
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

    /// A GIF frame as decoded by giflib, normalised to RGBA.
    pub struct DecodedRgba {
        pub width: u32,
        pub height: u32,
        /// Tightly packed RGBA, length `width * height * 4`. Alpha is
        /// always 0xFF — GIF palette entries have no alpha channel.
        pub rgba: Vec<u8>,
    }

    /// Decode a GIF byte string to RGBA via `DGifOpen` + `DGifSlurp`.
    /// Returns the *first* frame only (matches the cross-decode harness
    /// shape — both sides only handle one frame). Returns `None` on
    /// giflib-unavailable, decode failure, or missing palette.
    pub fn decode_to_rgba(data: &[u8]) -> Option<DecodedRgba> {
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
            // Pull frame 0.
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
            let colors = std::slice::from_raw_parts(cmap.colors, cmap_len);

            let n = (w as usize) * (h as usize);
            let raster = std::slice::from_raw_parts(img.raster_bits, n);

            let mut rgba: Vec<u8> = Vec::with_capacity(n * 4);
            for &idx in raster {
                let i = idx as usize;
                if i < cmap_len {
                    let c = &colors[i];
                    rgba.push(c.red);
                    rgba.push(c.green);
                    rgba.push(c.blue);
                } else {
                    rgba.extend_from_slice(&[0, 0, 0]);
                }
                rgba.push(0xFF);
            }

            let _ = d_close(gif, &mut err);
            Some(DecodedRgba {
                width: w,
                height: h,
                rgba,
            })
        }
    }
}
