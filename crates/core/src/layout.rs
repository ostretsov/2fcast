//! Layout of the recorded 1920x1080 canvas: where the shared space and the
//! two cameras go.  All tiles keep a 16:9 aspect ratio so screen content and
//! annotations are never distorted.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::PeerId;
use crate::annotations::Pt;

pub const CANVAS_W: f64 = 1920.0;
pub const CANVAS_H: f64 = 1080.0;
const MIN_TILE_W: f64 = 192.0;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Rect { x, y, w, h }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }

    /// Position of (x, y) relative to this rect, 0..1 on both axes.
    pub fn normalize(&self, x: f64, y: f64) -> Pt {
        Pt {
            x: ((x - self.x) / self.w) as f32,
            y: ((y - self.y) / self.h) as f32,
        }
    }

    fn clamped_to_canvas(mut self) -> Rect {
        self.w = self.w.clamp(MIN_TILE_W, CANVAS_W);
        self.h = self.w * 9.0 / 16.0;
        self.x = self.x.clamp(0.0, CANVAS_W - self.w);
        self.y = self.y.clamp(0.0, CANVAS_H - self.h);
        self
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    pub rect: Rect,
    pub visible: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TileId {
    Screen,
    Cam(PeerId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// Shared space large on the left, cameras stacked on the right.
    ScreenCamsRight,
    /// Shared space fills the frame, small cameras in the bottom-right corner.
    ScreenCamsOverlay,
    /// Only the two faces, side by side.
    CamsOnly,
    /// Only the shared space.
    ScreenOnly,
}

impl Preset {
    pub const ALL: [Preset; 4] = [
        Preset::ScreenCamsRight,
        Preset::ScreenCamsOverlay,
        Preset::CamsOnly,
        Preset::ScreenOnly,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Preset::ScreenCamsRight => "Screen + cameras on the right",
            Preset::ScreenCamsOverlay => "Screen + small cameras in corner",
            Preset::CamsOnly => "Cameras only",
            Preset::ScreenOnly => "Screen only",
        }
    }

    fn screen(self) -> Tile {
        match self {
            Preset::ScreenCamsRight => Tile {
                rect: Rect::new(0.0, 135.0, 1440.0, 810.0),
                visible: true,
            },
            Preset::ScreenCamsOverlay | Preset::ScreenOnly => Tile {
                rect: Rect::new(0.0, 0.0, 1920.0, 1080.0),
                visible: true,
            },
            Preset::CamsOnly => Tile {
                rect: Rect::new(0.0, 0.0, 1920.0, 1080.0),
                visible: false,
            },
        }
    }

    /// Tile for the camera at position `slot` (0 or 1) in id order.
    fn cam(self, slot: usize) -> Tile {
        let s = slot.min(1) as f64;
        match self {
            Preset::ScreenCamsRight => Tile {
                rect: Rect::new(1440.0, 270.0 + s * 270.0, 480.0, 270.0),
                visible: true,
            },
            Preset::ScreenCamsOverlay => Tile {
                rect: Rect::new(1112.0 + s * 400.0, 840.0, 384.0, 216.0),
                visible: true,
            },
            Preset::CamsOnly => Tile {
                rect: Rect::new(s * 960.0, 270.0, 960.0, 540.0),
                visible: true,
            },
            Preset::ScreenOnly => Tile {
                rect: Rect::new(1112.0 + s * 400.0, 840.0, 384.0, 216.0),
                visible: false,
            },
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Layout {
    pub screen: Tile,
    pub cams: BTreeMap<PeerId, Tile>,
}

impl Default for Layout {
    fn default() -> Self {
        Layout::preset(Preset::ScreenCamsRight, &[])
    }
}

fn sorted(ids: &[PeerId]) -> Vec<PeerId> {
    let mut v = ids.to_vec();
    v.sort();
    v.dedup();
    v
}

impl Layout {
    /// Builds a preset.  Ids are sorted, so both machines get the same result.
    pub fn preset(p: Preset, ids: &[PeerId]) -> Layout {
        let cams = sorted(ids)
            .into_iter()
            .enumerate()
            .map(|(i, id)| (id, p.cam(i)))
            .collect();
        Layout {
            screen: p.screen(),
            cams,
        }
    }

    /// Adds default tiles for cameras that have none, each in the first slot
    /// not already taken by another camera.  Returns true if changed.
    pub fn ensure_cams(&mut self, ids: &[PeerId]) -> bool {
        let mut changed = false;
        for id in sorted(ids) {
            if self.cams.contains_key(&id) {
                continue;
            }
            let taken = |t: &Tile| self.cams.values().any(|c| c.rect == t.rect);
            let tile = (0..2)
                .map(|slot| Preset::ScreenCamsRight.cam(slot))
                .find(|t| !taken(t))
                .unwrap_or(Preset::ScreenCamsOverlay.cam(0));
            self.cams.insert(id, tile);
            changed = true;
        }
        changed
    }

    pub fn tile(&self, id: &TileId) -> Option<&Tile> {
        match id {
            TileId::Screen => Some(&self.screen),
            TileId::Cam(p) => self.cams.get(p),
        }
    }

    fn tile_mut(&mut self, id: &TileId) -> Option<&mut Tile> {
        match id {
            TileId::Screen => Some(&mut self.screen),
            TileId::Cam(p) => self.cams.get_mut(p),
        }
    }

    /// Topmost visible tile under canvas point (x, y).  Cameras are drawn above
    /// the shared space; only cameras of `active` peers are considered.
    pub fn hit(&self, x: f64, y: f64, active: &[PeerId]) -> Option<TileId> {
        let mut cams: Vec<&PeerId> = active
            .iter()
            .filter(|id| self.cams.contains_key(*id))
            .collect();
        cams.sort();
        for id in cams.into_iter().rev() {
            let t = &self.cams[id];
            if t.visible && t.rect.contains(x, y) {
                return Some(TileId::Cam(id.clone()));
            }
        }
        (self.screen.visible && self.screen.rect.contains(x, y)).then_some(TileId::Screen)
    }

    pub fn move_by(&mut self, id: &TileId, dx: f64, dy: f64) {
        if let Some(t) = self.tile_mut(id) {
            t.rect = Rect {
                x: t.rect.x + dx,
                y: t.rect.y + dy,
                ..t.rect
            }
            .clamped_to_canvas();
        }
    }

    /// Places a tile at an absolute position (top-left corner), clamped.
    pub fn move_to(&mut self, id: &TileId, x: f64, y: f64) {
        if let Some(t) = self.tile_mut(id) {
            t.rect = Rect { x, y, ..t.rect }.clamped_to_canvas();
        }
    }

    /// Scales a tile around its center, keeping 16:9 and staying on canvas.
    pub fn scale(&mut self, id: &TileId, factor: f64) {
        if let Some(t) = self.tile_mut(id) {
            let (cx, cy) = (t.rect.x + t.rect.w / 2.0, t.rect.y + t.rect.h / 2.0);
            let w = (t.rect.w * factor).clamp(MIN_TILE_W, CANVAS_W);
            let h = w * 9.0 / 16.0;
            t.rect = Rect::new(cx - w / 2.0, cy - h / 2.0, w, h).clamped_to_canvas();
        }
    }

    pub fn set_visible(&mut self, id: &TileId, visible: bool) {
        if let Some(t) = self.tile_mut(id) {
            t.visible = visible;
        }
    }
}

/// New size for a shared window so it appears 1:1 in a tile of
/// `max_w`x`max_h`: each side shrunk to fit, never enlarged.  `None` if it
/// already fits.
pub fn shrink_to_fit(w: u32, h: u32, max_w: u32, max_h: u32) -> Option<(u32, u32)> {
    let new = (w.min(max_w), h.min(max_h));
    (new != (w, h)).then_some(new)
}

/// How the canvas is fitted ("contain", centered) into a widget.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    pub scale: f64,
    pub ox: f64,
    pub oy: f64,
}

impl Fit {
    pub fn contain(widget_w: f64, widget_h: f64) -> Fit {
        let scale = (widget_w / CANVAS_W).min(widget_h / CANVAS_H).max(1e-6);
        Fit {
            scale,
            ox: (widget_w - CANVAS_W * scale) / 2.0,
            oy: (widget_h - CANVAS_H * scale) / 2.0,
        }
    }

    pub fn to_canvas(&self, x: f64, y: f64) -> (f64, f64) {
        ((x - self.ox) / self.scale, (y - self.oy) / self.scale)
    }

    pub fn to_widget(&self, r: Rect) -> Rect {
        Rect::new(
            self.ox + r.x * self.scale,
            self.oy + r.y * self.scale,
            r.w * self.scale,
            r.h * self.scale,
        )
    }
}

/// Properties for one compositor input pad.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PadParams {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub alpha: f64,
    pub zorder: u32,
}

/// Parameters for the four compositor inputs of the program pipeline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pads {
    pub screen_local: PadParams,
    pub screen_remote: PadParams,
    pub cam_local: PadParams,
    pub cam_remote: PadParams,
}

fn pad(t: Option<&Tile>, shown: bool, zorder: u32) -> PadParams {
    let t = t.copied().unwrap_or(Tile {
        rect: Rect::new(0.0, 0.0, MIN_TILE_W, MIN_TILE_W * 9.0 / 16.0),
        visible: false,
    });
    let r = t.rect;
    PadParams {
        x: r.x.round() as i32,
        y: r.y.round() as i32,
        w: r.w.round() as i32,
        h: r.h.round() as i32,
        alpha: if shown && t.visible { 1.0 } else { 0.0 },
        zorder,
    }
}

impl Layout {
    pub fn pads(&self, me: &PeerId, peer: Option<&PeerId>, presenter: Option<&PeerId>) -> Pads {
        let me_first = peer.is_none_or(|p| me < p);
        let (z_me, z_peer) = if me_first { (1, 2) } else { (2, 1) };
        Pads {
            screen_local: pad(Some(&self.screen), presenter == Some(me), 0),
            screen_remote: pad(
                Some(&self.screen),
                presenter.is_some() && presenter == peer,
                0,
            ),
            cam_local: pad(self.cams.get(me), true, z_me),
            cam_remote: pad(peer.and_then(|p| self.cams.get(p)), peer.is_some(), z_peer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> Vec<PeerId> {
        vec!["b".into(), "a".into()]
    }

    fn is_16_9(r: &Rect) -> bool {
        (r.w * 9.0 / 16.0 - r.h).abs() < 1e-6
    }

    fn on_canvas(r: &Rect) -> bool {
        r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= CANVAS_W && r.y + r.h <= CANVAS_H
    }

    #[test]
    fn presets_are_valid_and_deterministic() {
        for p in Preset::ALL {
            let l = Layout::preset(p, &ids());
            assert_eq!(
                l,
                Layout::preset(p, &["a".into(), "b".into()]),
                "independent of id order"
            );
            assert!(
                is_16_9(&l.screen.rect) && on_canvas(&l.screen.rect),
                "{p:?}"
            );
            for t in l.cams.values() {
                assert!(is_16_9(&t.rect) && on_canvas(&t.rect), "{p:?}");
            }
            assert!(!p.label().is_empty());
        }
    }

    #[test]
    fn visible_cams_do_not_overlap_each_other() {
        for p in Preset::ALL {
            let l = Layout::preset(p, &ids());
            let (a, b) = (l.cams["a"].rect, l.cams["b"].rect);
            assert!(a.x + a.w <= b.x || a.y + a.h <= b.y, "{p:?}");
        }
    }

    #[test]
    fn cams_only_hides_screen() {
        let l = Layout::preset(Preset::CamsOnly, &ids());
        assert!(!l.screen.visible);
        assert!(l.cams.values().all(|t| t.visible));
    }

    #[test]
    fn ensure_cams_adds_missing_only() {
        let mut l = Layout::preset(Preset::CamsOnly, &["a".into()]);
        let a_before = l.cams["a"];
        assert!(l.ensure_cams(&ids()));
        assert_eq!(l.cams["a"], a_before);
        assert!(l.cams.contains_key("b"));
        assert!(!l.ensure_cams(&ids()));
    }

    #[test]
    fn ensure_cams_uses_a_free_slot() {
        // "b" sorts after "a" but "a" is added later: it must not land on "b".
        let mut l = Layout::preset(Preset::ScreenCamsRight, &["b".into()]);
        l.ensure_cams(&ids());
        assert_ne!(l.cams["a"].rect, l.cams["b"].rect);
    }

    #[test]
    fn hit_prefers_cams_over_screen() {
        let l = Layout::preset(Preset::ScreenCamsOverlay, &ids());
        let cam = l.cams["b"].rect;
        assert_eq!(
            l.hit(cam.x + 5.0, cam.y + 5.0, &ids()),
            Some(TileId::Cam("b".into()))
        );
        assert_eq!(l.hit(100.0, 100.0, &ids()), Some(TileId::Screen));
        // An inactive peer's camera is not hit.
        assert_eq!(
            l.hit(cam.x + 5.0, cam.y + 5.0, &["a".into()]),
            Some(TileId::Screen)
        );
        let l = Layout::preset(Preset::CamsOnly, &ids());
        assert_eq!(l.hit(10.0, 10.0, &ids()), None, "hidden screen is not hit");
    }

    #[test]
    fn move_is_clamped() {
        let mut l = Layout::preset(Preset::ScreenCamsRight, &ids());
        let id = TileId::Cam("a".into());
        l.move_by(&id, 10_000.0, -10_000.0);
        let r = l.cams["a"].rect;
        assert_eq!((r.x, r.y), (CANVAS_W - r.w, 0.0));
        l.move_to(&id, 100.0, 200.0);
        assert_eq!((l.cams["a"].rect.x, l.cams["a"].rect.y), (100.0, 200.0));
    }

    #[test]
    fn scale_keeps_aspect_and_limits() {
        let mut l = Layout::preset(Preset::ScreenCamsRight, &ids());
        let id = TileId::Cam("a".into());
        l.scale(&id, 0.01);
        assert_eq!(l.cams["a"].rect.w, MIN_TILE_W);
        l.scale(&id, 100.0);
        let r = l.cams["a"].rect;
        assert_eq!((r.w, r.h), (CANVAS_W, CANVAS_H));
        assert!(is_16_9(&r) && on_canvas(&r));
        l.scale(&id, 0.5);
        assert!(is_16_9(&l.cams["a"].rect) && on_canvas(&l.cams["a"].rect));
    }

    #[test]
    fn shared_windows_only_shrink() {
        assert_eq!(shrink_to_fit(2560, 1400, 1440, 810), Some((1440, 810)));
        assert_eq!(
            shrink_to_fit(1200, 1000, 1440, 810),
            Some((1200, 810)),
            "only the side that is too big"
        );
        assert_eq!(
            shrink_to_fit(800, 600, 1440, 810),
            None,
            "small windows are not enlarged"
        );
        assert_eq!(shrink_to_fit(1440, 810, 1440, 810), None);
    }

    #[test]
    fn fit_maps_both_ways() {
        // Wider widget: letterbox on the sides.
        let f = Fit::contain(1000.0, 270.0);
        assert!((f.scale - 0.25).abs() < 1e-9);
        assert_eq!(f.oy, 0.0);
        assert_eq!(f.ox, (1000.0 - 480.0) / 2.0);
        let (x, y) = f.to_canvas(f.ox + 240.0, 135.0);
        assert!((x - 960.0).abs() < 1e-9 && (y - 540.0).abs() < 1e-9);
        let r = f.to_widget(Rect::new(960.0, 540.0, 192.0, 108.0));
        assert_eq!(r, Rect::new(f.ox + 240.0, 135.0, 48.0, 27.0));
    }

    #[test]
    fn rect_normalize() {
        let r = Rect::new(100.0, 100.0, 200.0, 100.0);
        assert_eq!(r.normalize(200.0, 150.0), Pt { x: 0.5, y: 0.5 });
        assert!(r.contains(100.0, 100.0) && !r.contains(300.0, 150.0));
    }

    #[test]
    fn pads_follow_presenter_and_peer() {
        let l = Layout::preset(Preset::ScreenCamsRight, &ids());
        let a: PeerId = "a".into();
        let b: PeerId = "b".into();

        let p = l.pads(&a, None, None);
        assert_eq!(p.screen_local.alpha, 0.0);
        assert_eq!(p.screen_remote.alpha, 0.0);
        assert_eq!(p.cam_local.alpha, 1.0);
        assert_eq!(p.cam_remote.alpha, 0.0, "no peer, no remote camera");

        let p = l.pads(&a, Some(&b), Some(&a));
        assert_eq!((p.screen_local.alpha, p.screen_remote.alpha), (1.0, 0.0));
        assert_eq!(p.cam_remote.alpha, 1.0);
        assert_eq!(
            (
                p.screen_local.x,
                p.screen_local.y,
                p.screen_local.w,
                p.screen_local.h
            ),
            (0, 135, 1440, 810)
        );

        let p = l.pads(&a, Some(&b), Some(&b));
        assert_eq!((p.screen_local.alpha, p.screen_remote.alpha), (0.0, 1.0));
        assert!(p.cam_local.zorder > p.screen_local.zorder && p.cam_remote.zorder > 0);

        // Both machines agree on stacking order.
        let pb = l.pads(&b, Some(&a), Some(&b));
        assert_eq!(pb.cam_local.zorder, p.cam_remote.zorder);
    }

    #[test]
    fn pads_respect_hidden_tiles() {
        let l = Layout::preset(Preset::ScreenOnly, &ids());
        let p = l.pads(&"a".into(), Some(&"b".into()), Some(&"a".into()));
        assert_eq!(p.screen_local.alpha, 1.0);
        assert_eq!(p.cam_local.alpha, 0.0);
        assert_eq!(p.cam_remote.alpha, 0.0);
    }
}
