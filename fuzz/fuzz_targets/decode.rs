#![no_main]

//! End-to-end GIF decode fuzz harness.
//!
//! This target stresses the decode-side public surface plus every
//! downstream API that consumes the decoder's output, so panics in any
//! of the spec-classic problem areas — LZW code-table growth (Appendix
//! F), Graphic Control + Application Extension nesting (§23 + §26),
//! sub-block-chain framing (§15), frame-disposal arithmetic (§23.c.iv),
//! palette + transparency edge cases (§19 / §21 / §23.c.viii), and the
//! NETSCAPE2.0 loop-count sub-block — surface here regardless of which
//! entry point first hits them.
//!
//! Contract: every called function returns to its caller. A `panic!`,
//! `unwrap()` on `None`, slice-OOB, integer-overflow in debug, or OOM
//! abort is a finding and fails the fuzzer. Wrong-but-non-panic
//! behaviour (e.g. an `Err` on legitimate input) is out of scope for
//! this target.

use libfuzzer_sys::fuzz_target;
use oxideav_gif::{
    app_ext, compose, decode, decode_first_frame, decode_lenient, encode, playback::Playback,
};

// Cap downstream work on suspiciously large screens so we don't fault
// for OOM on a single ~17 GiB `Vec<u8>` allocation in `RgbaCanvas`.
// The decoder itself doesn't bound screen_width × screen_height, so this
// is the harness's responsibility.
const MAX_CANVAS_PIXELS: u32 = 1 << 20; // 1 Mpx — generous for fuzzing.

// Cap looping playback iterations so a `loop_count = Some(0)` (forever)
// stream doesn't pin the fuzzer indefinitely.
const MAX_PLAYBACK_FRAMES: usize = 64;

fuzz_target!(|data: &[u8]| {
    // 1. Lenient + strict + cover-frame entry points — all three must
    //    return on arbitrary bytes regardless of how malformed the
    //    input is. The lenient path resyncs to the next §20 Image
    //    Separator / §27 Trailer rather than returning Err, so it
    //    exercises a different state machine than the strict path.
    let _ = decode_lenient(data);
    let _ = decode_first_frame(data);

    let Ok(img) = decode(data) else {
        return;
    };

    // 2. Capacity gate — `compose` / `Playback` allocate
    //    `screen_width × screen_height × 4` bytes per canvas. A 65535²
    //    screen would request ~17 GiB and OOM the fuzzer before any
    //    interesting code ran.
    let pixels = (img.screen_width as u32).saturating_mul(img.screen_height as u32);
    if pixels > MAX_CANVAS_PIXELS {
        return;
    }

    // 3. Eager compositor — exercises §23 disposal-method state
    //    machine, §23.c.viii transparent-index handling, frame-rect
    //    bounds check, palette-index range check, and Plain Text
    //    (§25) rendering against the crate-local 8×8 font.
    let composed = compose(&img);

    // 3b. §20 inter-frame rect optimisation — cropping each frame to
    //     its changed region must preserve the composed output
    //     *exactly* on every decodable stream. This is an asserted
    //     equivalence, not just panic-freedom: a mismatch is a finding.
    {
        let mut optimized = img.clone();
        let _ = optimized.optimize_frame_rects();
        match (&composed, compose(&optimized)) {
            (Ok(before), Ok(after)) => assert_eq!(
                before, &after,
                "optimize_frame_rects changed the composed output"
            ),
            (Err(_), _) => {
                // A non-composing stream must be left untouched.
                assert_eq!(&optimized, &img);
            }
            (Ok(_), Err(e)) => {
                panic!("optimize_frame_rects broke composability: {e}");
            }
        }
    }

    // 4. Lazy playback iterators — same compositor state machine via a
    //    different driver, plus the NETSCAPE2.0 *Looping* sub-block
    //    semantics. Bounded explicitly because `loop_count = Some(0)`
    //    means "forever".
    let pb = Playback::new(&img);
    for _ in pb.frames().take(MAX_PLAYBACK_FRAMES) {}
    for _ in pb.looping_frames().take(MAX_PLAYBACK_FRAMES) {}

    // 5. Timing accessors — pure arithmetic over `GraphicControl::
    //    delay_centis` and the loop-count multiplier. Saturating ops
    //    should keep these total even on adversarial delay values.
    let _ = img.is_animated();
    let _ = img.single_pass_duration();
    let _ = img.total_play_duration();
    let _: Vec<_> = img.frame_delays().collect();
    let _ = img.loop_count();
    let _ = img.netscape_buffer_hint();

    // 6. Structured §26 Application Extension views — these are the
    //    "classic spot" the brief calls out (malformed netscape
    //    loop-count, nested sub-block chains, identifier+auth
    //    parsing). Walk every application extension through each
    //    typed parser; mismatched identifier+auth pairs return Err
    //    rather than panic, so the result is intentionally discarded.
    for app in img.application_extensions() {
        let _ = app_ext::LoopControl::from_application(app);
        let _ = app_ext::AnimextsLoopControl::from_application(app);
        let _ = app_ext::XmpPacket::from_application(app);
        let _ = app_ext::IccProfile::from_application(app);
        let _ = app_ext::ExifMetadata::from_application(app);
    }

    // 7. §24 Comment Extension accessors — comments are sub-block
    //    chains the decoder concatenates; ensure the §24.e.i ASCII
    //    check + the LF-join helper handle arbitrary byte payloads.
    let _ = img.concatenated_comment();
    let _ = img.comments_are_7bit_ascii();
    let _ = img.comments_in_recommended_position();
    let _: Vec<_> = img.comments().collect();

    // 8. §18.c.viii Pixel Aspect Ratio round-trip.
    let _ = img.pixel_aspect_ratio_value();

    // 9. §7 Required Version inference — must not panic for any
    //    decoded image regardless of which 89a-only blocks are present.
    let _ = img.required_version();

    // 10. Encode + re-decode the canonical image — proves the
    //     encoder is panic-free on every decoder output and shakes
    //     out cases where a decoded value escapes the encoder's
    //     validation. (The dedicated `roundtrip` target asserts
    //     equality; here we only care about panic-freedom on the
    //     re-decode path, which is a different state than fresh
    //     fuzzer bytes.)
    if let Ok(bytes) = encode(&img) {
        let _ = decode(&bytes);
        let _ = decode_lenient(&bytes);
        let _ = decode_first_frame(&bytes);
    }
});
