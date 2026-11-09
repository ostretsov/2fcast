//! The mouse pointer in a shared window's picture, on a private Xvfb display
//! (so nothing touches the real desktop): a real `ximagesrc` with the app's
//! pointer probe, the window moved around while the pointer follows it, as
//! when the user drags a shared window by its title bar.
//!
//! Skipped when `Xvfb` is not installed.

use std::ffi::{c_int, c_void};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use gst::prelude::*;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt, CreateWindowAux, WindowClass};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const GRAY: u32 = 0x808080;
const W: u16 = 200;
const H: u16 = 150;
const SCREEN: (u16, u16) = (640, 480);

#[link(name = "X11")]
unsafe extern "C" {
    fn XSetErrorHandler(
        handler: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int>,
    ) -> *mut c_void;
}

unsafe extern "C" fn ignore_x_error(_: *mut c_void, _: *mut c_void) -> c_int {
    0
}

/// As in the app, where GDK's handler ignores X errors (Xlib's default one
/// exits): ximagesrc's capture of a partly off-screen window then fails
/// quietly and leaves an old picture in the buffer.
fn ignore_x_errors() {
    // SAFETY: a plain C function pointer, set before any Xlib use.
    unsafe { XSetErrorHandler(Some(ignore_x_error)) };
}

struct Xvfb(Child);

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Starts Xvfb on a free display and points `DISPLAY` at it.
fn xvfb() -> Option<Xvfb> {
    let mut child = Command::new("Xvfb")
        .args(["-displayfd", "1", "-screen", "0"])
        .arg(format!("{}x{}x24", SCREEN.0, SCREEN.1))
        .args(["-nolisten", "tcp"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let mut num = String::new();
    let mut byte = [0u8];
    while out.read(&mut byte).ok()? == 1 && byte[0] != b'\n' {
        num.push(byte[0] as char);
    }
    let num = num.trim().to_string();
    assert!(!num.is_empty(), "Xvfb did not report its display");
    // SAFETY: set before any thread of this test binary uses the display.
    unsafe { std::env::set_var("DISPLAY", format!(":{num}")) };
    Some(Xvfb(child))
}

struct Scene {
    conn: RustConnection,
    root: u32,
    win: u32,
}

impl Scene {
    fn new() -> Self {
        let (conn, n) = x11rb::connect(None).unwrap();
        let screen = &conn.setup().roots[n];
        let root = screen.root;
        let win = conn.generate_id().unwrap();
        conn.create_window(
            screen.root_depth,
            win,
            root,
            100,
            100,
            W,
            H,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().background_pixel(GRAY),
        )
        .unwrap();
        conn.map_window(win).unwrap();
        conn.sync().unwrap();
        Self { conn, root, win }
    }

    /// Moves the window to (`x`, `y`) and the pointer to (`px`, `py`)
    /// inside the window.
    fn place(&self, x: i32, y: i32, px: i16, py: i16) {
        self.conn
            .configure_window(self.win, &ConfigureWindowAux::new().x(x).y(y))
            .unwrap();
        self.conn
            .warp_pointer(
                x11rb::NONE,
                self.root,
                0,
                0,
                0,
                0,
                x as i16 + px,
                y as i16 + py,
            )
            .unwrap();
        self.conn.sync().unwrap();
    }
}

fn pipeline(xid: u32) -> (gst::Pipeline, gst_app::AppSink) {
    let desc = twofcast_core::pipelines::screen(
        &twofcast_core::pipelines::Channels::new("ptrtest"),
        &twofcast_core::pipelines::ScreenSource::Window(xid as u64),
        None,
    );
    // Only the source as the app builds it; the rest is ours.
    let src = desc.split(" ! ").next().unwrap();
    let pipe = gst::parse::launch(&format!(
        "{src} ! video/x-raw,format=BGRx,framerate=30/1 ! appsink name=sink sync=false max-buffers=1 drop=true"
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let pad = pipe
        .by_name("screensrc")
        .unwrap()
        .static_pad("src")
        .unwrap();
    twofcast::pointer::install(&pad, xid as u64).unwrap();
    let sink = pipe
        .by_name("sink")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();
    pipe.set_state(gst::State::Playing).unwrap();
    (pipe, sink)
}

type Box = (usize, usize, usize, usize);

/// Bounding box of the pointer in a frame captured after the scene settled.
fn pointer_box(sink: &gst_app::AppSink) -> Option<Box> {
    // Let frames captured before the last move pass.
    let mut sample = None;
    for _ in 0..5 {
        sample = sink.try_pull_sample(gst::ClockTime::from_seconds(2));
    }
    non_gray_box(&sample.expect("no frame"))
}

/// Bounding box (x0, y0, x1, y1) of the pixels that are not the window's gray.
fn non_gray_box(sample: &gst::Sample) -> Option<Box> {
    let info = gst_video::VideoInfo::from_caps(sample.caps().unwrap()).unwrap();
    let map = sample.buffer().unwrap().map_readable().unwrap();
    let stride = info.stride()[0] as usize;
    let mut bbox: Option<Box> = None;
    for y in 0..info.height() as usize {
        for x in 0..info.width() as usize {
            let p = &map[y * stride + x * 4..y * stride + x * 4 + 3];
            if p != [0x80, 0x80, 0x80] {
                let b = bbox.get_or_insert((x, y, x, y));
                *b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
            }
        }
    }
    bbox
}

fn assert_one_pointer_at(bbox: Option<Box>, px: usize, py: usize) {
    let (x0, y0, x1, y1) = bbox.expect("no pointer in the picture");
    // One pointer image (the X cursor is ~16 px), around its hot spot.
    assert!(
        x1 - x0 < 32 && y1 - y0 < 32,
        "more than one pointer (trail?): {x0},{y0}..{x1},{y1}"
    );
    assert!(
        (x0..=x1).contains(&px) && (y0..=y1).contains(&py),
        "pointer at {x0},{y0}..{x1},{y1}, expected around {px},{py}"
    );
}

#[test]
fn pointer_follows_moved_window_without_trails() {
    gst::init().unwrap();
    let Some(_xvfb) = xvfb() else {
        eprintln!("Xvfb not installed: skipped");
        return;
    };
    ignore_x_errors();
    let scene = Scene::new();
    let (pipe, sink) = pipeline(scene.win);

    scene.place(100, 100, 50, 40);
    assert_one_pointer_at(pointer_box(&sink), 50, 40);

    // Dragged: window and pointer move together, then the pointer moves.
    for (x, y, px, py) in [(130, 110, 50, 40), (300, 250, 50, 40), (300, 250, 120, 90)] {
        scene.place(x, y, px, py);
        assert_one_pointer_at(pointer_box(&sink), px as usize, py as usize);
    }

    // Pointer outside the window: clean picture.
    scene.place(300, 250, -60, -60);
    assert_eq!(pointer_box(&sink), None);

    // Partly off-screen, where ximagesrc's capture fails: frames are
    // dropped (no compositor in Xvfb; with one, GetImage still works), so
    // the last good picture is all anyone sees, never one with old pointers.
    for i in 0..10 {
        scene.place(500 + i, 250, 20 + i as i16 * 7, 20 + i as i16 * 5);
        if let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(100))
            && let Some((x0, y0, x1, y1)) = non_gray_box(&sample)
        {
            assert!(x1 - x0 < 32 && y1 - y0 < 32, "trail while off-screen");
        }
    }

    // Back on screen: live again, with one pointer.
    scene.place(200, 200, 70, 30);
    assert_one_pointer_at(pointer_box(&sink), 70, 30);

    pipe.set_state(gst::State::Null).unwrap();
    std::thread::sleep(Duration::from_millis(50));
}
