//! Black-box cross-validation against installed GIF tools.
//!
//! We do NOT read any third-party GIF library source code — these tests
//! invoke binaries (`magick`, `giftext`) as opaque processes and parse
//! their stdout / stderr / output files. If the binary is not on `PATH`
//! the test is skipped (not failed) so the suite stays green on
//! systems without optional dev tooling installed.

use std::io::Write;
use std::process::{Command, Stdio};

use oxideav_gif::{decode_gif, encode_gif, GifBlock, GifFrame, GifImage, GifVersion};

/// Smallest interesting GIF: 4-color palette, 4 pixels in a 2x2 grid,
/// one of each color. Useful as a known test image.
fn four_color_2x2() -> GifImage {
    GifImage {
        version: GifVersion::Gif87a,
        width: 2,
        height: 2,
        color_resolution: 1,
        global_palette_sorted: false,
        background_color_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(vec![
            0, 0, 0, // 0: black
            255, 255, 255, // 1: white
            255, 0, 0, // 2: red
            0, 255, 0, // 3: green
        ]),
        blocks: vec![GifBlock::Frame(GifFrame {
            left: 0,
            top: 0,
            width: 2,
            height: 2,
            local_palette: None,
            local_palette_sorted: false,
            interlaced: false,
            indices: vec![0, 1, 2, 3],
            control: None,
        })],
    }
}

/// Helper: invoke a binary, return Some(output) or None if the binary
/// is not on PATH.
fn try_run(cmd: &str, args: &[&str], stdin: Option<&[u8]>) -> Option<std::process::Output> {
    let mut c = Command::new(cmd);
    c.args(args);
    c.stdout(Stdio::piped());
    c.stderr(Stdio::piped());
    if stdin.is_some() {
        c.stdin(Stdio::piped());
    }
    match c.spawn() {
        Ok(mut child) => {
            if let Some(data) = stdin {
                if let Some(mut s) = child.stdin.take() {
                    let _ = s.write_all(data);
                }
            }
            child.wait_with_output().ok()
        }
        Err(_) => None,
    }
}

/// Cross-validate: encode a GIF with us, then have ImageMagick decode
/// it and re-encode as PPM. Read the PPM back and compare RGB triples
/// against what we expect (per palette lookup of our indices).
#[test]
fn imagemagick_can_read_our_encoded_gif() {
    let img = four_color_2x2();
    let our_bytes = encode_gif(&img).unwrap();

    // Probe ImageMagick. If absent, skip silently.
    let probe = Command::new("magick").arg("-version").output();
    if probe.is_err() {
        eprintln!("skipping: `magick` not installed");
        return;
    }

    // Use stdin → stdout pipeline. `magick gif:- ppm:-` reads a GIF
    // from stdin and writes a PPM to stdout. ImageMagick will refuse
    // to write to stdout for some formats unless we use this exact
    // syntax.
    let out =
        try_run("magick", &["gif:-", "ppm:-"], Some(&our_bytes)).expect("magick failed to spawn");
    if !out.status.success() {
        eprintln!("magick stderr: {}", String::from_utf8_lossy(&out.stderr));
    }
    assert!(
        out.status.success(),
        "magick refused to decode our GIF: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Parse the PPM (P6 binary). Header is "P6\n<w> <h>\n<maxval>\n"
    // followed by raw RGB bytes.
    let body = out.stdout;
    let mut pos = 0;
    fn next_token(buf: &[u8], pos: &mut usize) -> Vec<u8> {
        while *pos < buf.len() && buf[*pos].is_ascii_whitespace() {
            *pos += 1;
        }
        let start = *pos;
        while *pos < buf.len() && !buf[*pos].is_ascii_whitespace() {
            *pos += 1;
        }
        buf[start..*pos].to_vec()
    }
    let magic = next_token(&body, &mut pos);
    assert_eq!(&magic, b"P6", "expected P6 PPM, got {:?}", magic);
    let w: usize = std::str::from_utf8(&next_token(&body, &mut pos))
        .unwrap()
        .parse()
        .unwrap();
    let h: usize = std::str::from_utf8(&next_token(&body, &mut pos))
        .unwrap()
        .parse()
        .unwrap();
    let _max: usize = std::str::from_utf8(&next_token(&body, &mut pos))
        .unwrap()
        .parse()
        .unwrap();
    // Skip a single whitespace byte.
    if pos < body.len() && body[pos].is_ascii_whitespace() {
        pos += 1;
    }
    assert_eq!(w, 2);
    assert_eq!(h, 2);
    let pixels = &body[pos..];
    assert_eq!(pixels.len(), w * h * 3);

    // Expected RGB values from our palette + indices.
    let pal: [(u8, u8, u8); 4] = [(0, 0, 0), (255, 255, 255), (255, 0, 0), (0, 255, 0)];
    let indices: [u8; 4] = [0, 1, 2, 3];
    let expected: Vec<u8> = indices
        .iter()
        .flat_map(|&i| {
            let (r, g, b) = pal[i as usize];
            [r, g, b]
        })
        .collect();
    assert_eq!(
        pixels, expected,
        "magick-decoded RGB doesn't match our palette lookup"
    );
}

/// Cross-validate the other direction: have ImageMagick *encode* a
/// known image, then we decode it and verify pixel values.
#[test]
fn we_can_decode_imagemagick_encoded_gif() {
    let probe = Command::new("magick").arg("-version").output();
    if probe.is_err() {
        eprintln!("skipping: `magick` not installed");
        return;
    }

    // Create a 4x2 PPM with known pixels.
    // Row 0: (255,0,0) (0,255,0) (0,0,255) (255,255,255)
    // Row 1: (255,255,255) (0,0,0) (128,128,128) (64,200,32)
    let ppm = b"P6\n4 2\n255\n\
        \xff\x00\x00\x00\xff\x00\x00\x00\xff\xff\xff\xff\
        \xff\xff\xff\x00\x00\x00\x80\x80\x80\x40\xc8\x20";
    let out = try_run("magick", &["ppm:-", "gif:-"], Some(ppm)).expect("magick failed to spawn");
    if !out.status.success() {
        eprintln!("magick stderr: {}", String::from_utf8_lossy(&out.stderr));
    }
    assert!(out.status.success(), "magick refused to encode the PPM");

    let gif = out.stdout;
    let img =
        decode_gif(&gif).unwrap_or_else(|e| panic!("failed to decode magick-encoded GIF: {e:?}"));
    assert_eq!(img.width, 4);
    assert_eq!(img.height, 2);
    assert!(img.frames().count() >= 1);

    // Composite first frame onto a zero canvas to get RGBA, then
    // compare colours pixel-by-pixel against the source PPM.
    let zero = vec![0u8; (img.width as usize) * (img.height as usize) * 4];
    let rgba = img.composite_frame_rgba(0, &zero).unwrap();
    let src_rgb: Vec<(u8, u8, u8)> = vec![
        (255, 0, 0),
        (0, 255, 0),
        (0, 0, 255),
        (255, 255, 255),
        (255, 255, 255),
        (0, 0, 0),
        (128, 128, 128),
        (64, 200, 32),
    ];
    for (i, (r, g, b)) in src_rgb.iter().enumerate() {
        let p = i * 4;
        let (dr, dg, db) = (rgba[p], rgba[p + 1], rgba[p + 2]);
        // Magick may dither / pick palette differently; allow ±32 per
        // channel as a slack.
        let drf = (dr as i32 - *r as i32).abs();
        let dgf = (dg as i32 - *g as i32).abs();
        let dbf = (db as i32 - *b as i32).abs();
        assert!(
            drf <= 32 && dgf <= 32 && dbf <= 32,
            "pixel {i} colour drift > 32 channels: src=({r},{g},{b}) decoded=({dr},{dg},{db})"
        );
    }
}

/// Cross-validate against giflib's `giftext` (a giflib companion tool
/// that prints a GIF file's structure to stdout). We treat the binary
/// as a structural validator: if `giftext` accepts our file without
/// error, our on-disk bytes parse cleanly per the giflib reference.
#[test]
fn giftext_can_parse_our_encoded_gif() {
    let probe = Command::new("giftext").arg("-h").output();
    if probe.is_err() {
        eprintln!("skipping: `giftext` not installed");
        return;
    }

    let img = four_color_2x2();
    let our_bytes = encode_gif(&img).unwrap();

    let out = try_run("giftext", &[], Some(&our_bytes)).expect("giftext failed to spawn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "giftext rejected our GIF.\nstderr: {stderr}\nstdout: {stdout}"
    );

    // A bare structural sanity check: giftext should report the screen
    // width and height we emitted.
    assert!(
        stdout.contains("Width = 2") || stdout.contains("Width: 2") || stdout.contains("Width 2"),
        "giftext output didn't report our screen width: {stdout}"
    );
}
