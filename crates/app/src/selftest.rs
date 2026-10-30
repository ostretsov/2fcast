//! `2fcast --self-test`: checks that this installation works, without a
//! camera, microphone or partner.  Used to verify packages on clean systems.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gst::prelude::*;
use gtk::glib;
use gtk::prelude::*;
use twofcast_core::deps;
use twofcast_core::layout::{self, Layout, Preset};
use twofcast_core::mic_channel::Mode as MicMode;
use twofcast_core::pipelines::{CamSource, Channels, MicSource, Ports, ScreenSource, TrackNames};

use crate::media::{self, Media, MediaConfig};

/// Elements used besides the ones in [`deps::ELEMENTS`].
const MORE_ELEMENTS: &[&str] = &[
    "queue",
    "tee",
    "valve",
    "identity",
    "volume",
    "level",
    "audiotestsrc",
    "decodebin",
    "fakesink",
    "filesrc",
    "matroskademux",
];

pub fn run() -> glib::ExitCode {
    let mut failed = false;
    let mut check = |ok: bool, what: &str| {
        println!("{} {what}", if ok { "ok  " } else { "FAIL" });
        failed |= !ok;
    };

    if let Err(e) = gst::init() {
        check(false, &format!("GStreamer: {e}"));
        return glib::ExitCode::FAILURE;
    }
    check(true, &format!("GStreamer {}", gst::version_string()));
    let missing: Vec<&str> = deps::ELEMENTS
        .iter()
        .map(|(e, _)| *e)
        .chain(MORE_ELEMENTS.iter().copied())
        .filter(|e| !e.split('|').any(|x| gst::ElementFactory::find(x).is_some()))
        .collect();
    check(
        missing.is_empty(),
        &format!(
            "GStreamer elements{}",
            if missing.is_empty() {
                String::new()
            } else {
                format!(", missing: {}", missing.join(" "))
            }
        ),
    );
    check(deps::in_path("pactl"), "pactl (shared-window sound)");
    check(true, &format!("H.264 decoder: {}", media::h264_decoder()));

    match media_loopback() {
        Ok(msg) => check(true, &msg),
        Err(e) => check(false, &format!("media: {e:#}")),
    }

    if std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some() {
        match show_window() {
            Ok(()) => check(true, "GTK window"),
            Err(e) => check(false, &format!("GTK window: {e:#}")),
        }
    } else {
        println!("skip GTK window (no display)");
    }

    println!(
        "{}",
        if failed {
            "SELF-TEST FAILED"
        } else {
            "SELF-TEST PASSED"
        }
    );
    if failed {
        glib::ExitCode::FAILURE
    } else {
        glib::ExitCode::SUCCESS
    }
}

/// Camera, mic and screen (test sources) are sent to ourselves over UDP,
/// received, composed and recorded for a few seconds; the file is checked.
fn media_loopback() -> anyhow::Result<String> {
    let errors: media::ErrorFn = Arc::new(|e| eprintln!("media error: {e}"));
    let base = 47950;
    let mut m = Media::new(
        MediaConfig {
            channels: Channels::new("selftest-"),
            ports: Ports::from_base(base),
            camera: Some(CamSource::Test(0x204060)),
            mic: Some(MicSource::Test),
            mic_channel: MicMode::Auto,
            noise_gate_db: Some(-45.0),
            high_pass_hz: None,
            compressor: false,
            duck_db: None,
            playback: false,
            h264_decoder: None,
        },
        errors,
    )?;
    m.set_peer(Some((
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        Ports::from_base(base),
    )));
    // Cameras side by side: our own on the left, the "partner's" (our own,
    // sent over UDP and decoded) on the right.
    let (me, peer) = ("me".to_string(), "peer".to_string());
    let layout = Layout::preset(Preset::CamsOnly, &[me.clone(), peer.clone()]);
    m.apply_pads(&layout.pads(&me, Some(&peer), None));
    m.start_screen(&ScreenSource::Test, Some("test"))?;
    let dir = std::env::temp_dir().join(format!("2fcast-selftest-{}", std::process::id()));
    let path = dir.join("selftest.mkv");
    let names = TrackNames {
        me: "Me".into(),
        peer: "Partner".into(),
    };
    m.start_recording(&path, 3000, "ultrafast", &names)?;
    let start = Instant::now();
    let mut heard_back = false;
    let mut saw_back = false;
    while start.elapsed() < Duration::from_secs(4) {
        heard_back |= m.voice_levels().1.is_some();
        saw_back |= m
            .latest_frame()
            .is_some_and(|f| is_test_color(&f, 1440.0, 300.0));
        std::thread::sleep(Duration::from_millis(100));
    }
    let saved = m.stop_recording().unwrap().recv_blocking()??;
    anyhow::ensure!(
        heard_back,
        "own voice did not come back over the LAN loopback"
    );
    anyhow::ensure!(
        saw_back,
        "own camera did not come back (H.264 decoding with {})",
        media::h264_decoder()
    );

    let mut desc = format!(
        "filesrc location={} ! matroskademux name=d d.video_0 ! queue ! fakesink",
        saved.display()
    );
    for i in 0..4 {
        desc += &format!(" d.audio_{i} ! queue ! fakesink");
    }
    let check = gst::parse::launch(&desc)?
        .downcast::<gst::Pipeline>()
        .unwrap();
    check.set_state(gst::State::Paused)?;
    let (res, _, _) = check.state(gst::ClockTime::from_seconds(10));
    res.map_err(|_| anyhow::anyhow!("recording is not a valid MKV with 1 video + 4 audio tracks"))?;
    let duration = check.query_duration::<gst::ClockTime>().unwrap_or_default();
    check.set_state(gst::State::Null)?;
    let _ = std::fs::remove_dir_all(&dir);
    anyhow::ensure!(
        duration >= gst::ClockTime::from_mseconds(2500),
        "recording too short: {duration}"
    );
    Ok(format!(
        "camera (decoded) / mic / screen over UDP loopback, {:.1} s recording with 1 video + 4 audio tracks",
        duration.seconds_f64()
    ))
}

/// Whether the preview shows the test camera's background color at canvas
/// point (x, y).
fn is_test_color(f: &media::Frame, x: f64, y: f64) -> bool {
    let s = f.width as f64 / layout::CANVAS_W;
    let o = (y * s) as usize * f.stride + (x * s) as usize * 4;
    let Some(px) = f.data.get(o..o + 3) else {
        return false;
    };
    let (b, g, r) = (px[0] as i32, px[1] as i32, px[2] as i32);
    (r - 0x20).abs() < 24 && (g - 0x40).abs() < 24 && (b - 0x60).abs() < 24
}

fn show_window() -> anyhow::Result<()> {
    gtk::init()?;
    let window = gtk::Window::builder()
        .title("2fcast self-test")
        .default_width(320)
        .default_height(120)
        .build();
    window.set_child(Some(&gtk::Label::new(Some("2fcast self-test"))));
    window.present();
    let ctx = glib::MainContext::default();
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(800) {
        ctx.iteration(false);
        std::thread::sleep(Duration::from_millis(10));
    }
    anyhow::ensure!(window.is_visible(), "window did not show");
    window.destroy();
    Ok(())
}
