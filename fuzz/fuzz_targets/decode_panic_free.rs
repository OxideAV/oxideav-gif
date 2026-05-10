#![no_main]

//! Decode arbitrary fuzz-supplied bytes. The decoder must always
//! return a `Result` and never panic, regardless of how malformed the
//! input is.

use libfuzzer_sys::fuzz_target;
use oxideav_gif::decode;

fuzz_target!(|data: &[u8]| {
    // The return value is intentionally discarded — the contract under
    // test is that the call returns at all, and that any returned
    // error is a `Result::Err` (not a panic / abort / OOM).
    let _ = decode(data);
});
