//! End-to-end media tests: two complete "machines" in one process, talking
//! over UDP on localhost, using test camera/microphone/screen sources.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gst::prelude::*;
use twofcast::media::{Frame, Media, MediaConfig};
use twofcast_core::annotations::{AnnotOp, Annotations, Pt};
use twofcast_core::layout::{CANVAS_W, Layout, Preset};
use twofcast_core::pipelines::{CamSource, Channels, MicSource, PREVIEW_W, Ports, ScreenSource};

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const A_COLOR: u32 = 0x204060;
const B_COLOR: u32 = 0x60a020;

fn machine(prefix: &str, base: u16, color: u32) -> Media {
    machine_with(prefix, base, color, Some(MicSource::Test), false)
}

fn machine_with(
    prefix: &str,
    base: u16,
    color: u32,
    mic: Option<MicSource>,
    playback: bool,
) -> Media {
    gst::init().unwrap();
    let errors: twofcast::media::ErrorFn = Arc::new(|e| eprintln!("media error: {e}"));
    Media::new(
        MediaConfig {
            channels: Channels::new(prefix),
            ports: Ports::from_base(base),
            camera: Some(CamSource::Test(color)),
            mic,
            playback,
        },
        errors,
    )
    .unwrap()
}

/// BGRA pixel of the preview at canvas coordinates.
fn pixel(f: &Frame, cx: f64, cy: f64) -> [u8; 3] {
    let s = PREVIEW_W as f64 / CANVAS_W;
    let (x, y) = ((cx * s) as usize, (cy * s) as usize);
    let o = y * f.stride + x * 4;
    [f.data[o + 2], f.data[o + 1], f.data[o]]
}

fn near(p: [u8; 3], rgb: u32) -> bool {
    let want = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8];
    p.iter()
        .zip(want)
        .all(|(a, b)| (*a as i32 - b as i32).abs() <= 20)
}

/// Waits until `check` holds for the latest preview frame.
fn wait_for(m: &Media, what: &str, check: impl Fn(&Frame) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last = None;
    while Instant::now() < deadline {
        if let Some(f) = m.latest_frame() {
            if check(&f) {
                return;
            }
            last = Some(f);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let detail = last
        .map(|f| format!("{:?}", pixel(&f, 960.0, 540.0)))
        .unwrap_or("no frame".into());
    panic!("timed out waiting for {what}; center pixel {detail}");
}

/// Points near the corners of a tile (the test ball moves through the middle).
fn tile_matches(f: &Frame, x: f64, y: f64, w: f64, h: f64, rgb: u32) -> bool {
    let pts = [
        (x + 20.0, y + 20.0),
        (x + w - 20.0, y + 20.0),
        (x + 20.0, y + h - 20.0),
        (x + w - 20.0, y + h - 20.0),
    ];
    pts.iter()
        .filter(|(px, py)| near(pixel(f, *px, *py), rgb))
        .count()
        >= 3
}

fn ids() -> Vec<String> {
    vec!["a".into(), "b".into()]
}

#[test]
fn peers_see_each_others_camera() {
    // No microphones but real speakers, like a session where nobody talks yet.
    let mut a = machine_with("cam-a-", 47300, A_COLOR, None, true);
    let mut b = machine_with("cam-b-", 47320, B_COLOR, None, true);
    let layout = Layout::preset(Preset::CamsOnly, &ids());
    let (me_a, me_b) = ("a".to_string(), "b".to_string());
    a.apply_pads(&layout.pads(&me_a, None, None));
    // Alone: own camera on the left, nothing on the right.
    wait_for(&a, "own camera", |f| {
        tile_matches(f, 0.0, 270.0, 960.0, 540.0, A_COLOR)
            && tile_matches(f, 960.0, 270.0, 960.0, 540.0, 0x000000)
    });

    a.apply_pads(&layout.pads(&me_a, Some(&me_b), None));
    b.apply_pads(&layout.pads(&me_b, Some(&me_a), None));
    // In the app the peer shows up a while after the pipelines started.
    std::thread::sleep(Duration::from_secs(2));
    a.set_peer(Some((LOCALHOST, Ports::from_base(47320))));
    b.set_peer(Some((LOCALHOST, Ports::from_base(47300))));
    wait_for(&a, "B's camera on A", |f| {
        tile_matches(f, 960.0, 270.0, 960.0, 540.0, B_COLOR)
            && tile_matches(f, 0.0, 270.0, 960.0, 540.0, A_COLOR)
    });
    wait_for(&b, "A's camera on B", |f| {
        tile_matches(f, 0.0, 270.0, 960.0, 540.0, A_COLOR)
            && tile_matches(f, 960.0, 270.0, 960.0, 540.0, B_COLOR)
    });

    // Peer leaves: its tile goes dark again.
    a.set_peer(None);
    a.apply_pads(&layout.pads(&me_a, None, None));
    wait_for(&a, "remote tile cleared", |f| {
        tile_matches(f, 960.0, 270.0, 960.0, 540.0, 0x000000)
    });
}

/// SMPTE test pattern: the top-left bar is a bright neutral white/gray.
fn smpte_visible(f: &Frame) -> bool {
    let [r, g, b] = pixel(f, 100.0, 200.0);
    r > 150 && (r as i32 - g as i32).abs() < 20 && (r as i32 - b as i32).abs() < 20
}

#[test]
fn shared_screen_reaches_peer_and_annotations_are_drawn() {
    let mut a = machine("scr-a-", 47340, A_COLOR);
    let mut b = machine("scr-b-", 47360, B_COLOR);
    let layout = Layout::preset(Preset::ScreenOnly, &ids());
    let (me_a, me_b) = ("a".to_string(), "b".to_string());
    a.set_peer(Some((LOCALHOST, Ports::from_base(47360))));
    b.set_peer(Some((LOCALHOST, Ports::from_base(47340))));

    a.start_screen(&ScreenSource::Test, Some("test")).unwrap();
    a.apply_pads(&layout.pads(&me_a, Some(&me_b), Some(&me_a)));
    b.apply_pads(&layout.pads(&me_b, Some(&me_a), Some(&me_a)));
    b.set_receive_screen(true);
    wait_for(&a, "own shared screen", smpte_visible);
    wait_for(&b, "A's shared screen on B", smpte_visible);

    // A red line across the middle of the shared space.
    let mut ann = Annotations::default();
    ann.apply(&AnnotOp::StrokeBegin {
        id: 1,
        color: [255, 0, 0],
        width: 0.05,
        at: Pt { x: 0.0, y: 0.5 },
    });
    ann.apply(&AnnotOp::StrokeTo {
        id: 1,
        pts: vec![Pt { x: 1.0, y: 0.5 }],
    });
    b.set_overlay(&ann, Some(layout.screen.rect));
    wait_for(&b, "annotation", |f| {
        near(pixel(f, 960.0, 540.0), 0xff0000) && smpte_visible(f)
    });

    // Presenter stops: the shared space goes dark.
    a.stop_screen();
    b.set_receive_screen(false);
    b.set_overlay(&Annotations::default(), None);
    b.apply_pads(&layout.pads(&me_b, Some(&me_a), None));
    wait_for(&b, "shared space cleared", |f| {
        near(pixel(f, 100.0, 200.0), 0x000000)
    });
}

#[test]
fn records_video_and_audio_to_mkv() {
    let mut a = machine("rec-a-", 47380, A_COLOR);
    let mut b = machine("rec-b-", 47400, B_COLOR);
    a.set_peer(Some((LOCALHOST, Ports::from_base(47400))));
    b.set_peer(Some((LOCALHOST, Ports::from_base(47380))));
    let layout = Layout::preset(Preset::ScreenCamsRight, &ids());
    a.start_screen(&ScreenSource::Test, Some("test")).unwrap();
    a.apply_pads(&layout.pads(
        &"a".to_string(),
        Some(&"b".to_string()),
        Some(&"a".to_string()),
    ));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("test.mkv");
    a.start_recording(&path, 3000, "ultrafast").unwrap();
    assert!(a.is_recording());
    std::thread::sleep(Duration::from_secs(4));
    let rx = a.stop_recording().unwrap();
    assert!(!a.is_recording());
    let saved = rx.recv_blocking().unwrap().expect("recording finalized");
    assert_eq!(saved, path);

    // Read it back: one H.264 1920x1080 video stream, one Opus stereo stream.
    let check = gst::parse::launch(&format!(
        "filesrc location={} ! matroskademux name=d \
         d.video_0 ! queue ! h264parse ! video/x-h264,width=1920,height=1080 ! fakesink \
         d.audio_0 ! queue ! opusparse ! audio/x-opus,channels=2 ! fakesink",
        path.display()
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    check.set_state(gst::State::Paused).unwrap();
    let (res, _, _) = check.state(gst::ClockTime::from_seconds(10));
    res.expect("file prerolls");
    let duration = check
        .query_duration::<gst::ClockTime>()
        .expect("has duration");
    assert!(
        duration >= gst::ClockTime::from_mseconds(3000),
        "duration {duration}"
    );
    check.set_state(gst::State::Playing).unwrap();
    let msg = check.bus().unwrap().timed_pop_filtered(
        gst::ClockTime::from_seconds(20),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );
    assert!(
        matches!(
            msg.as_ref().map(|m| m.view()),
            Some(gst::MessageView::Eos(_))
        ),
        "{msg:?}"
    );
    check.set_state(gst::State::Null).unwrap();
}

#[test]
fn mute_and_peer_changes_are_safe_at_any_time() {
    let mut a = machine("misc-a-", 47420, A_COLOR);
    a.set_mic_muted(true);
    a.set_mic_muted(false);
    a.set_receive_screen(true); // no peer yet: harmless
    a.set_peer(Some((LOCALHOST, Ports::from_base(47440))));
    a.set_peer(Some((LOCALHOST, Ports::from_base(47440))));
    a.start_screen(&ScreenSource::Test, None).unwrap();
    a.set_peer(None);
    a.stop_screen();
    assert!(a.stop_recording().is_none());
    assert!(a.has_camera());
}
