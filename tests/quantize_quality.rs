//! Quality properties of the truecolour → colour-table reduction,
//! measured on synthetic pictures: colour difference in CIE L*a*b*,
//! exactness, dither behaviour.

#[path = "support/synth.rs"]
mod synth;

use oxideav_gif::quantize::{
    ColorMetric, Dither, PaletteMethod, QuantizeOptions, quantize_rgb_with_options,
    remap_rgb_to_palette,
};

const W: usize = 160;
const H: usize = 120;

fn render(q: &oxideav_gif::Quantized) -> Vec<u8> {
    q.indices
        .iter()
        .flat_map(|&i| {
            let c = q.palette[i as usize];
            [c.r, c.g, c.b]
        })
        .collect()
}

fn delta_e(rgb: &[u8], opts: QuantizeOptions) -> f64 {
    let q = quantize_rgb_with_options(rgb, W, H, opts).unwrap();
    synth::mean_delta_e(rgb, &render(&q))
}

fn delta_e_low_pass(rgb: &[u8], opts: QuantizeOptions) -> f64 {
    let q = quantize_rgb_with_options(rgb, W, H, opts).unwrap();
    synth::mean_delta_e_low_pass(rgb, &render(&q), W, H)
}

fn pictures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("photo", synth::photo(W, H, 1)),
        ("gradient", synth::gradient(W, H)),
        ("screen", synth::screen(W, H, 2)),
    ]
}

#[test]
fn default_palette_is_closer_than_median_cut_at_every_budget() {
    for (name, rgb) in pictures() {
        for budget in [256, 64, 16] {
            let flat = QuantizeOptions::default().dither(Dither::None);
            let new = delta_e(&rgb, flat.max_colors(budget));
            let old = delta_e(&rgb, QuantizeOptions::median_cut().max_colors(budget));
            assert!(
                new < old * 0.9,
                "{name} at {budget} colours: {new:.3} vs median cut {old:.3}"
            );
        }
    }
}

#[test]
fn perceptual_metric_lowers_the_perceptual_error() {
    for (name, rgb) in pictures() {
        let base = QuantizeOptions::default()
            .dither(Dither::None)
            .max_colors(64);
        let lab = delta_e(&rgb, base);
        let rgb_metric = delta_e(&rgb, base.metric(ColorMetric::Rgb));
        assert!(lab < rgb_metric, "{name}: {lab:.3} vs {rgb_metric:.3}");
    }
}

#[test]
fn kmeans_rounds_never_hurt() {
    let rgb = synth::photo(W, H, 4);
    let base = QuantizeOptions::default()
        .dither(Dither::None)
        .max_colors(32);
    let none = delta_e(&rgb, base.palette_refine_iterations(0));
    let some = delta_e(&rgb, base.palette_refine_iterations(8));
    assert!(some < none, "{some:.3} vs {none:.3}");
}

#[test]
fn pictures_within_the_budget_are_exact_under_every_setting() {
    let rgb = synth::flat_art(W, H);
    for dither in [
        Dither::None,
        Dither::FloydSteinberg,
        Dither::Atkinson,
        Dither::OrderedBayer8x8,
        Dither::BlueNoise,
    ] {
        for method in [PaletteMethod::VarianceSplit, PaletteMethod::MedianCut] {
            for metric in [ColorMetric::Perceptual, ColorMetric::Rgb] {
                let opts = QuantizeOptions::default()
                    .dither(dither)
                    .method(method)
                    .metric(metric);
                let q = quantize_rgb_with_options(&rgb, W, H, opts).unwrap();
                assert_eq!(render(&q), rgb, "{dither:?} {method:?} {metric:?}");
                if method == PaletteMethod::VarianceSplit {
                    assert!(q.palette.len() <= 12, "one entry per colour");
                }
            }
        }
    }
}

#[test]
fn equal_colour_sets_give_equal_tables() {
    let a = synth::flat_art(W, H);
    let mut b = a.clone();
    // The same colours met in the opposite order.
    b.as_chunks_mut::<3>().0.reverse();
    let qa = quantize_rgb_with_options(&a, W, H, QuantizeOptions::default()).unwrap();
    let qb = quantize_rgb_with_options(&b, W, H, QuantizeOptions::default()).unwrap();
    assert_eq!(qa.palette, qb.palette);
}

#[test]
fn every_dither_beats_flat_mapping_after_a_low_pass_on_a_ramp() {
    let rgb = synth::gradient(W, H);
    let base = QuantizeOptions::default().max_colors(24);
    let flat = delta_e_low_pass(&rgb, base.dither(Dither::None));
    for dither in [
        Dither::FloydSteinberg,
        Dither::Stucki,
        Dither::OrderedBayer8x8,
        Dither::BlueNoise,
    ] {
        let d = delta_e_low_pass(&rgb, base.dither(dither));
        assert!(d < flat * 0.75, "{dither:?}: {d:.3} vs flat {flat:.3}");
    }
}

#[test]
fn dither_strength_scales_between_flat_and_full() {
    let rgb = synth::gradient(W, H);
    for dither in [Dither::FloydSteinberg, Dither::BlueNoise] {
        let base = QuantizeOptions::default().max_colors(24).dither(dither);
        let flat = quantize_rgb_with_options(&rgb, W, H, base.dither(Dither::None)).unwrap();
        let zero = quantize_rgb_with_options(&rgb, W, H, base.dither_strength(0)).unwrap();
        assert_eq!(zero.indices, flat.indices, "{dither:?}: strength 0 is flat");
        let changed = |strength: u8| {
            let q = quantize_rgb_with_options(&rgb, W, H, base.dither_strength(strength)).unwrap();
            q.indices
                .iter()
                .zip(&flat.indices)
                .filter(|(a, b)| a != b)
                .count()
        };
        let (half, full) = (changed(50), changed(100));
        assert!(0 < half && half < full, "{dither:?}: {half} / {full}");
    }
}

/// An ordered dither decides every pixel from its own colour and
/// position: where two frames agree, their index planes agree, whatever
/// happens elsewhere in the picture. Error diffusion does not have that
/// property (the change leaks along the scan).
#[test]
fn ordered_dithers_are_stable_where_the_picture_is_unchanged() {
    let a = synth::gradient(W, H);
    let mut b = a.clone();
    for y in 10..40 {
        for x in 20..70 {
            let o = (y * W + x) * 3;
            b[o..o + 3].copy_from_slice(&[255, 255, 255]);
        }
    }
    let base = QuantizeOptions::default().max_colors(24);
    let palette = quantize_rgb_with_options(&a, W, H, base).unwrap().palette;
    let differing = |dither: Dither| {
        let opts = base.dither(dither);
        let qa = remap_rgb_to_palette(&a, W, H, &palette, opts).unwrap();
        let qb = remap_rgb_to_palette(&b, W, H, &palette, opts).unwrap();
        (0..W * H)
            .filter(|&p| a[p * 3..p * 3 + 3] == b[p * 3..p * 3 + 3])
            .filter(|&p| qa.indices[p] != qb.indices[p])
            .count()
    };
    assert_eq!(differing(Dither::BlueNoise), 0);
    assert_eq!(differing(Dither::OrderedBayer8x8), 0);
    assert!(differing(Dither::FloydSteinberg) > 0);
}

/// Colours the table cannot reach must not make the diffused error grow
/// without bound: a saturated picture against a dark two-entry table
/// settles on the nearest entry instead of oscillating.
#[test]
fn error_diffusion_is_clamped_when_the_table_cannot_reach_the_source() {
    use oxideav_gif::Rgb;
    let rgb: Vec<u8> = (0..W * H).flat_map(|_| [255u8, 0, 0]).collect();
    let palette = vec![Rgb::new(0, 0, 0), Rgb::new(96, 0, 0), Rgb::new(0, 0, 96)];
    for dither in [
        Dither::FloydSteinberg,
        Dither::JarvisJudiceNinke,
        Dither::Atkinson,
    ] {
        let q = remap_rgb_to_palette(
            &rgb,
            W,
            H,
            &palette,
            QuantizeOptions::default().dither(dither),
        )
        .unwrap();
        assert!(q.indices.iter().all(|&i| i == 1), "{dither:?}");
    }
}

#[test]
fn dither_leaves_table_colours_alone() {
    // Half of the picture is two exact table colours, half a ramp that
    // needs dithering: the exact half must come out untouched.
    let mut rgb = synth::gradient(W, H);
    for y in 0..H {
        for x in 0..W / 2 {
            let c = if (x / 8 + y / 8) % 2 == 0 {
                [20, 20, 30]
            } else {
                [240, 240, 235]
            };
            rgb[(y * W + x) * 3..(y * W + x) * 3 + 3].copy_from_slice(&c);
        }
    }
    for dither in [Dither::BlueNoise, Dither::OrderedBayer8x8] {
        let q = quantize_rgb_with_options(
            &rgb,
            W,
            H,
            QuantizeOptions::default().max_colors(32).dither(dither),
        )
        .unwrap();
        let out = render(&q);
        for y in 0..H {
            for x in 0..W / 2 {
                let o = (y * W + x) * 3;
                let d: i32 = (0..3)
                    .map(|c| (out[o + c] as i32 - rgb[o + c] as i32).abs())
                    .sum();
                assert!(
                    d <= 6,
                    "{dither:?} at ({x},{y}): {:?} vs {:?}",
                    &out[o..o + 3],
                    &rgb[o..o + 3]
                );
            }
        }
    }
}
