#![no_main]
//! Round-trip the LZW codec under fuzzed inputs. The encoder accepts
//! a code-size ∈ 2..=8; the input is treated as palette indices that
//! must fit in `code_size` bits. We pick a code_size from the first
//! byte and clamp the rest of the buffer accordingly.

use libfuzzer_sys::fuzz_target;
use oxideav_gif::lzw;

fuzz_target!(|data: &[u8]| {
    let Some((&first, rest)) = data.split_first() else {
        return;
    };
    let code_size: u8 = (first % 7) + 2; // 2..=8
    let max: u16 = 1 << code_size;
    let indices: Vec<u8> = rest.iter().map(|&b| (b as u16 % max) as u8).collect();
    let enc = match lzw::encode(&indices, code_size) {
        Ok(b) => b,
        Err(_) => return,
    };
    let dec = match lzw::decode(&enc, code_size, indices.len()) {
        Ok(b) => b,
        Err(_) => panic!("LZW decode of self-encoded stream failed"),
    };
    assert_eq!(dec, indices, "LZW round-trip mismatch");
});
