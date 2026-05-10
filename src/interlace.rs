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

#[cfg(test)]
mod tests {
    use super::*;

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
            vec![0, 8, 16, 4, 12, 2, 6, 10, 14, 18, 1, 3, 5, 7, 9, 11, 13, 15, 17, 19]
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
