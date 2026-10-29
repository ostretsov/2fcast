//! Drawings and text placed over the shared space.
//!
//! Coordinates are normalized to the shared-space tile (0..1 on both axes) so
//! both machines render them identically regardless of resolution.  Stroke
//! width and text size are fractions of the tile height.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::layout::Rect;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

pub type Rgb = [u8; 3];

/// Colors offered in the UI.
pub const PALETTE: [Rgb; 4] = [[235, 64, 52], [250, 204, 21], [34, 197, 94], [59, 130, 246]];
pub const STROKE_WIDTH: f32 = 0.008;
pub const TEXT_SIZE: f32 = 0.05;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    Stroke {
        id: u64,
        color: Rgb,
        width: f32,
        pts: Vec<Pt>,
    },
    Text {
        id: u64,
        color: Rgb,
        size: f32,
        at: Pt,
        text: String,
    },
}

impl Item {
    pub fn id(&self) -> u64 {
        match self {
            Item::Stroke { id, .. } | Item::Text { id, .. } => *id,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AnnotOp {
    StrokeBegin {
        id: u64,
        color: Rgb,
        width: f32,
        at: Pt,
    },
    StrokeTo {
        id: u64,
        pts: Vec<Pt>,
    },
    Text {
        id: u64,
        color: Rgb,
        size: f32,
        at: Pt,
        text: String,
    },
    Remove {
        id: u64,
    },
    Clear,
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct Annotations {
    items: Vec<Item>,
}

impl Annotations {
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    fn find(&self, id: u64) -> Option<usize> {
        self.items.iter().position(|i| i.id() == id)
    }

    pub fn apply(&mut self, op: &AnnotOp) {
        match op {
            AnnotOp::StrokeBegin {
                id,
                color,
                width,
                at,
            } => {
                if self.find(*id).is_none() {
                    self.items.push(Item::Stroke {
                        id: *id,
                        color: *color,
                        width: *width,
                        pts: vec![clamp(*at)],
                    });
                }
            }
            AnnotOp::StrokeTo { id, pts } => {
                if let Some(i) = self.find(*id)
                    && let Item::Stroke { pts: existing, .. } = &mut self.items[i]
                {
                    existing.extend(pts.iter().copied().map(clamp));
                }
            }
            AnnotOp::Text {
                id,
                color,
                size,
                at,
                text,
            } => {
                if self.find(*id).is_none() && !text.trim().is_empty() {
                    self.items.push(Item::Text {
                        id: *id,
                        color: *color,
                        size: *size,
                        at: clamp(*at),
                        text: text.clone(),
                    });
                }
            }
            AnnotOp::Remove { id } => self.items.retain(|i| i.id() != *id),
            AnnotOp::Clear => self.items.clear(),
        }
    }

    /// Adds items we don't have yet (used after (re)connecting).
    pub fn merge(&mut self, items: &[Item]) {
        let have: HashMap<u64, ()> = self.items.iter().map(|i| (i.id(), ())).collect();
        self.items.extend(
            items
                .iter()
                .filter(|i| !have.contains_key(&i.id()))
                .cloned(),
        );
    }

    /// Draws all annotations into `rect` (pixels on the cairo surface).
    pub fn draw(&self, cr: &cairo::Context, rect: Rect) {
        let map = |p: &Pt| (rect.x + p.x as f64 * rect.w, rect.y + p.y as f64 * rect.h);
        cr.save().ok();
        cr.rectangle(rect.x, rect.y, rect.w, rect.h);
        cr.clip();
        for item in &self.items {
            match item {
                Item::Stroke {
                    color, width, pts, ..
                } => {
                    set_color(cr, *color);
                    cr.set_line_width((*width as f64 * rect.h).max(1.0));
                    cr.set_line_cap(cairo::LineCap::Round);
                    cr.set_line_join(cairo::LineJoin::Round);
                    let (x0, y0) = map(&pts[0]);
                    cr.move_to(x0, y0);
                    if pts.len() == 1 {
                        cr.line_to(x0 + 0.01, y0); // a dot
                    }
                    for p in &pts[1..] {
                        let (x, y) = map(p);
                        cr.line_to(x, y);
                    }
                    cr.stroke().ok();
                }
                Item::Text {
                    color,
                    size,
                    at,
                    text,
                    ..
                } => {
                    let (x, y) = map(at);
                    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
                    cr.set_font_size((*size as f64 * rect.h).max(6.0));
                    // Dark outline so text is readable on any background.
                    cr.move_to(x, y);
                    cr.text_path(text);
                    cr.set_source_rgba(0.0, 0.0, 0.0, 0.8);
                    cr.set_line_width((*size as f64 * rect.h / 12.0).max(1.0));
                    cr.stroke_preserve().ok();
                    set_color(cr, *color);
                    cr.fill().ok();
                }
            }
        }
        cr.restore().ok();
    }
}

fn clamp(p: Pt) -> Pt {
    Pt {
        x: p.x.clamp(0.0, 1.0),
        y: p.y.clamp(0.0, 1.0),
    }
}

fn set_color(cr: &cairo::Context, c: Rgb) {
    cr.set_source_rgb(
        c[0] as f64 / 255.0,
        c[1] as f64 / 255.0,
        c[2] as f64 / 255.0,
    );
}

/// Generates annotation ids that do not collide between peers:
/// the high 32 bits are random per process, the low 32 bits count up.
pub struct IdGen {
    prefix: u64,
    next: u32,
}

impl IdGen {
    pub fn new() -> Self {
        IdGen {
            prefix: (rand::random::<u32>() as u64) << 32,
            next: 0,
        }
    }

    pub fn next_id(&mut self) -> u64 {
        self.next = self.next.wrapping_add(1);
        self.prefix | self.next as u64
    }
}

impl Default for IdGen {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgb = [255, 0, 0];

    fn begin(id: u64, x: f32, y: f32) -> AnnotOp {
        AnnotOp::StrokeBegin {
            id,
            color: RED,
            width: 0.01,
            at: Pt { x, y },
        }
    }

    #[test]
    fn stroke_grows_with_points() {
        let mut a = Annotations::default();
        a.apply(&begin(1, 0.1, 0.1));
        a.apply(&AnnotOp::StrokeTo {
            id: 1,
            pts: vec![Pt { x: 0.2, y: 0.2 }, Pt { x: 0.3, y: 0.3 }],
        });
        match &a.items()[0] {
            Item::Stroke { pts, .. } => assert_eq!(pts.len(), 3),
            _ => panic!(),
        }
    }

    #[test]
    fn points_are_clamped_and_unknown_ids_ignored() {
        let mut a = Annotations::default();
        a.apply(&begin(1, -1.0, 2.0));
        a.apply(&AnnotOp::StrokeTo {
            id: 99,
            pts: vec![Pt { x: 0.5, y: 0.5 }],
        });
        assert_eq!(a.items().len(), 1);
        match &a.items()[0] {
            Item::Stroke { pts, .. } => assert_eq!(pts[0], Pt { x: 0.0, y: 1.0 }),
            _ => panic!(),
        }
    }

    #[test]
    fn duplicate_begin_is_ignored() {
        let mut a = Annotations::default();
        a.apply(&begin(1, 0.1, 0.1));
        a.apply(&begin(1, 0.9, 0.9));
        assert_eq!(a.items().len(), 1);
    }

    #[test]
    fn text_remove_and_clear() {
        let mut a = Annotations::default();
        a.apply(&AnnotOp::Text {
            id: 2,
            color: RED,
            size: 0.05,
            at: Pt { x: 0.5, y: 0.5 },
            text: "hi".into(),
        });
        a.apply(&AnnotOp::Text {
            id: 3,
            color: RED,
            size: 0.05,
            at: Pt { x: 0.5, y: 0.5 },
            text: "  ".into(),
        });
        a.apply(&begin(1, 0.1, 0.1));
        assert_eq!(a.items().len(), 2, "blank text is dropped");
        a.apply(&AnnotOp::Remove { id: 2 });
        assert_eq!(a.items().len(), 1);
        a.apply(&AnnotOp::Clear);
        assert!(a.is_empty());
    }

    #[test]
    fn merge_adds_only_missing() {
        let mut a = Annotations::default();
        a.apply(&begin(1, 0.1, 0.1));
        let mut b = Annotations::default();
        b.apply(&begin(1, 0.5, 0.5));
        b.apply(&begin(2, 0.5, 0.5));
        a.merge(b.items());
        assert_eq!(a.items().len(), 2);
        match &a.items()[0] {
            Item::Stroke { pts, .. } => assert_eq!(pts[0], Pt { x: 0.1, y: 0.1 }, "own copy kept"),
            _ => panic!(),
        }
    }

    #[test]
    fn id_gen_is_unique() {
        let mut g = IdGen::new();
        let a = g.next_id();
        let b = g.next_id();
        assert_ne!(a, b);
        assert_eq!(a >> 32, b >> 32);
    }

    fn pixel(s: &mut cairo::ImageSurface, x: usize, y: usize) -> [u8; 4] {
        let stride = s.stride() as usize;
        let data = s.data().unwrap();
        let o = y * stride + x * 4;
        [data[o], data[o + 1], data[o + 2], data[o + 3]]
    }

    #[test]
    fn draws_only_inside_rect() {
        let mut surf = cairo::ImageSurface::create(cairo::Format::Rgb24, 200, 100).unwrap();
        {
            let cr = cairo::Context::new(&surf).unwrap();
            let mut a = Annotations::default();
            // Thick horizontal line across the whole tile, at its vertical middle.
            a.apply(&AnnotOp::StrokeBegin {
                id: 1,
                color: RED,
                width: 0.1,
                at: Pt { x: 0.0, y: 0.5 },
            });
            a.apply(&AnnotOp::StrokeTo {
                id: 1,
                pts: vec![Pt { x: 1.0, y: 0.5 }],
            });
            a.draw(
                &cr,
                Rect {
                    x: 100.0,
                    y: 0.0,
                    w: 100.0,
                    h: 100.0,
                },
            );
        }
        surf.flush();
        // BGRx on little endian: red is byte 2.
        assert_eq!(
            pixel(&mut surf, 150, 50)[2],
            255,
            "line drawn inside the tile"
        );
        assert_eq!(pixel(&mut surf, 50, 50)[2], 0, "nothing outside the tile");
    }

    #[test]
    fn draws_text() {
        let mut surf = cairo::ImageSurface::create(cairo::Format::Rgb24, 200, 100).unwrap();
        {
            let cr = cairo::Context::new(&surf).unwrap();
            let mut a = Annotations::default();
            a.apply(&AnnotOp::Text {
                id: 1,
                color: [0, 255, 0],
                size: 0.4,
                at: Pt { x: 0.05, y: 0.6 },
                text: "HELLO".into(),
            });
            a.draw(
                &cr,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 200.0,
                    h: 100.0,
                },
            );
        }
        surf.flush();
        let stride = surf.stride() as usize;
        let data = surf.data().unwrap();
        let green = (0..100)
            .flat_map(|y| (0..200).map(move |x| y * stride + x * 4))
            .filter(|&o| data[o + 1] > 200)
            .count();
        assert!(green > 100, "text pixels rendered: {green}");
    }
}
