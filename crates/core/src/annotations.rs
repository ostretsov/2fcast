//! Drawings and text placed over the shared space.
//!
//! Coordinates are normalized to the shared-space tile (0..1 on both axes) so
//! both machines render them identically regardless of resolution.  Stroke
//! width and text size are fractions of the tile height.

use std::collections::HashMap;
use std::time::{Duration, Instant};

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
/// Arrow/line/rectangle width (fraction of the shared-space height): default
/// and the range the mouse wheel can set.
pub const ARROW_WIDTH: f32 = 0.016;
pub const ARROW_MIN_WIDTH: f32 = 0.002;
pub const ARROW_MAX_WIDTH: f32 = 0.04;
pub const TEXT_FONT: &str = "Monospace";
/// How long a ping's ripples last.
pub const PING_TIME: Duration = Duration::from_millis(1600);

/// What a dragged shape ([`Item::Arrow`]) looks like.  Missing on the wire
/// (older peers) means an arrow.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    #[default]
    Arrow,
    Line,
    Rect,
}

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
    /// A shape dragged from `from` to `to`: arrow, line or rectangle.
    Arrow {
        id: u64,
        #[serde(default)]
        shape: Shape,
        color: Rgb,
        width: f32,
        from: Pt,
        to: Pt,
    },
}

impl Item {
    pub fn id(&self) -> u64 {
        match self {
            Item::Stroke { id, .. } | Item::Text { id, .. } | Item::Arrow { id, .. } => *id,
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
    /// Replaces the text of a [`AnnotOp::Text`] while it is typed, so the
    /// peer sees it appear letter by letter.
    TextSet {
        id: u64,
        text: String,
    },
    /// Starts an arrow (or line, rectangle); it points from `at` to where
    /// [`AnnotOp::ArrowSet`] says.
    ArrowBegin {
        id: u64,
        #[serde(default)]
        shape: Shape,
        color: Rgb,
        width: f32,
        at: Pt,
    },
    /// Moves the arrow's tip and/or changes its width while it is drawn.
    ArrowSet {
        id: u64,
        to: Pt,
        width: f32,
    },
    /// "Look here": ripples around `at` for [`PING_TIME`] (not a drawing:
    /// it is not kept, synced later or undone).
    Ping {
        color: Rgb,
        at: Pt,
    },
    Remove {
        id: u64,
    },
    Clear,
}

#[derive(Clone, Debug, PartialEq)]
struct Ping {
    color: Rgb,
    at: Pt,
    started: Instant,
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct Annotations {
    items: Vec<Item>,
    pings: Vec<Ping>,
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
            AnnotOp::TextSet { id, text: new } => {
                if let Some(i) = self.find(*id)
                    && let Item::Text { text, .. } = &mut self.items[i]
                {
                    new.clone_into(text);
                }
            }
            AnnotOp::ArrowBegin {
                id,
                shape,
                color,
                width,
                at,
            } => {
                if self.find(*id).is_none() {
                    let at = clamp(*at);
                    self.items.push(Item::Arrow {
                        id: *id,
                        shape: *shape,
                        color: *color,
                        width: *width,
                        from: at,
                        to: at,
                    });
                }
            }
            AnnotOp::ArrowSet { id, to, width } => {
                if let Some(i) = self.find(*id)
                    && let Item::Arrow {
                        to: tip, width: w, ..
                    } = &mut self.items[i]
                {
                    *tip = clamp(*to);
                    *w = width.clamp(ARROW_MIN_WIDTH, ARROW_MAX_WIDTH);
                }
            }
            AnnotOp::Remove { id } => self.items.retain(|i| i.id() != *id),
            AnnotOp::Ping { color, at } => {
                self.pings.retain(|p| p.started.elapsed() < PING_TIME);
                self.pings.push(Ping {
                    color: *color,
                    at: clamp(*at),
                    started: Instant::now(),
                });
            }
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
        self.draw_at(cr, rect, Instant::now());
    }

    /// Draws as of `now` (pings are animated).
    fn draw_at(&self, cr: &cairo::Context, rect: Rect, now: Instant) {
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
                Item::Arrow {
                    shape,
                    color,
                    width,
                    from,
                    to,
                    ..
                } => {
                    let width = *width as f64 * rect.h;
                    match shape {
                        Shape::Arrow => draw_arrow(cr, map(from), map(to), width, *color),
                        Shape::Line | Shape::Rect => {
                            let ((x0, y0), (x1, y1)) = (map(from), map(to));
                            set_color(cr, *color);
                            cr.set_line_width(width.max(1.0));
                            cr.set_line_cap(cairo::LineCap::Round);
                            cr.set_line_join(cairo::LineJoin::Round);
                            if *shape == Shape::Line {
                                cr.move_to(x0, y0);
                                cr.line_to(x1, y1);
                            } else {
                                cr.rectangle(x0, y0, x1 - x0, y1 - y0);
                            }
                            cr.stroke().ok();
                        }
                    }
                }
                Item::Text {
                    color,
                    size,
                    at,
                    text,
                    ..
                } => {
                    let (x, y) = map(at);
                    // Monospace: good for code; fontconfig maps it to the system code font.
                    cr.select_font_face(
                        TEXT_FONT,
                        cairo::FontSlant::Normal,
                        cairo::FontWeight::Bold,
                    );
                    let px = (*size as f64 * rect.h).max(6.0);
                    cr.set_font_size(px);
                    // One line below the other; tabs as 4 spaces (code).
                    cr.new_path();
                    for (i, line) in text.lines().enumerate() {
                        cr.move_to(x, y + i as f64 * px * 1.25);
                        cr.text_path(&line.replace('\t', "    "));
                    }
                    // Dark outline so text is readable on any background.
                    cr.set_source_rgba(0.0, 0.0, 0.0, 0.8);
                    cr.set_line_width((*size as f64 * rect.h / 12.0).max(1.0));
                    cr.stroke_preserve().ok();
                    set_color(cr, *color);
                    cr.fill().ok();
                }
            }
        }
        for p in &self.pings {
            draw_ping(
                cr,
                map(&p.at),
                rect.h,
                now.saturating_duration_since(p.started),
                p.color,
            );
        }
        cr.restore().ok();
    }
}

/// A sticker-like arrow: the shaft widens from a sharp tail to a big head,
/// filled with `color`, with a white outline and a soft shadow so it stands
/// out on any background.  The shaft is 3x `width` wide at the head (the
/// line width of lines and rectangles), so arrows read as bold pointers.
fn draw_arrow(
    cr: &cairo::Context,
    (x0, y0): (f64, f64),
    (x1, y1): (f64, f64),
    width: f64,
    color: Rgb,
) {
    let width = width.max(2.0);
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return; // Not dragged yet.
    }
    let (ux, uy) = (dx / len, dy / len);
    // Short arrows shrink as a whole so the head stays under 40% of the length.
    let s = (len * 0.4 / (width * 4.8)).min(1.0);
    let head = width * 4.8 * s;
    let (shaft, wing, tail) = (width * 1.5 * s, width * 3.4 * s, width * 0.25 * s);
    let (bx, by) = (x1 - ux * head, y1 - uy * head);
    // Perpendicular offset from a point on the axis.
    let side = |x: f64, y: f64, d: f64| (x - uy * d, y + ux * d);
    let pts = [
        side(x0, y0, tail),
        side(bx, by, shaft),
        side(bx, by, wing),
        (x1, y1),
        side(bx, by, -wing),
        side(bx, by, -shaft),
        side(x0, y0, -tail),
    ];
    let outline = (width * 0.5 * s).max(1.5);
    let path = |ox: f64, oy: f64| {
        cr.new_path();
        for (i, (x, y)) in pts.iter().enumerate() {
            if i == 0 {
                cr.move_to(x + ox, y + oy);
            } else {
                cr.line_to(x + ox, y + oy);
            }
        }
        cr.close_path();
    };
    cr.set_line_join(cairo::LineJoin::Round);
    cr.set_line_width(outline * 2.0);
    let off = outline * 0.8;
    path(off, off);
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.25);
    cr.stroke_preserve().ok();
    cr.fill().ok();
    path(0.0, 0.0);
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.stroke_preserve().ok();
    set_color(cr, color);
    cr.fill().ok();
}

/// Three rings spreading from the point one after another and fading, around
/// a dot; `h` is the shared-space height in pixels.
fn draw_ping(cr: &cairo::Context, (x, y): (f64, f64), h: f64, age: Duration, color: Rgb) {
    const RING_SECS: f64 = 1.0;
    const RING_GAP_SECS: f64 = 0.3;
    let t = age.as_secs_f64();
    if t >= PING_TIME.as_secs_f64() {
        return;
    }
    let radius = 0.15 * h;
    let [r, g, b] = color.map(|c| c as f64 / 255.0);
    for i in 0..3 {
        let p = (t - i as f64 * RING_GAP_SECS) / RING_SECS;
        if !(0.0..1.0).contains(&p) {
            continue;
        }
        let ease = 1.0 - (1.0 - p) * (1.0 - p);
        let alpha = 1.0 - p * p;
        let width = (0.014 * h * (1.0 - 0.5 * p)).max(2.0);
        cr.new_path();
        cr.arc(
            x,
            y,
            radius * (0.15 + 0.85 * ease),
            0.0,
            std::f64::consts::TAU,
        );
        // White halo so the ring shows on any background.
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.7 * alpha);
        cr.set_line_width(width + 3.0);
        cr.stroke_preserve().ok();
        cr.set_source_rgba(r, g, b, alpha);
        cr.set_line_width(width);
        cr.stroke().ok();
    }
    // The dot fades during the last 0.4 s.
    let alpha = ((PING_TIME.as_secs_f64() - t) / 0.4).min(1.0);
    cr.new_path();
    cr.arc(x, y, (0.02 * h).max(4.0), 0.0, std::f64::consts::TAU);
    cr.set_source_rgba(1.0, 1.0, 1.0, alpha);
    cr.fill().ok();
    cr.new_path();
    cr.arc(x, y, (0.015 * h).max(3.0), 0.0, std::f64::consts::TAU);
    cr.set_source_rgba(r, g, b, alpha);
    cr.fill().ok();
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
    fn arrows_follow_the_tip_and_width() {
        let mut a = Annotations::default();
        a.apply(&AnnotOp::ArrowBegin {
            id: 4,
            shape: Shape::Arrow,
            color: RED,
            width: 0.01,
            at: Pt { x: 0.1, y: 0.1 },
        });
        a.apply(&AnnotOp::ArrowSet {
            id: 4,
            to: Pt { x: 0.8, y: 1.5 },
            width: 0.02,
        });
        a.apply(&AnnotOp::ArrowSet {
            id: 4,
            to: Pt { x: 0.8, y: 0.6 },
            width: 9.0,
        });
        match &a.items()[0] {
            Item::Arrow {
                from, to, width, ..
            } => {
                assert_eq!(*from, Pt { x: 0.1, y: 0.1 });
                assert_eq!(*to, Pt { x: 0.8, y: 0.6 });
                assert_eq!(*width, ARROW_MAX_WIDTH, "width is limited");
            }
            _ => panic!(),
        }
        a.apply(&AnnotOp::ArrowSet {
            id: 99,
            to: Pt { x: 0.5, y: 0.5 },
            width: 0.01,
        });
        assert_eq!(a.items().len(), 1, "unknown arrow ignored");
    }

    /// Draws one shape from (0.05, 0.5) to `to` on a 400x250 surface.
    fn draw_shape(shape: Shape, width: f32, to: Pt) -> cairo::ImageSurface {
        let surf = cairo::ImageSurface::create(cairo::Format::Rgb24, 400, 250).unwrap();
        {
            let cr = cairo::Context::new(&surf).unwrap();
            let mut a = Annotations::default();
            a.apply(&AnnotOp::ArrowBegin {
                id: 1,
                shape,
                color: RED,
                width,
                at: Pt { x: 0.05, y: 0.5 },
            });
            a.apply(&AnnotOp::ArrowSet { id: 1, to, width });
            a.draw(
                &cr,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 400.0,
                    h: 250.0,
                },
            );
        }
        surf.flush();
        surf
    }

    // BGRx on little endian.
    fn is_red(s: &mut cairo::ImageSurface, x: usize, y: usize) -> bool {
        let p = pixel(s, x, y);
        p[2] > 200 && p[1] < 80 && p[0] < 80
    }

    #[test]
    fn draws_arrow_with_head_at_the_tip() {
        // Width 10 px: tail at x=20, tip at x=380 on y=125, head 48 px long
        // and 68 px wide, shaft 30 px wide where it meets the head.
        let mut surf = draw_shape(Shape::Arrow, 0.04, Pt { x: 0.95, y: 0.5 });
        assert!(is_red(&mut surf, 200, 125), "shaft");
        assert!(is_red(&mut surf, 345, 105), "head is wider than the shaft");
        assert!(!is_red(&mut surf, 320, 105), "shaft is narrower");
        assert!(!is_red(&mut surf, 30, 120), "tail is narrow");
        assert_eq!(
            pixel(&mut surf, 345, 98)[..3],
            [255, 255, 255],
            "white outline"
        );
    }

    #[test]
    fn draws_lines_and_rectangles() {
        let mut line = draw_shape(Shape::Line, 0.04, Pt { x: 0.95, y: 0.5 });
        assert!(is_red(&mut line, 200, 125));
        assert!(!is_red(&mut line, 200, 110));
        let mut rect = draw_shape(Shape::Rect, 0.04, Pt { x: 0.95, y: 0.9 });
        assert!(is_red(&mut rect, 200, 125), "top edge");
        assert!(is_red(&mut rect, 200, 225), "bottom edge");
        assert!(is_red(&mut rect, 20, 175), "left edge");
        assert!(!is_red(&mut rect, 200, 175), "hollow");
    }

    #[test]
    fn shape_defaults_to_arrow_on_the_wire() {
        let op: AnnotOp = serde_json::from_str(
            r#"{"op":"arrow_begin","id":1,"color":[1,2,3],"width":0.01,"at":{"x":0,"y":0}}"#,
        )
        .unwrap();
        let AnnotOp::ArrowBegin { shape, .. } = op else {
            panic!()
        };
        assert_eq!(shape, Shape::Arrow);
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
    fn text_is_updated_while_typed_and_drawn_line_by_line() {
        let mut a = Annotations::default();
        a.apply(&AnnotOp::Text {
            id: 1,
            color: [0, 255, 0],
            size: 0.2,
            at: Pt { x: 0.05, y: 0.25 },
            text: "f".into(),
        });
        a.apply(&AnnotOp::TextSet {
            id: 1,
            text: "fn\n\tx".into(),
        });
        a.apply(&AnnotOp::TextSet {
            id: 9,
            text: "nope".into(),
        });
        assert!(matches!(&a.items()[0], Item::Text { text, .. } if text == "fn\n\tx"));
        assert_eq!(a.items().len(), 1);

        let mut surf = cairo::ImageSurface::create(cairo::Format::Rgb24, 200, 100).unwrap();
        {
            let cr = cairo::Context::new(&surf).unwrap();
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
        // Count green pixels per area: line 1 above y=30, line 2 (indented) below.
        let green = |ys: std::ops::Range<usize>, xs: std::ops::Range<usize>| {
            ys.flat_map(|y| xs.clone().map(move |x| y * stride + x * 4))
                .filter(|&o| data[o + 1] > 200 && data[o + 2] < 100)
                .count()
        };
        assert!(green(0..30, 0..200) > 20, "first line");
        assert!(green(32..60, 0..200) > 20, "second line");
        assert_eq!(green(32..60, 0..40), 0, "tab indents the second line");
    }

    #[test]
    fn pings_ripple_then_vanish_without_becoming_drawings() {
        let mut a = Annotations::default();
        a.apply(&AnnotOp::Ping {
            color: RED,
            at: Pt { x: 0.5, y: 0.5 },
        });
        assert!(a.is_empty(), "not a drawing (no undo, no sync)");
        let at = |secs: f64| {
            let surf = cairo::ImageSurface::create(cairo::Format::Rgb24, 400, 250).unwrap();
            {
                let cr = cairo::Context::new(&surf).unwrap();
                a.draw_at(
                    &cr,
                    Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 400.0,
                        h: 250.0,
                    },
                    a.pings[0].started + Duration::from_secs_f64(secs),
                );
            }
            surf.flush();
            surf
        };
        // At 0.5 s the first ring is 79% out: radius 0.79 * 37.5 px ≈ 29.5 px.
        let mut s = at(0.5);
        let reddish = |p: [u8; 4]| p[2] > 100 && p[1] < 60 && p[0] < 60;
        assert!(reddish(pixel(&mut s, 200, 125)), "dot");
        assert!(reddish(pixel(&mut s, 229, 125)), "first ring");
        // The second ring started 0.3 s later: 46% out (17 px).
        assert!(reddish(pixel(&mut s, 217, 125)), "second ring");
        assert!(!reddish(pixel(&mut s, 209, 125)), "rings are hollow");
        let mut s = at(2.0);
        assert_eq!(
            pixel(&mut s, 200, 125)[..3],
            [0, 0, 0],
            "gone after PING_TIME"
        );
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
