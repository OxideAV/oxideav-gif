//! Colour-difference spaces for palette selection and nearest-entry
//! search.
//!
//! GIF stores 8-bit sRGB-like triples (§19 / §21); how close two of them
//! *look* is not their Euclidean distance in that encoding. The
//! quantiser therefore measures distance in CIE 1976 L\*a\*b\* — a space
//! built so that equal Euclidean steps are roughly equal perceived
//! steps — computed from first principles:
//!
//! 1. undo the sRGB transfer curve (`c / 12.92` below the linear toe,
//!    `((c + 0.055) / 1.055)^2.4` above it);
//! 2. apply the linear map from the Rec. 709 / sRGB primaries with a D65
//!    white to CIE XYZ, normalised so that white maps to `(1, 1, 1)`;
//! 3. apply the CIE lightness function `f(t) = t^(1/3)` (with its linear
//!    segment below `(6/29)^3`) and form `L = 116 f(Y) - 16`,
//!    `a = 500 (f(X) - f(Y))`, `b = 200 (f(Y) - f(Z))`.

use std::sync::OnceLock;

/// The space colour distances are measured in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Metric {
    /// Squared Euclidean distance over the stored 8-bit R, G, B values.
    Rgb,
    /// Squared Euclidean distance in CIE L\*a\*b\* (ΔE\*ab squared).
    Lab,
}

/// sRGB primaries + D65 white → XYZ, each row divided by the white
/// point's component so that `(1, 1, 1)` linear RGB maps to `(1, 1, 1)`.
const RGB_TO_WHITE_NORMALISED_XYZ: [[f32; 3]; 3] = [
    [0.433_953, 0.376_219, 0.189_828],
    [0.212_671, 0.715_160, 0.072_169],
    [0.017_757, 0.109_468, 0.872_775],
];

/// `(6/29)^3`: below this the lightness function is linear.
const CUBE_ROOT_TOE: f32 = 216.0 / 24389.0;
/// Slope of the linear segment, `(29/6)^2 / 3`.
const TOE_SLOPE: f32 = 841.0 / 108.0;

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(l: f32) -> f32 {
    if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// Linear-light value of each 8-bit code.
fn linear_table() -> &'static [f32; 256] {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0)))
}

fn lightness_fn_direct(t: f32) -> f32 {
    if t > CUBE_ROOT_TOE {
        t.cbrt()
    } else {
        TOE_SLOPE * t + 4.0 / 29.0
    }
}

/// Segments of the tabulated lightness function over `0.0..=1.0`.
const LIGHTNESS_SEGMENTS: usize = 4096;

/// The CIE lightness function `f(t)` for `t` in `0.0..=1.0`, linearly
/// interpolated from a table (the cube root is by far the most expensive
/// step of the conversion). The curve is smooth and the table fine
/// enough that the interpolation stays within a few thousandths of an
/// L\*a\*b\* unit; every point of the space goes through this one
/// function, so distances are consistent.
#[inline]
fn lightness_fn(t: f32) -> f32 {
    static TABLE: OnceLock<Vec<f32>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        (0..=LIGHTNESS_SEGMENTS + 1)
            .map(|i| lightness_fn_direct(i as f32 / LIGHTNESS_SEGMENTS as f32))
            .collect()
    });
    let x = t.clamp(0.0, 1.0) * LIGHTNESS_SEGMENTS as f32;
    let i = x as usize;
    let frac = x - i as f32;
    table[i] + (table[i + 1] - table[i]) * frac
}

fn lightness_fn_inverse(f: f32) -> f32 {
    if f > 6.0 / 29.0 {
        f * f * f
    } else {
        (f - 4.0 / 29.0) / TOE_SLOPE
    }
}

fn linear_to_lab(lin: [f32; 3]) -> [f32; 3] {
    let m = &RGB_TO_WHITE_NORMALISED_XYZ;
    let f: [f32; 3] = std::array::from_fn(|i| {
        lightness_fn(m[i][0] * lin[0] + m[i][1] * lin[1] + m[i][2] * lin[2])
    });
    [
        116.0 * f[1] - 16.0,
        500.0 * (f[0] - f[1]),
        200.0 * (f[1] - f[2]),
    ]
}

/// Inverse of [`RGB_TO_WHITE_NORMALISED_XYZ`] by cofactor expansion.
fn xyz_to_rgb_matrix() -> &'static [[f32; 3]; 3] {
    static INVERSE: OnceLock<[[f32; 3]; 3]> = OnceLock::new();
    INVERSE.get_or_init(|| {
        let m = RGB_TO_WHITE_NORMALISED_XYZ.map(|r| r.map(f64::from));
        let cof = |r: usize, c: usize| {
            let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
            let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
            m[r1][c1] * m[r2][c2] - m[r1][c2] * m[r2][c1]
        };
        let det = m[0][0] * cof(0, 0) + m[0][1] * cof(0, 1) + m[0][2] * cof(0, 2);
        // Inverse = transpose of the cofactor matrix over the determinant.
        std::array::from_fn(|r| std::array::from_fn(|c| (cof(c, r) / det) as f32))
    })
}

impl Metric {
    /// Squared distance below which two colours are taken to look the
    /// same: one L\*a\*b\* unit (about one just-noticeable difference),
    /// or two 8-bit code steps for the R, G, B metric.
    pub(crate) fn visibility_floor_squared(self) -> f32 {
        match self {
            Metric::Rgb => 4.0,
            Metric::Lab => 1.0,
        }
    }

    /// The point an 8-bit triple occupies in this space.
    pub(crate) fn point(self, rgb: [u8; 3]) -> [f32; 3] {
        match self {
            Metric::Rgb => rgb.map(f32::from),
            Metric::Lab => {
                let t = linear_table();
                linear_to_lab(rgb.map(|c| t[c as usize]))
            }
        }
    }

    /// The point a fractional triple (each component in `0.0..=255.0`)
    /// occupies in this space.
    pub(crate) fn point_f(self, rgb: [f32; 3]) -> [f32; 3] {
        match self {
            Metric::Rgb => rgb,
            Metric::Lab => linear_to_lab(rgb.map(|c| srgb_to_linear(c / 255.0))),
        }
    }

    /// The 8-bit triple nearest to a point of this space (out-of-gamut
    /// points are clipped per component).
    pub(crate) fn to_rgb8(self, p: [f32; 3]) -> [u8; 3] {
        let round = |v: f32| (v + 0.5).clamp(0.0, 255.0) as u8;
        match self {
            Metric::Rgb => p.map(round),
            Metric::Lab => {
                let fy = (p[0] + 16.0) / 116.0;
                let xyz = [fy + p[1] / 500.0, fy, fy - p[2] / 200.0].map(lightness_fn_inverse);
                let inv = xyz_to_rgb_matrix();
                std::array::from_fn(|i| {
                    let lin = inv[i][0] * xyz[0] + inv[i][1] * xyz[1] + inv[i][2] * xyz[2];
                    round(linear_to_srgb(lin.clamp(0.0, 1.0)) * 255.0)
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_black_and_grey_axis() {
        let w = Metric::Lab.point([255, 255, 255]);
        assert!(
            (w[0] - 100.0).abs() < 0.01 && w[1].abs() < 0.02 && w[2].abs() < 0.02,
            "{w:?}"
        );
        let k = Metric::Lab.point([0, 0, 0]);
        assert!(k.iter().all(|c| c.abs() < 1e-3), "{k:?}");
        // Neutral greys have no chroma; mid-grey 119 is close to L = 50.
        let g = Metric::Lab.point([119, 119, 119]);
        assert!(
            (g[0] - 50.0).abs() < 0.6 && g[1].abs() < 0.02 && g[2].abs() < 0.02,
            "{g:?}"
        );
    }

    #[test]
    fn every_grid_colour_round_trips() {
        for r in (0..=255).step_by(15) {
            for g in (0..=255).step_by(15) {
                for b in (0..=255).step_by(15) {
                    let c = [r as u8, g as u8, b as u8];
                    assert_eq!(Metric::Lab.to_rgb8(Metric::Lab.point(c)), c);
                    assert_eq!(Metric::Rgb.to_rgb8(Metric::Rgb.point(c)), c);
                }
            }
        }
    }

    #[test]
    fn fractional_point_matches_table_on_integers() {
        for v in [0u8, 1, 10, 11, 128, 254, 255] {
            let a = Metric::Lab.point([v, 255 - v, v / 2]);
            let b = Metric::Lab.point_f([v as f32, (255 - v) as f32, (v / 2) as f32]);
            for i in 0..3 {
                assert!((a[i] - b[i]).abs() < 1e-3);
            }
        }
    }
}
