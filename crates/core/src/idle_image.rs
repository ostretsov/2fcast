//! A picture shown in the shared space while nobody shares (instead of
//! black), and how sharp it will look.
//!
//! The picture is scaled once, to exactly the shared space's size on the
//! 1920x1080 canvas (keeping its proportions, black bars where they differ),
//! and placed there 1:1.  Like a shared window shrunk to the tile size, it is
//! sharpest when it already has exactly that size; a smaller picture is
//! enlarged and looks blurry.

use crate::layout::Rect;

/// How the picture fits the shared space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sharpness {
    /// Exactly the size of the shared space: shown 1:1, sharpest.
    Exact,
    /// Larger: reduced (fine, a little softer than exact).
    Reduced,
    /// Smaller: enlarged, blurry.
    Enlarged,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub sharpness: Sharpness,
    /// The size the picture is scaled to (inside the shared space).
    pub shown: (u32, u32),
    /// The shared space in canvas pixels: the best picture size.
    pub best: (u32, u32),
    /// The proportions differ: black bars.
    pub bars: bool,
}

/// Size of the shared space in canvas (= recording) pixels.
pub fn tile_size(r: &Rect) -> (u32, u32) {
    (r.w.round().max(1.0) as u32, r.h.round().max(1.0) as u32)
}

/// Largest size with the picture's proportions that fits `tile`.
pub fn fit(img: (u32, u32), tile: (u32, u32)) -> (u32, u32) {
    let (iw, ih) = (img.0.max(1) as f64, img.1.max(1) as f64);
    let s = (tile.0 as f64 / iw).min(tile.1 as f64 / ih);
    let w = ((iw * s).round() as u32).clamp(1, tile.0);
    let h = ((ih * s).round() as u32).clamp(1, tile.1);
    (w, h)
}

pub fn check(img: (u32, u32), tile: (u32, u32)) -> Check {
    let shown = fit(img, tile);
    // One pixel of rounding is still the same proportions.
    let bars = shown.0 + 1 < tile.0 || shown.1 + 1 < tile.1;
    let sharpness = if shown == img {
        Sharpness::Exact
    } else if shown.0 > img.0 || shown.1 > img.1 {
        Sharpness::Enlarged
    } else {
        Sharpness::Reduced
    };
    Check {
        sharpness,
        shown,
        best: tile,
        bars,
    }
}

impl Check {
    /// Plain-words verdict for the user.
    pub fn message(&self, img: (u32, u32)) -> String {
        let (iw, ih) = img;
        let (bw, bh) = self.best;
        let mut s = match self.sharpness {
            Sharpness::Exact => {
                format!("{iw}×{ih}: exactly the shared space, shown 1:1 (sharpest).")
            }
            Sharpness::Reduced => format!(
                "{iw}×{ih} is reduced to {}×{}: fine; exactly {bw}×{bh} would be sharpest.",
                self.shown.0, self.shown.1
            ),
            Sharpness::Enlarged => format!(
                "{iw}×{ih} is too small: enlarged to {}×{}, it looks blurry. Use at least {bw}×{bh} (exactly that is sharpest).",
                self.shown.0, self.shown.1
            ),
        };
        if self.bars {
            s += &format!(" Its proportions differ from the shared space ({bw}×{bh}): black bars.");
        }
        s
    }
}

/// A frame of `tile` size in BGRx: the already scaled picture (RGB or RGBA
/// rows of `stride` bytes, `channels` 3 or 4) centered on black; transparent
/// parts become black.
pub fn compose_bgrx(
    pixels: &[u8],
    size: (u32, u32),
    stride: usize,
    channels: usize,
    tile: (u32, u32),
) -> Vec<u8> {
    let (tw, th) = (tile.0 as usize, tile.1 as usize);
    let (w, h) = ((size.0 as usize).min(tw), (size.1 as usize).min(th));
    let (ox, oy) = ((tw - w) / 2, (th - h) / 2);
    let mut out = vec![0u8; tw * th * 4];
    for y in 0..h {
        let src = &pixels[y * stride..];
        let dst = &mut out[((oy + y) * tw + ox) * 4..];
        for x in 0..w {
            let p = &src[x * channels..x * channels + channels];
            let a = if channels == 4 { p[3] as u32 } else { 255 };
            let mul = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            dst[x * 4..x * 4 + 4].copy_from_slice(&[mul(p[2]), mul(p[1]), mul(p[0]), 255]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_size_is_shown_one_to_one() {
        let c = check((1440, 810), (1440, 810));
        assert_eq!(c.sharpness, Sharpness::Exact);
        assert_eq!(c.shown, (1440, 810));
        assert!(!c.bars);
        assert!(c.message((1440, 810)).contains("sharpest"));
    }

    #[test]
    fn larger_picture_is_reduced_keeping_proportions() {
        let c = check((3840, 2160), (1440, 810));
        assert_eq!(
            (c.sharpness, c.shown, c.bars),
            (Sharpness::Reduced, (1440, 810), false)
        );
        assert!(c.message((3840, 2160)).contains("1440×810"));
    }

    #[test]
    fn smaller_picture_is_enlarged_and_called_blurry() {
        let c = check((1280, 720), (1920, 1080));
        assert_eq!(c.sharpness, Sharpness::Enlarged);
        assert_eq!(c.shown, (1920, 1080));
        let m = c.message((1280, 720));
        assert!(m.contains("blurry") && m.contains("1920×1080"), "{m}");
    }

    #[test]
    fn other_proportions_get_bars() {
        // 4:3 in a 16:9 space: full height, bars left and right.
        let c = check((1440, 1080), (1920, 1080));
        assert_eq!(
            (c.sharpness, c.shown, c.bars),
            (Sharpness::Exact, (1440, 1080), true)
        );
        assert!(c.message((1440, 1080)).contains("black bars"));
        // A wide banner: full width.
        assert_eq!(fit((3000, 500), (1500, 844)), (1500, 250));
    }

    #[test]
    fn tile_size_rounds_canvas_rect() {
        assert_eq!(
            tile_size(&Rect::new(10.0, 20.0, 1439.6, 809.5)),
            (1440, 810)
        );
    }

    #[test]
    fn rounding_does_not_count_as_bars() {
        // 16:9 picture in a tile that is 16:9 only up to rounding.
        assert!(!check((1920, 1080), (1441, 810)).bars);
    }

    #[test]
    fn frame_is_centered_bgrx_on_black() {
        // A 2x1 RGBA picture (red, half-transparent green) in a 4x3 tile.
        let px = [255, 0, 0, 255, 0, 200, 0, 128, 9, 9];
        let f = compose_bgrx(&px, (2, 1), 10, 4, (4, 3));
        assert_eq!(f.len(), 4 * 3 * 4);
        let at = |x: usize, y: usize| &f[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
        assert_eq!(at(1, 1), [0, 0, 255, 255], "red, as BGRx");
        assert_eq!(at(2, 1), [0, 100, 0, 255], "transparency darkens to black");
        assert_eq!(at(0, 1), [0, 0, 0, 0], "bar");
        assert_eq!(at(1, 0), [0, 0, 0, 0], "bar");
        // RGB input.
        let f = compose_bgrx(&[1, 2, 3], (1, 1), 3, 3, (1, 1));
        assert_eq!(f, [3, 2, 1, 255]);
    }
}
