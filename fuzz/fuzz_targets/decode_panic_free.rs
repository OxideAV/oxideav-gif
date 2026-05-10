#![no_main]
//! `decode_gif` must NEVER panic on arbitrary input. Errors → `Err`,
//! never `unwrap`/`unreachable`/`assert`/index-out-of-bounds.

use libfuzzer_sys::fuzz_target;
use oxideav_gif::decode_gif;

fuzz_target!(|data: &[u8]| {
    let _ = decode_gif(data);
});
