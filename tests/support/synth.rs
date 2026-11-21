//! Deterministic synthetic pictures and animations for the encoder
//! quality / size / speed measurements, plus the colour-difference
//! measures those report. Nothing here is committed as a fixture: every
//! buffer is generated from a seeded integer generator.
#![allow(dead_code)]

/// 64-bit xorshift-multiply generator (deterministic across platforms).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }
    /// Uniform in `0.0..1.0`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }
    pub fn below(&mut self, n: u32) -> u32 {
        self.next_u32() % n.max(1)
    }
}

/// Smooth value noise on a `cell`-pixel lattice.
struct Noise {
    gw: usize,
    gh: usize,
    cell: f32,
    grid: Vec<f32>,
}

impl Noise {
    fn new(w: usize, h: usize, cell: usize, rng: &mut Rng) -> Self {
        let gw = w / cell + 3;
        let gh = h / cell + 3;
        let grid = (0..gw * gh).map(|_| rng.unit()).collect();
        Noise {
            gw,
            gh,
            cell: cell as f32,
            grid,
        }
    }
    fn at(&self, x: f32, y: f32) -> f32 {
        let fx = x / self.cell;
        let fy = y / self.cell;
        let (ix, iy) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - ix, fy - iy);
        let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
        let ix = (ix as usize).min(self.gw - 2);
        let iy = (iy as usize).min(self.gh - 2);
        let g = |x: usize, y: usize| self.grid[y * self.gw + x];
        let a = g(ix, iy) + (g(ix + 1, iy) - g(ix, iy)) * sx;
        let b = g(ix, iy + 1) + (g(ix + 1, iy + 1) - g(ix, iy + 1)) * sx;
        a + (b - a) * sy
    }
}

fn to_u8(v: f32) -> u8 {
    (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// Photograph-like picture: a sky gradient with a soft highlight over a
/// textured multi-hue ground, with fine grain. Tens of thousands of
/// distinct colours.
pub fn photo(w: usize, h: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let big = [
        Noise::new(w, h, 96, &mut rng),
        Noise::new(w, h, 96, &mut rng),
        Noise::new(w, h, 96, &mut rng),
    ];
    let mid = Noise::new(w, h, 24, &mut rng);
    let fine = Noise::new(w, h, 5, &mut rng);
    let horizon = h as f32 * 0.42;
    let (sunx, suny) = (w as f32 * 0.7, h as f32 * 0.18);
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let (xf, yf) = (x as f32, y as f32);
            let wobble = (mid.at(xf, 0.0) - 0.5) * h as f32 * 0.12;
            let grain = (rng.unit() - 0.5) * 0.025;
            let rgb = if yf < horizon + wobble {
                let t = yf / (horizon + wobble).max(1.0);
                let d = ((xf - sunx).powi(2) + (yf - suny).powi(2)).sqrt() / w as f32;
                let glow = (1.0 - d * 3.0).max(0.0).powi(2);
                let cloud = (mid.at(xf * 0.6, yf * 1.7) - 0.55).max(0.0) * 1.2;
                [
                    0.20 + 0.45 * t + 0.5 * glow + cloud,
                    0.42 + 0.38 * t + 0.4 * glow + cloud,
                    0.85 - 0.05 * t + 0.1 * glow + cloud * 0.8,
                ]
            } else {
                let tex = fine.at(xf, yf) * 0.25 + mid.at(xf, yf) * 0.35;
                let shade = 0.35 + 0.65 * (yf - horizon) / (h as f32 - horizon);
                [
                    (0.25 + 0.5 * big[0].at(xf, yf) + tex * 0.5) * shade,
                    (0.30 + 0.5 * big[1].at(xf, yf) + tex) * shade,
                    (0.10 + 0.35 * big[2].at(xf, yf) + tex * 0.3) * shade,
                ]
            };
            for c in rgb {
                out.push(to_u8(c + grain));
            }
        }
    }
    out
}

/// Smooth colour ramps: a four-corner blend with a soft radial spot.
pub fn gradient(w: usize, h: usize) -> Vec<u8> {
    let corners = [
        [0.95f32, 0.25, 0.20],
        [0.15, 0.30, 0.90],
        [0.10, 0.75, 0.45],
        [0.98, 0.90, 0.35],
    ];
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let u = x as f32 / (w - 1).max(1) as f32;
            let v = y as f32 / (h - 1).max(1) as f32;
            let d = ((u - 0.35).powi(2) + (v - 0.6).powi(2)).sqrt();
            let spot = (1.0 - d * 2.2).max(0.0).powi(2) * 0.5;
            for c in 0..3 {
                let top = corners[0][c] + (corners[1][c] - corners[0][c]) * u;
                let bot = corners[2][c] + (corners[3][c] - corners[2][c]) * u;
                out.push(to_u8(top + (bot - top) * v + spot));
            }
        }
    }
    out
}

const FLAT: [[u8; 3]; 12] = [
    [250, 244, 230],
    [30, 30, 46],
    [230, 57, 70],
    [29, 53, 87],
    [69, 123, 157],
    [168, 218, 220],
    [255, 183, 3],
    [251, 133, 0],
    [42, 157, 143],
    [38, 70, 83],
    [233, 196, 106],
    [140, 80, 160],
];

fn put(buf: &mut [u8], w: usize, x: usize, y: usize, c: [u8; 3]) {
    let o = (y * w + x) * 3;
    if o + 3 <= buf.len() && x < w {
        buf[o..o + 3].copy_from_slice(&c);
    }
}

fn fill_rect(buf: &mut [u8], w: usize, h: usize, r: (i32, i32, i32, i32), c: [u8; 3]) {
    let (x0, y0, rw, rh) = r;
    for y in y0.max(0)..(y0 + rh).min(h as i32) {
        for x in x0.max(0)..(x0 + rw).min(w as i32) {
            put(buf, w, x as usize, y as usize, c);
        }
    }
}

fn fill_disc(buf: &mut [u8], w: usize, h: usize, cx: i32, cy: i32, rad: i32, c: [u8; 3]) {
    for y in (cy - rad).max(0)..(cy + rad + 1).min(h as i32) {
        for x in (cx - rad).max(0)..(cx + rad + 1).min(w as i32) {
            if (x - cx).pow(2) + (y - cy).pow(2) <= rad * rad {
                put(buf, w, x as usize, y as usize, c);
            }
        }
    }
}

/// Flat-colour artwork: a dozen colours, hard edges, stripes.
pub fn flat_art(w: usize, h: usize) -> Vec<u8> {
    let mut buf = vec![0u8; w * h * 3];
    cartoon_scene(&mut buf, w, h, 0);
    buf
}

fn cartoon_scene(buf: &mut [u8], w: usize, h: usize, t: usize) {
    let (wi, hi) = (w as i32, h as i32);
    fill_rect(buf, w, h, (0, 0, wi, hi), FLAT[0]);
    fill_rect(buf, w, h, (0, hi * 2 / 3, wi, hi / 3), FLAT[8]);
    for k in 0..8 {
        let x = k * wi / 8;
        let c = if k % 2 == 0 { FLAT[5] } else { FLAT[4] };
        fill_rect(buf, w, h, (x, 0, wi / 16, hi / 6), c);
    }
    fill_rect(buf, w, h, (wi / 10, hi / 3, wi / 4, hi / 3), FLAT[3]);
    fill_rect(
        buf,
        w,
        h,
        (wi / 10 + 8, hi / 3 + 8, wi / 4 - 16, hi / 3 - 16),
        FLAT[10],
    );
    fill_disc(buf, w, h, wi * 3 / 4, hi / 4, hi / 9, FLAT[6]);
    fill_disc(buf, w, h, wi * 3 / 4, hi / 4, hi / 14, FLAT[7]);
    // Moving parts: a bouncing ball and a sliding block.
    let period = 40usize;
    let ph = (t % period) as f32 / period as f32;
    let bx = (wi as f32 * (0.15 + 0.7 * ph)) as i32;
    let by = (hi as f32 * (0.62 - 0.35 * (ph * std::f32::consts::PI * 2.0).sin().abs())) as i32;
    fill_disc(buf, w, h, bx, by, hi / 12, FLAT[2]);
    fill_disc(buf, w, h, bx - hi / 40, by - hi / 40, hi / 40, FLAT[0]);
    let sx = (wi as f32 * (0.8 - 0.6 * ph)) as i32;
    fill_rect(buf, w, h, (sx, hi * 3 / 4, wi / 8, hi / 12), FLAT[11]);
    fill_rect(
        buf,
        w,
        h,
        (sx + 4, hi * 3 / 4 + 4, wi / 8 - 8, hi / 12 - 8),
        FLAT[1],
    );
}

fn glyph_row(
    buf: &mut [u8],
    (w, h): (usize, usize),
    (x0, y0): (usize, usize),
    n: usize,
    ink: [u8; 3],
    rng: &mut Rng,
) {
    for g in 0..n {
        if rng.below(6) == 0 {
            continue; // word gap
        }
        let bits = rng.next_u32();
        for gy in 0..7 {
            for gx in 0..5 {
                if bits >> ((gy * 5 + gx) % 32) & 1 == 1 {
                    let (x, y) = (x0 + g * 7 + gx, y0 + gy);
                    if x < w && y < h {
                        put(buf, w, x, y, ink);
                    }
                }
            }
        }
    }
}

/// Screen-capture-like picture: window chrome with a ramp title bar,
/// glyph-like text in several ink colours, an inset photograph.
pub fn screen(w: usize, h: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let mut buf = vec![0u8; w * h * 3];
    let (wi, hi) = (w as i32, h as i32);
    fill_rect(&mut buf, w, h, (0, 0, wi, hi), [236, 239, 244]);
    for y in 0..22.min(h) {
        for x in 0..w {
            let t = x as f32 / w as f32;
            put(
                &mut buf,
                w,
                x,
                y,
                [
                    to_u8(0.18 + 0.2 * t),
                    to_u8(0.22 + 0.25 * t),
                    to_u8(0.45 + 0.3 * t),
                ],
            );
        }
    }
    fill_rect(&mut buf, w, h, (0, 22, wi / 5, hi - 22), [46, 52, 64]);
    let inks = [
        [40, 44, 52],
        [191, 97, 106],
        [94, 129, 172],
        [163, 190, 140],
        [180, 142, 173],
    ];
    let mut y = 30usize;
    while y + 9 < h {
        let ink = inks[rng.below(inks.len() as u32) as usize];
        let n = 6 + rng.below(((w * 4 / 5) / 7).saturating_sub(10) as u32) as usize;
        glyph_row(&mut buf, (w, h), (w / 5 + 8, y), n, ink, &mut rng);
        glyph_row(
            &mut buf,
            (w, h),
            (6, y),
            (w / 5) / 9,
            [216, 222, 233],
            &mut rng,
        );
        y += 11;
    }
    // Inset photograph (pushes the picture past 256 colours).
    let (pw, ph) = (w / 4, h / 4);
    if pw > 8 && ph > 8 {
        let p = photo(pw, ph, seed ^ 0x51);
        for py in 0..ph {
            for px in 0..pw {
                let o = (py * pw + px) * 3;
                put(
                    &mut buf,
                    w,
                    w - pw - 12 + px,
                    h - ph - 12 + py,
                    [p[o], p[o + 1], p[o + 2]],
                );
            }
        }
    }
    buf
}

/// RGB → RGBA (opaque).
pub fn rgba(rgb: &[u8]) -> Vec<u8> {
    rgb.as_chunks::<3>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2], 255])
        .collect()
}

/// Screen-recording-like animation: a static screen with a moving
/// pointer and a line of text being typed; every fourth frame repeats
/// the one before it exactly.
pub fn anim_screen(w: usize, h: usize, n: usize) -> Vec<Vec<u8>> {
    let base = screen(w, h, 7);
    let mut typed = base.clone();
    let mut rng = Rng::new(99);
    let mut frames: Vec<Vec<u8>> = Vec::with_capacity(n);
    for t in 0..n {
        if t % 4 == 3 {
            let prev = frames[t - 1].clone();
            frames.push(prev);
            continue;
        }
        glyph_row(
            &mut typed,
            (w, h),
            (w / 5 + 8 + (t % 40) * 7, h / 2 + (t / 40) * 11),
            1,
            [20, 20, 20],
            &mut rng,
        );
        let mut f = typed.clone();
        let (cx, cy) = ((w / 4 + t * 5) % w, (h / 3 + t * 3) % h);
        for dy in 0..12 {
            for dx in 0..(12 - dy) {
                let c = if dx == 0 || dy == 0 || dx + dy == 11 {
                    [0, 0, 0]
                } else {
                    [255, 255, 255]
                };
                if cx + dx < w && cy + dy < h {
                    put(&mut f, w, cx + dx, cy + dy, c);
                }
            }
        }
        frames.push(rgba(&f));
    }
    frames
}

/// Cartoon-like animation: flat shapes moving over a static flat scene.
pub fn anim_cartoon(w: usize, h: usize, n: usize) -> Vec<Vec<u8>> {
    (0..n)
        .map(|t| {
            let mut buf = vec![0u8; w * h * 3];
            cartoon_scene(&mut buf, w, h, t);
            rgba(&buf)
        })
        .collect()
}

/// Video-like animation: a slow pan across a photograph with fresh
/// grain on every frame.
pub fn anim_video(w: usize, h: usize, n: usize) -> Vec<Vec<u8>> {
    let wide = photo(w + 2 * n, h, 3);
    let ww = w + 2 * n;
    let mut rng = Rng::new(5);
    (0..n)
        .map(|t| {
            let mut f = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                for x in 0..w {
                    let o = (y * ww + x + 2 * t) * 3;
                    for c in 0..3 {
                        let g = rng.below(5) as i32 - 2;
                        f.push((wide[o + c] as i32 + g).clamp(0, 255) as u8);
                    }
                    f.push(255);
                }
            }
            f
        })
        .collect()
}

// ---------------------------------------------------------------------
// Measures.
// ---------------------------------------------------------------------

/// Peak signal-to-noise ratio over all samples of two 8-bit buffers.
pub fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let se: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (x as f64 - y as f64).powi(2))
        .sum();
    if se == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 * a.len() as f64 / se).log10()
}

/// CIE 1976 L*a*b* of an 8-bit sRGB triple (D65 white).
pub fn lab(p: [u8; 3]) -> [f64; 3] {
    let lin = |v: u8| {
        let c = v as f64 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(p[0]), lin(p[1]), lin(p[2]));
    let x = (0.4124564 * r + 0.3575761 * g + 0.1804375 * b) / 0.95047;
    let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
    let z = (0.0193339 * r + 0.1191920 * g + 0.9503041 * b) / 1.08883;
    let f = |t: f64| {
        if t > 216.0 / 24389.0 {
            t.cbrt()
        } else {
            (24389.0 / 27.0 * t + 16.0) / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Mean CIE 1976 colour difference (ΔE*ab) between two RGB pictures.
pub fn mean_delta_e(a: &[u8], b: &[u8]) -> f64 {
    let mut cache: std::collections::HashMap<[u8; 3], [f64; 3]> = std::collections::HashMap::new();
    let mut get = |p: &[u8]| {
        *cache
            .entry([p[0], p[1], p[2]])
            .or_insert_with(|| lab([p[0], p[1], p[2]]))
    };
    let mut sum = 0.0;
    for (p, q) in a.as_chunks::<3>().0.iter().zip(b.as_chunks::<3>().0) {
        let (x, y) = (get(p), get(q));
        sum += ((x[0] - y[0]).powi(2) + (x[1] - y[1]).powi(2) + (x[2] - y[2]).powi(2)).sqrt();
    }
    sum / (a.len() / 3) as f64
}

/// 5×5 binomial low-pass of an RGB picture (edge-clamped), the stand-in
/// for viewing distance: a dither pattern that averages to the source
/// colour scores well after it, a flat band does not.
pub fn low_pass(rgb: &[u8], w: usize, h: usize) -> Vec<u8> {
    const K: [u32; 5] = [1, 4, 6, 4, 1];
    let mut tmp = vec![0u32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let mut s = 0;
                for (k, wt) in K.iter().enumerate() {
                    let xx = (x + k).saturating_sub(2).min(w - 1);
                    s += wt * rgb[(y * w + xx) * 3 + c] as u32;
                }
                tmp[(y * w + x) * 3 + c] = s;
            }
        }
    }
    let mut out = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let mut s = 0;
                for (k, wt) in K.iter().enumerate() {
                    let yy = (y + k).saturating_sub(2).min(h - 1);
                    s += wt * tmp[(yy * w + x) * 3 + c];
                }
                out[(y * w + x) * 3 + c] = ((s + 128) / 256) as u8;
            }
        }
    }
    out
}

/// Mean ΔE*ab after the 5×5 low-pass of both pictures.
pub fn mean_delta_e_low_pass(a: &[u8], b: &[u8], w: usize, h: usize) -> f64 {
    mean_delta_e(&low_pass(a, w, h), &low_pass(b, w, h))
}

/// Median wall time of `runs` calls, in seconds.
pub fn median_secs<T>(runs: usize, mut f: impl FnMut() -> T) -> f64 {
    let mut times: Vec<f64> = (0..runs.max(1))
        .map(|_| {
            let t = std::time::Instant::now();
            std::hint::black_box(f());
            t.elapsed().as_secs_f64()
        })
        .collect();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    times[times.len() / 2]
}
