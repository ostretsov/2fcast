//! Camera look: "Smooth skin", a cheap beauty filter, and "Warmth", a warm
//! colour grade, for the camera picture (I420).
//!
//! An edge-preserving guided filter on the brightness smooths small texture
//! (pores, blemishes, noise) but keeps edges such as eyes, brows, lips and
//! hair, which vary much more.  It only applies where the colour looks like
//! skin, so the background and clothes stay sharp; no face detection.
//!
//! Cheap by design: the filter coefficients are computed at half resolution
//! (the chroma planes' size, where the skin mask comes from anyway) from box
//! blurs whose cost does not depend on the radius, and applied at full
//! resolution with bilinear interpolation.  About 1-2 ms per 960x540 frame on
//! one core in an optimized build.

/// At full warmth: blue-difference (Cb) down and red-difference (Cr) up by
/// this many levels (towards orange, like a lower colour temperature) ...
const WARM_CB: f32 = -10.0;
const WARM_CR: f32 = 7.0;
/// ... and colours this much more saturated.
const WARM_SAT: f32 = 0.12;

/// Lookup tables for the Cb and Cr planes giving `warmth` (0 none .. 1).
pub fn warm_tables(warmth: f32) -> ([u8; 256], [u8; 256]) {
    let w = warmth.clamp(0.0, 1.0);
    let table = |shift: f32| {
        let mut t = [0u8; 256];
        for (i, out) in t.iter_mut().enumerate() {
            let c = 128.0 + (i as f32 - 128.0) * (1.0 + WARM_SAT * w) + shift * w;
            *out = (c.clamp(0.0, 255.0) + 0.5) as u8;
        }
        t
    };
    (table(WARM_CB), table(WARM_CR))
}

/// Applies one table from [`warm_tables`] to a chroma plane in place.
pub fn warm(plane: &mut [u8], table: &[u8; 256]) {
    plane.iter_mut().for_each(|c| *c = table[*c as usize]);
}

/// Box-blur radius at half resolution (about 6 pixels at full resolution).
const RADIUS: usize = 3;
/// Variance (in 8-bit levels²) below which detail counts as texture to
/// smooth; edges vary much more and are kept.
const EPS: f32 = 14.0 * 14.0;
/// At full strength skin also gets this much lighter (fraction towards white).
const LIFT: f32 = 0.04;

/// Reusable buffers for one picture size.
#[derive(Default)]
pub struct SkinFilter {
    w: usize,
    h: usize,
    /// Half-resolution brightness, then `a` of the guided filter.
    a: Vec<f32>,
    /// Half-resolution brightness squared, then `b`.
    b: Vec<f32>,
    mean: Vec<f32>,
    mean_sq: Vec<f32>,
    mask: Vec<f32>,
    tmp: Vec<f32>,
    /// Coefficients of the current row (a, b, mask), interpolated vertically.
    rows: Vec<f32>,
}

/// One I420 picture: full-resolution `y`, half-resolution `u` and `v`.
pub struct Planes<'a> {
    pub width: usize,
    pub height: usize,
    pub y: &'a mut [u8],
    pub y_stride: usize,
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub uv_stride: usize,
}

/// How much a chroma pair looks like skin, 0..1 (soft edges, so the effect
/// fades in instead of making blotches).  Classic YCbCr skin box:
/// Cb 77..127, Cr 133..173, fading out over its outer 8 levels.
pub fn skin_weight(cb: u8, cr: u8) -> f32 {
    fn ramp(x: f32, lo: f32, hi: f32) -> f32 {
        const SOFT: f32 = 8.0;
        ((x - lo).min(hi - x) * (1.0 / SOFT)).clamp(0.0, 1.0)
    }
    ramp(cb as f32, 77.0, 127.0) * ramp(cr as f32, 133.0, 173.0)
}

/// Upsamples a half-resolution row to `dst` (full width) bilinearly, times
/// `k`: pixel 2j+1 = ¾·src[j] + ¼·src[j+1], pixel 2j+2 = ¼·src[j] + ¾·src[j+1].
fn widen(dst: &mut [f32], src: &[f32], k: f32) {
    let last = src[src.len() - 1] * k;
    dst[0] = src[0] * k;
    for (pair, s) in dst[1..].chunks_mut(2).zip(src.windows(2)) {
        let (a, b) = (s[0] * k, s[1] * k);
        pair[0] = a + (b - a) * 0.25;
        if let Some(p) = pair.get_mut(1) {
            *p = a + (b - a) * 0.75;
        }
    }
    let filled = 1 + 2 * (src.len() - 1);
    for d in dst.iter_mut().skip(filled) {
        *d = last;
    }
}

/// Mean over a (2r+1)² window, clamped at the borders.  `tmp` is scratch.
fn box_blur(src: &[f32], dst: &mut [f32], tmp: &mut [f32], w: usize, h: usize, r: usize) {
    // Horizontal running sums into `tmp`, then vertical into `dst`.
    let full = 1.0 / (2 * r + 1) as f32;
    for (row, out) in src.chunks_exact(w).zip(tmp.chunks_exact_mut(w)).take(h) {
        if w <= 2 * r + 1 {
            // Tiny picture: every window is clamped.
            for (x, o) in out.iter_mut().enumerate() {
                let win = &row[x.saturating_sub(r)..(x + r + 1).min(w)];
                *o = win.iter().sum::<f32>() / win.len() as f32;
            }
            continue;
        }
        let mut sum: f32 = row[..r].iter().sum();
        // Left edge: the window grows.
        for x in 0..=r {
            sum += row[x + r];
            out[x] = sum / (x + r + 1) as f32;
        }
        // Middle: full windows, add one and drop one.
        for x in r + 1..w - r {
            sum += row[x + r] - row[x - r - 1];
            out[x] = sum * full;
        }
        // Right edge: the window shrinks.
        for x in w - r..w {
            sum -= row[x - r - 1];
            out[x] = sum / (w - x + r) as f32;
        }
    }
    let mut sum = vec![0f32; w];
    let mut n = (r + 1).min(h);
    for row in tmp.chunks_exact(w).take(n) {
        sum.iter_mut().zip(row).for_each(|(s, v)| *s += v);
    }
    for (y, out) in dst.chunks_exact_mut(w).enumerate().take(h) {
        let k = 1.0 / n as f32;
        out.iter_mut().zip(&sum).for_each(|(o, s)| *o = s * k);
        if y + r + 1 < h {
            let add = &tmp[(y + r + 1) * w..(y + r + 2) * w];
            sum.iter_mut().zip(add).for_each(|(s, v)| *s += v);
            n += 1;
        }
        if y >= r {
            let sub = &tmp[(y - r) * w..(y - r + 1) * w];
            sum.iter_mut().zip(sub).for_each(|(s, v)| *s -= v);
            n -= 1;
        }
    }
}

impl SkinFilter {
    pub fn new() -> SkinFilter {
        SkinFilter::default()
    }

    fn resize(&mut self, w: usize, h: usize) {
        if (w, h) != (self.w, self.h) {
            (self.w, self.h) = (w, h);
            for v in [
                &mut self.a,
                &mut self.b,
                &mut self.mean,
                &mut self.mean_sq,
                &mut self.mask,
                &mut self.tmp,
            ] {
                *v = vec![0.0; w * h];
            }
        }
    }

    /// Smooths the skin in `p` in place; `strength` 0 (off) .. 1.
    pub fn process(&mut self, p: Planes, strength: f32) {
        let strength = strength.clamp(0.0, 1.0);
        let (hw, hh) = (p.width.div_ceil(2), p.height.div_ceil(2));
        if strength == 0.0 || hw < 2 || hh < 2 {
            return;
        }
        self.resize(hw, hh);
        // Half-resolution brightness (2x2 average) and its square; skin mask.
        for y in 0..hh {
            let r0 = (2 * y).min(p.height - 1) * p.y_stride;
            let r1 = (2 * y + 1).min(p.height - 1) * p.y_stride;
            let (top, bot) = (&p.y[r0..r0 + p.width], &p.y[r1..r1 + p.width]);
            let out = y * hw..(y + 1) * hw;
            let (a, b) = (&mut self.a[out.clone()], &mut self.b[out.clone()]);
            for (x, (a, b)) in a.iter_mut().zip(b.iter_mut()).enumerate() {
                let (x0, x1) = (2 * x, (2 * x + 1).min(p.width - 1));
                let i = (top[x0] as f32 + top[x1] as f32 + bot[x0] as f32 + bot[x1] as f32) * 0.25;
                *a = i;
                *b = i * i;
            }
            let c = y * p.uv_stride;
            let (u, v) = (&p.u[c..c + hw], &p.v[c..c + hw]);
            for ((m, &u), &v) in self.tmp[out].iter_mut().zip(u).zip(v) {
                *m = skin_weight(u, v);
            }
        }
        // Soften the mask's edges: it comes from noisy chroma.
        let (w, h) = (hw, hh);
        std::mem::swap(&mut self.tmp, &mut self.mean);
        box_blur(&self.mean, &mut self.mask, &mut self.tmp, w, h, 2);
        box_blur(&self.a, &mut self.mean, &mut self.tmp, w, h, RADIUS);
        box_blur(&self.b, &mut self.mean_sq, &mut self.tmp, w, h, RADIUS);
        // Guided filter (the picture guides itself): q = a·I + b.
        for k in 0..w * h {
            let m = self.mean[k];
            let var = (self.mean_sq[k] - m * m).max(0.0);
            let a = var / (var + EPS);
            self.a[k] = a;
            self.b[k] = m - a * m;
        }
        box_blur(&self.a, &mut self.mean, &mut self.tmp, w, h, RADIUS);
        box_blur(&self.b, &mut self.mean_sq, &mut self.tmp, w, h, RADIUS);
        // Apply at full resolution, coefficients interpolated bilinearly.
        // Half-resolution sample k sits at full-resolution 2k+0.5, so full
        // row/column 2k mixes k-1 and k by 1/4 : 3/4, and 2k+1 mixes k and
        // k+1 by 3/4 : 1/4.  Rows are interpolated first (vertically, then
        // widened), then one branch-free pass per row, which vectorizes.
        let fw = p.width;
        self.rows.resize(3 * w + 3 * fw, 0.0);
        let (half_rows, full_rows) = self.rows.split_at_mut(3 * w);
        let (ra, rest) = half_rows.split_at_mut(w);
        let (rb, rm) = rest.split_at_mut(w);
        let (fa, rest) = full_rows.split_at_mut(fw);
        let (fb, fs) = rest.split_at_mut(fw);
        let neighbours = |i: usize, n: usize| {
            let k = i / 2;
            if i.is_multiple_of(2) {
                (k.saturating_sub(1), k, if k == 0 { 1.0 } else { 0.75 })
            } else {
                (k, (k + 1).min(n - 1), 0.25)
            }
        };
        for y in 0..p.height {
            let (y0, y1, t) = neighbours(y, h);
            let top = y0 * w..y0 * w + w;
            let bot = y1 * w..y1 * w + w;
            let lerp_rows = |dst: &mut [f32], v: &[f32]| {
                for ((d, a), b) in dst.iter_mut().zip(&v[top.clone()]).zip(&v[bot.clone()]) {
                    *d = a + (b - a) * t;
                }
            };
            lerp_rows(rm, &self.mask);
            if rm.iter().all(|&m| m * strength < 0.01) {
                continue;
            }
            lerp_rows(ra, &self.mean);
            lerp_rows(rb, &self.mean_sq);
            widen(fa, ra, 1.0);
            widen(fb, rb, 1.0);
            widen(fs, rm, strength);
            let row = &mut p.y[y * p.y_stride..y * p.y_stride + fw];
            for (((px, &a), &b), &s) in row.iter_mut().zip(&*fa).zip(&*fb).zip(&*fs) {
                let i = *px as f32;
                let out = i + s * (a * i + b - i);
                let out = out + s * LIFT * (255.0 - out);
                *px = (out.clamp(0.0, 255.0) + 0.5) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 96;
    const H: usize = 64;
    /// A typical skin chroma (Cb, Cr).
    const SKIN: (u8, u8) = (105, 150);
    const BLUE: (u8, u8) = (170, 110);

    fn noise(i: usize) -> f32 {
        // Deterministic ±6 levels of "pore" texture.
        let x = (i as u32).wrapping_mul(2_654_435_761).rotate_left(13);
        (x % 13) as f32 - 6.0
    }

    /// Left half: skin with texture and a hard edge (an eyebrow) in the
    /// middle row band; right half: textured blue background.
    fn picture() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut y = vec![0u8; W * H];
        for r in 0..H {
            for c in 0..W {
                let base = if (28..36).contains(&r) && c < W / 2 {
                    40.0
                } else {
                    170.0
                };
                y[r * W + c] = (base + noise(r * W + c)) as u8;
            }
        }
        let (mut u, mut v) = (vec![0u8; W * H / 4], vec![0u8; W * H / 4]);
        for r in 0..H / 2 {
            for c in 0..W / 2 {
                let (cb, cr) = if c < W / 4 { SKIN } else { BLUE };
                u[r * W / 2 + c] = cb;
                v[r * W / 2 + c] = cr;
            }
        }
        (y, u, v)
    }

    fn run(strength: f32) -> (Vec<u8>, Vec<u8>) {
        let (mut y, u, v) = picture();
        let before = y.clone();
        SkinFilter::new().process(
            Planes {
                width: W,
                height: H,
                y: &mut y,
                y_stride: W,
                u: &u,
                v: &v,
                uv_stride: W / 2,
            },
            strength,
        );
        (before, y)
    }

    /// Standard deviation of the brightness in a block.
    fn spread(y: &[u8], rows: std::ops::Range<usize>, cols: std::ops::Range<usize>) -> f32 {
        let vals: Vec<f32> = rows
            .flat_map(|r| cols.clone().map(move |c| (r, c)))
            .map(|(r, c)| y[r * W + c] as f32)
            .collect();
        let mean = vals.iter().sum::<f32>() / vals.len() as f32;
        (vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / vals.len() as f32).sqrt()
    }

    #[test]
    fn skin_colours() {
        assert_eq!(skin_weight(SKIN.0, SKIN.1), 1.0);
        assert_eq!(skin_weight(BLUE.0, BLUE.1), 0.0);
        assert_eq!(skin_weight(128, 128), 0.0, "grey is not skin");
        let edge = skin_weight(80, 150);
        assert!(edge > 0.0 && edge < 1.0, "soft edge: {edge}");
    }

    #[test]
    fn smooths_skin_texture_only() {
        let (before, after) = run(1.0);
        let skin = |y: &[u8]| spread(y, 6..22, 6..40);
        assert!(
            skin(&after) < skin(&before) * 0.5,
            "skin texture {} -> {}",
            skin(&before),
            skin(&after)
        );
        // Background untouched (well away from the soft mask edge).
        for r in 0..H {
            for c in W / 2 + 12..W {
                assert_eq!(after[r * W + c], before[r * W + c], "at {r},{c}");
            }
        }
    }

    #[test]
    fn keeps_edges_sharp() {
        let (_, after) = run(1.0);
        let mean = |rows: std::ops::Range<usize>| {
            let n = (rows.len() * 30) as f32;
            rows.flat_map(|r| (6..36).map(move |c| (r, c)))
                .map(|(r, c)| after[r * W + c] as f32)
                .sum::<f32>()
                / n
        };
        // The dark band (40) stays dark and the skin around it bright (170+).
        assert!(mean(30..34) < 70.0, "band {}", mean(30..34));
        assert!(mean(18..24) > 165.0, "skin {}", mean(18..24));
    }

    #[test]
    fn strength_zero_changes_nothing() {
        let (before, after) = run(0.0);
        assert_eq!(before, after);
        let (_, half) = run(0.5);
        let (_, full) = run(1.0);
        let skin = |y: &[u8]| spread(y, 6..22, 6..40);
        assert!(skin(&full) < skin(&half));
    }

    #[test]
    fn odd_sizes_and_strides_are_safe() {
        let (w, h, stride) = (7, 5, 10);
        let mut y = vec![120u8; stride * h];
        let (u, v) = (vec![SKIN.0; 4 * 3], vec![SKIN.1; 4 * 3]);
        let mut f = SkinFilter::new();
        for _ in 0..2 {
            f.process(
                Planes {
                    width: w,
                    height: h,
                    y: &mut y,
                    y_stride: stride,
                    u: &u,
                    v: &v,
                    uv_stride: 4,
                },
                1.0,
            );
        }
        // Flat skin only gets the slight lift; padding bytes stay.
        assert!(y[0] > 120 && y[0] < 135, "{}", y[0]);
        assert_eq!(y[w], 120, "stride padding untouched");
    }

    #[test]
    fn warmth_shifts_towards_orange() {
        let none = warm_tables(0.0);
        assert!((0..256).all(|i| none.0[i] == i as u8 && none.1[i] == i as u8));
        let full = warm_tables(1.0);
        // Grey turns slightly orange: less blue, more red.
        assert_eq!((full.0[128], full.1[128]), (118, 135));
        // Colours get a bit more saturated; extremes clamp.
        assert!(full.1[160] as i32 - 135 > 160 - 128);
        assert_eq!(full.1[255], 255);
        assert_eq!(full.0[0], 0);
        let (mut u, mut v) = (vec![128u8, 100], vec![128u8, 150]);
        let half = warm_tables(0.5);
        warm(&mut u, &half.0);
        warm(&mut v, &half.1);
        assert_eq!((u[0], v[0]), (123, 132));
        assert!(u[1] < 100 && v[1] > 150);
        assert_eq!(warm_tables(7.0), full, "clamped to 1");
    }

    #[test]
    fn widening_matches_bilinear() {
        let mut dst = vec![0.0; 7];
        widen(&mut dst, &[0.0, 4.0, 8.0, 12.0], 1.0);
        assert_eq!(dst, [0.0, 1.0, 3.0, 5.0, 7.0, 9.0, 11.0]);
        let mut even = vec![0.0; 8];
        widen(&mut even, &[0.0, 4.0, 8.0, 12.0], 0.5);
        assert_eq!(even, [0.0, 0.5, 1.5, 2.5, 3.5, 4.5, 5.5, 6.0]);
    }

    #[test]
    fn box_blur_means() {
        let src: Vec<f32> = (0..12).map(|i| i as f32).collect();
        let (mut dst, mut tmp) = (vec![0.0; 12], vec![0.0; 12]);
        box_blur(&src, &mut dst, &mut tmp, 4, 3, 1);
        // Centre (row 1, col 1): mean of 0,1,2,4,5,6,8,9,10.
        assert_eq!(dst[5], 5.0);
        // Corner: mean of 0,1,4,5.
        assert_eq!(dst[0], 2.5);
        // Same as a direct mean everywhere, also for wide rows and tiny ones.
        for (w, h, r) in [(11, 4, 2), (3, 2, 2), (5, 5, 0)] {
            let src: Vec<f32> = (0..w * h).map(|i| ((i * 37) % 11) as f32).collect();
            let (mut dst, mut tmp) = (vec![0.0; w * h], vec![0.0; w * h]);
            box_blur(&src, &mut dst, &mut tmp, w, h, r);
            for y in 0..h {
                for x in 0..w {
                    let mut vals = vec![];
                    for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
                        for xx in x.saturating_sub(r)..(x + r + 1).min(w) {
                            vals.push(src[yy * w + xx]);
                        }
                    }
                    let want = vals.iter().sum::<f32>() / vals.len() as f32;
                    assert!(
                        (dst[y * w + x] - want).abs() < 1e-4,
                        "{w}x{h} r{r} at {x},{y}"
                    );
                }
            }
        }
    }
}
