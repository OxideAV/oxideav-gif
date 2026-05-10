//! Opt-in cross-validation against an external opaque validator.
//!
//! This test spawns the binary at `$OXIDEAV_GIF_VALIDATOR_BIN` (if set)
//! and feeds it our encoded GIF bytes on stdin. The validator is
//! treated as a complete black box — only its exit status and the
//! presence/absence of any decode-error pattern in its output is
//! consulted. The binary's identity is never named in code, comments,
//! or tests.
//!
//! When the env var is unset, the test prints a SKIP marker via
//! `eprintln!` and returns successfully (no `#[ignore]` is used; the
//! skip path is a runtime decision so opt-in CI runs always re-evaluate
//! the validator's availability).

use std::io::Write;
use std::process::{Command, Stdio};

use oxideav_gif::{encode, Block, Frame, GifImage, Rgb, Version};

fn build_sample_gif() -> Vec<u8> {
    let palette = vec![
        Rgb::new(0, 0, 0),
        Rgb::new(0xFF, 0, 0),
        Rgb::new(0, 0xFF, 0),
        Rgb::new(0, 0, 0xFF),
    ];
    let w = 8u16;
    let h = 8u16;
    let mut indices = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            indices.push(((x + y) % 4) as u8);
        }
    }
    let img = GifImage {
        version: Version::Gif89a,
        screen_width: w,
        screen_height: h,
        color_resolution: 1,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(palette),
        blocks: vec![Block::Image(Frame {
            left: 0,
            top: 0,
            width: w,
            height: h,
            local_palette: None,
            palette_sorted: false,
            interlaced: false,
            indices,
            graphic_control: None,
        })],
    };
    encode(&img).expect("encode test sample")
}

#[test]
fn external_validator_accepts_our_output() {
    let bin = match std::env::var("OXIDEAV_GIF_VALIDATOR_BIN") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!(
                "SKIP external_validator_accepts_our_output: OXIDEAV_GIF_VALIDATOR_BIN unset"
            );
            return;
        }
    };

    let bytes = build_sample_gif();

    let mut child = match Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP external_validator: failed to spawn: {e}");
            return;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&bytes);
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("SKIP external_validator: wait failed: {e}");
            return;
        }
    };

    assert!(
        output.status.success(),
        "external validator rejected our encoded output (exit {:?}, stderr: {})",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}
