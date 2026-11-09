//! The mouse pointer drawn into a shared window's picture.
//!
//! `ximagesrc` (1.20) draws it badly when capturing one window: it reads the
//! window position once at start (after a drag the pointer lands in the
//! wrong place) and draws into its reused buffers, so a frame whose capture
//! failed (window partly off-screen) still carries the pointers of earlier
//! frames: trails of pointers.  So the app captures without the pointer and
//! draws it here, onto a copy of the frame.

/// The pointer image (premultiplied ARGB, as XFixes gives it), placed with
/// its top-left corner at (`x`, `y`) in frame coordinates.
#[derive(Clone, Copy, Debug)]
pub struct Cursor<'a> {
    pub x: i32,
    pub y: i32,
    pub width: usize,
    pub height: usize,
    pub pixels: &'a [u32],
}

impl Cursor<'_> {
    /// The part of the pointer inside a `w`×`h` frame, as pointer-relative
    /// column and row ranges; `None` if it is outside.
    fn visible(
        &self,
        w: usize,
        h: usize,
    ) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
        let span = |pos: i32, len: usize, limit: usize| {
            let start = (-pos).max(0) as usize;
            let end = (limit as i64 - pos as i64).clamp(0, len as i64) as usize;
            (start < end).then_some(start..end)
        };
        if self.pixels.len() < self.width * self.height {
            return None;
        }
        Some((span(self.x, self.width, w)?, span(self.y, self.height, h)?))
    }

    pub fn overlaps(&self, w: usize, h: usize) -> bool {
        self.visible(w, h).is_some()
    }

    /// Blends the pointer onto a BGRx frame of `w`×`h` with `stride` bytes per row.
    pub fn draw(&self, frame: &mut [u8], w: usize, h: usize, stride: usize) {
        let Some((cols, rows)) = self.visible(w, h) else {
            return;
        };
        for cy in rows {
            let fy = (self.y + cy as i32) as usize;
            for cx in cols.clone() {
                let fx = (self.x + cx as i32) as usize;
                let argb = self.pixels[cy * self.width + cx];
                let a = argb >> 24;
                if a == 0 {
                    continue;
                }
                let i = fy * stride + fx * 4;
                let Some(px) = frame.get_mut(i..i + 3) else {
                    continue;
                };
                // B, G, R: source is premultiplied.
                for (c, d) in px.iter_mut().enumerate() {
                    let s = (argb >> (8 * c)) & 0xff;
                    let v = s + (*d as u32 * (255 - a) + 127) / 255;
                    *d = v.min(255) as u8;
                }
            }
        }
    }
}

/// Whether the rectangle lies entirely on the screen. `XShmGetImage` (which
/// `ximagesrc` uses) fails for a window that does not.
pub fn on_screen(x: i32, y: i32, w: u32, h: u32, screen_w: u32, screen_h: u32) -> bool {
    x >= 0
        && y >= 0
        && x as i64 + w as i64 <= screen_w as i64
        && y as i64 + h as i64 <= screen_h as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize) -> Vec<u8> {
        vec![10; w * h * 4]
    }

    #[test]
    fn opaque_pixel_replaces_and_transparent_keeps() {
        let (w, h) = (4, 3);
        let mut f = frame(w, h);
        let pixels = [0xff_20_40_60, 0x00_00_00_00];
        Cursor {
            x: 1,
            y: 2,
            width: 2,
            height: 1,
            pixels: &pixels,
        }
        .draw(&mut f, w, h, w * 4);
        let at = |x: usize, y: usize| &f[(y * w + x) * 4..(y * w + x) * 4 + 4];
        assert_eq!(at(1, 2), [0x60, 0x40, 0x20, 10]);
        assert_eq!(at(2, 2), [10, 10, 10, 10]);
        assert_eq!(at(0, 0), [10, 10, 10, 10]);
    }

    #[test]
    fn half_transparent_blends_premultiplied() {
        let mut f = frame(1, 1);
        let pixels = [0x80_40_40_40];
        Cursor {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            pixels: &pixels,
        }
        .draw(&mut f, 1, 1, 4);
        // 0x40 + 10 * 127 / 255 = 64 + 5
        assert_eq!(&f[..3], [69, 69, 69]);
    }

    #[test]
    fn clipped_at_every_edge() {
        let (w, h) = (3, 3);
        let pixels = [0xff_ff_ff_ff; 4];
        for (x, y, expect) in [
            (-1, -1, (0, 0)),
            (2, 2, (2, 2)),
            (-1, 2, (0, 2)),
            (2, -1, (2, 0)),
        ] {
            let mut f = frame(w, h);
            let c = Cursor {
                x,
                y,
                width: 2,
                height: 2,
                pixels: &pixels,
            };
            assert!(c.overlaps(w, h));
            c.draw(&mut f, w, h, w * 4);
            let white: Vec<_> = (0..w * h).filter(|i| f[i * 4] == 255).collect();
            assert_eq!(white, [expect.1 * w + expect.0], "cursor at {x},{y}");
        }
    }

    #[test]
    fn outside_does_not_overlap() {
        let pixels = [0xff_ff_ff_ff; 4];
        for (x, y) in [(-2, 0), (0, -2), (3, 0), (0, 3), (-100, -100)] {
            let c = Cursor {
                x,
                y,
                width: 2,
                height: 2,
                pixels: &pixels,
            };
            assert!(!c.overlaps(3, 3), "{x},{y}");
            let mut f = frame(3, 3);
            c.draw(&mut f, 3, 3, 12);
            assert!(f.iter().all(|&b| b == 10));
        }
    }

    #[test]
    fn short_pixel_data_is_ignored() {
        let c = Cursor {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
            pixels: &[0xffffffff],
        };
        assert!(!c.overlaps(3, 3));
    }

    #[test]
    fn on_screen_checks_all_edges() {
        assert!(on_screen(0, 0, 1920, 1080, 1920, 1080));
        assert!(on_screen(100, 50, 800, 600, 1920, 1080));
        assert!(!on_screen(-1, 0, 800, 600, 1920, 1080));
        assert!(!on_screen(0, -5, 800, 600, 1920, 1080));
        assert!(!on_screen(1200, 0, 800, 600, 1920, 1080));
        assert!(!on_screen(0, 500, 800, 600, 1920, 1080));
    }
}
