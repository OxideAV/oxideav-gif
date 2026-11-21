//! Appendix E — interlaced row ordering.
//!
//! The four passes are:
//! - Pass 1: every 8th row, starting with row 0
//! - Pass 2: every 8th row, starting with row 4
//! - Pass 3: every 4th row, starting with row 2
//! - Pass 4: every 2nd row, starting with row 1

/// Walk all rows of an `height`-row image in interlaced storage order
/// and emit each one's destination row index in the de-interlaced
/// raster.
///
/// Length of the returned vector is exactly `height`.
pub(crate) fn interlace_row_order(height: u16) -> Vec<u16> {
    let h = height as usize;
    let mut out = Vec::with_capacity(h);
    for &(start, step) in &[(0usize, 8usize), (4, 8), (2, 4), (1, 2)] {
        let mut r = start;
        while r < h {
            out.push(r as u16);
            r += step;
        }
    }
    debug_assert_eq!(out.len(), h);
    out
}

/// Reorder the rows of `raster` (`height` rows of `width` pixels) from
/// interlaced storage order to top-to-bottom order, in place.
///
/// Storage row `s` belongs at row `order[s]`; the move is a permutation
/// of the rows, carried out cycle by cycle with one row of scratch
/// space, so de-interlacing needs no second raster.
pub(crate) fn deinterlace_in_place(raster: &mut [u8], width: usize, height: u16) {
    let order = interlace_row_order(height);
    if width == 0 || raster.len() < width * order.len() {
        return;
    }
    let mut placed = vec![false; order.len()];
    let mut carried = vec![0u8; width];
    let mut displaced = vec![0u8; width];
    for start in 0..order.len() {
        if placed[start] || order[start] as usize == start {
            placed[start] = true;
            continue;
        }
        // Carry the row stored at `start` to where it belongs, pick up
        // the row it displaces, and follow the cycle back to `start`.
        carried.copy_from_slice(&raster[start * width..(start + 1) * width]);
        let mut from = start;
        loop {
            let to = order[from] as usize;
            let slot = &mut raster[to * width..(to + 1) * width];
            displaced.copy_from_slice(slot);
            slot.copy_from_slice(&carried);
            std::mem::swap(&mut carried, &mut displaced);
            placed[from] = true;
            from = to;
            if from == start {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_place_matches_the_row_by_row_copy() {
        for height in [1u16, 2, 3, 4, 5, 7, 8, 9, 16, 17, 33, 100] {
            for width in [1usize, 3, 8] {
                let stored: Vec<u8> = (0..width * height as usize)
                    .map(|i| (i * 31) as u8)
                    .collect();
                let mut want = vec![0u8; stored.len()];
                for (s, &t) in interlace_row_order(height).iter().enumerate() {
                    want[t as usize * width..(t as usize + 1) * width]
                        .copy_from_slice(&stored[s * width..(s + 1) * width]);
                }
                let mut got = stored.clone();
                deinterlace_in_place(&mut got, width, height);
                assert_eq!(got, want, "{width}x{height}");
            }
        }
    }

    #[test]
    fn appendix_e_example_20_rows() {
        // The spec's 20-row figure assigns each row to one of four
        // passes. Reproduce that mapping by collecting rows pass by
        // pass and check equality with the figure.
        let order = interlace_row_order(20);
        assert_eq!(
            order,
            // pass 1: 0, 8, 16
            // pass 2: 4, 12
            // pass 3: 2, 6, 10, 14, 18
            // pass 4: 1, 3, 5, 7, 9, 11, 13, 15, 17, 19
            vec![
                0, 8, 16, 4, 12, 2, 6, 10, 14, 18, 1, 3, 5, 7, 9, 11, 13, 15, 17, 19
            ]
        );
    }

    #[test]
    fn covers_each_row_exactly_once() {
        for h in 1u16..=300 {
            let order = interlace_row_order(h);
            let mut seen = vec![false; h as usize];
            for r in order {
                assert!(!seen[r as usize], "row {r} repeated for height {h}");
                seen[r as usize] = true;
            }
            assert!(seen.iter().all(|x| *x));
        }
    }

    #[test]
    fn height_one_emits_pass1_only() {
        assert_eq!(interlace_row_order(1), vec![0]);
    }
}
