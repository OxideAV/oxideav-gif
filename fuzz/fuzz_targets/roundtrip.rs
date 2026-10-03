#![no_main]

//! For any `decode`-able input, re-encoding the resulting `GifFile`
//! and decoding the result must yield the same `GifFile`. This proves
//! the encoder is a left inverse of the decoder on the decoder's
//! image of valid inputs.

use libfuzzer_sys::fuzz_target;
use oxideav_gif::{encode_file, parse};

fuzz_target!(|data: &[u8]| {
    let Ok(img) = parse(data) else {
        return;
    };
    let Ok(encoded) = encode_file(&img) else {
        // Encoder rejected an input the decoder accepted: that's a
        // bug in either side. Surface it as a panic so the fuzzer
        // bins it.
        panic!("decoded image rejected by encoder");
    };
    let img2 = match parse(&encoded) {
        Ok(i) => i,
        Err(e) => panic!("re-encoded GIF failed to decode: {e}"),
    };
    assert_eq!(img, img2, "decode→encode→decode is not idempotent");
});
