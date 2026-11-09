//! Draws the mouse pointer into a shared window's frames (see
//! [`twofcast_core::cursor`] for why `ximagesrc` doesn't do it).

use gst::prelude::*;
use twofcast_core::cursor::{self, Cursor};
use x11rb::connection::Connection;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, Window};
use x11rb::rust_connection::RustConnection;

struct Overlay {
    conn: RustConnection,
    root: Window,
    window: Window,
}

enum Frame {
    Keep,
    Replace(gst::Buffer),
    Drop,
}

impl Overlay {
    fn connect(window: Window) -> anyhow::Result<Self> {
        let (conn, n) = x11rb::connect(None)?;
        let root = conn.setup().roots[n].root;
        conn.xfixes_query_version(5, 0)?.reply()?;
        Ok(Self { conn, root, window })
    }

    fn process(&self, buffer: &gst::Buffer, info: &gst_video::VideoInfo) -> Frame {
        let (w, h) = (info.width(), info.height());
        let stride = info.stride()[0] as usize;
        let screen = self
            .conn
            .get_geometry(self.root)
            .ok()
            .and_then(|c| c.reply().ok());
        let pos = self
            .conn
            .translate_coordinates(self.window, self.root, 0, 0)
            .ok()
            .and_then(|c| c.reply().ok());
        let (Some(screen), Some(pos)) = (screen, pos) else {
            return Frame::Keep;
        };
        let (wx, wy) = (pos.dst_x as i32, pos.dst_y as i32);
        let mut out = None;
        if !cursor::on_screen(wx, wy, w, h, screen.width as u32, screen.height as u32) {
            // ximagesrc's XShmGetImage failed: `buffer` holds an old picture.
            // A plain GetImage works off-screen too (with a compositor).
            let Some(nb) = self.get_image(buffer, w as usize, h as usize, stride) else {
                return Frame::Drop;
            };
            out = Some(nb);
        }
        if let Some(c) = self
            .conn
            .xfixes_get_cursor_image()
            .ok()
            .and_then(|c| c.reply().ok())
        {
            let cur = Cursor {
                x: c.x as i32 - c.xhot as i32 - wx,
                y: c.y as i32 - c.yhot as i32 - wy,
                width: c.width as usize,
                height: c.height as usize,
                pixels: &c.cursor_image,
            };
            if cur.overlaps(w as usize, h as usize) {
                // Never into ximagesrc's own (reused) buffer: a later failed
                // capture would bring this pointer back.
                if out.is_none() {
                    out = buffer.copy_deep().ok();
                }
                if let Some(nb) = &mut out
                    && let Ok(mut map) = nb.make_mut().map_writable()
                {
                    cur.draw(&mut map, w as usize, h as usize, stride);
                }
            }
        }
        out.map_or(Frame::Keep, Frame::Replace)
    }

    /// `buffer` with its picture replaced by the window's current contents.
    fn get_image(
        &self,
        buffer: &gst::Buffer,
        w: usize,
        h: usize,
        stride: usize,
    ) -> Option<gst::Buffer> {
        let img = self
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.window,
                0,
                0,
                w as u16,
                h as u16,
                !0,
            )
            .ok()?
            .reply()
            .ok()?;
        let row = w * 4;
        if img.data.len() < row * h {
            return None;
        }
        let mut nb = buffer.copy_deep().ok()?;
        {
            let mut map = nb.make_mut().map_writable().ok()?;
            for y in 0..h {
                map.get_mut(y * stride..y * stride + row)?
                    .copy_from_slice(&img.data[y * row..(y + 1) * row]);
            }
        }
        Some(nb)
    }
}

/// Draws the pointer into the BGRx frames leaving `pad` (an `ximagesrc`
/// capturing `window` with `show-pointer=false`).
pub fn install(pad: &gst::Pad, window: u64) -> anyhow::Result<()> {
    let overlay = Overlay::connect(window.try_into()?)?;
    pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
        let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data else {
            return gst::PadProbeReturn::Ok;
        };
        let Some(vinfo) = pad
            .current_caps()
            .and_then(|c| gst_video::VideoInfo::from_caps(&c).ok())
        else {
            return gst::PadProbeReturn::Ok;
        };
        match overlay.process(buffer, &vinfo) {
            Frame::Keep => gst::PadProbeReturn::Ok,
            Frame::Replace(nb) => {
                *buffer = nb;
                gst::PadProbeReturn::Ok
            }
            Frame::Drop => gst::PadProbeReturn::Drop,
        }
    });
    Ok(())
}
