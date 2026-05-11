#![no_main]

//! `decode_lenient` must be panic-free on arbitrary input the same
//! way `decode` is. The lenient parser drops malformed sub-blocks and
//! resyncs to the next §20 Image Separator / §27 Trailer instead of
//! returning an error; the fuzzer asserts the call returns at all and
//! never panics / aborts / OOMs.

use libfuzzer_sys::fuzz_target;
use oxideav_gif::decode_lenient;

fuzz_target!(|data: &[u8]| {
    let _ = decode_lenient(data);
});
