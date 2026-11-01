//! End-to-end media tests: two complete "machines" in one process, talking
//! over UDP on localhost, using test camera/microphone/screen sources.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gst::prelude::*;
use twofcast::media::{Frame, Media, MediaConfig};
use twofcast_core::annotations::{AnnotOp, Annotations, Pt};
use twofcast_core::layout::{CANVAS_W, Layout, Preset};
use twofcast_core::pipelines::{
    CamSource, Channels, MicSource, PREVIEW_W, Ports, ScreenSource, TrackNames,
};

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
    machine_cfg(prefix, base, color, |c| {
        c.mic = mic;
        c.playback = playback;
    })
}

/// A machine with a test camera and mic, no voice filters (but the gate),
/// then `adjust`ed.
fn machine_cfg(
    prefix: &str,
    base: u16,
    color: u32,
    adjust: impl FnOnce(&mut MediaConfig),
) -> Media {
    gst::init().unwrap();
    take_slot();
    let errors: twofcast::media::ErrorFn = Arc::new(|e| eprintln!("media error: {e}"));
    let mut cfg = MediaConfig {
        channels: Channels::new(prefix),
        ports: Ports::from_base(base),
        camera: Some(CamSource::Test(color)),
        mic: Some(MicSource::Test),
        mic_channel: twofcast_core::mic_channel::Mode::Auto,
        noise_gate_db: Some(-45.0),
        high_pass_hz: None,
        compressor: false,
        duck_db: None,
        playback: false,
        h264_decoder: DECODER.with(|d| d.get()),
    };
    adjust(&mut cfg);
    Media::new(cfg, errors).unwrap()
}

/// Each media test runs two complete stacks (1080p compositing, encoding);
/// running all of them at once overloads a laptop and makes timings
/// meaningless.  At most three test threads hold media at a time; a thread
/// keeps its slot until it ends.
fn take_slot() {
    if HELD.with(|h| h.replace(true)) {
        return;
    }
    let mut used = SLOTS.lock().unwrap();
    while *used >= 3 {
        used = FREED.wait(used).unwrap();
    }
    *used += 1;
    RELEASE.with(|_| {}); // registers the release for when this thread ends
}

static SLOTS: std::sync::Mutex<u32> = std::sync::Mutex::new(0);
static FREED: std::sync::Condvar = std::sync::Condvar::new();
thread_local! {
    static HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static RELEASE: Release = const { Release };
}

struct Release;
impl Drop for Release {
    fn drop(&mut self) {
        *SLOTS.lock().unwrap() -= 1;
        FREED.notify_one();
    }
}

thread_local!(static DECODER: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) });

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
    wait_frame(m, what, check, || {});
}

/// Waits until `check` holds on `receiver`, which receives `sender`'s screen;
/// keyframe requests are passed on as the control connection would.
fn wait_for_shared(sender: &Media, receiver: &Media, what: &str, check: impl Fn(&Frame) -> bool) {
    wait_frame(receiver, what, check, || {
        if receiver.screen_keyframe_due() {
            sender.force_screen_keyframe();
        }
        receiver.refresh_pads();
    });
}

fn wait_frame(m: &Media, what: &str, check: impl Fn(&Frame) -> bool, tick: impl Fn()) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last = None;
    while Instant::now() < deadline {
        tick();
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
    cameras_are_exchanged("cam-", 47300);
}

/// The self-contained package decodes with OpenH264, not FFmpeg; a sender
/// setting once made OpenH264 decode nothing (black partner camera).
#[test]
fn peers_see_each_others_camera_with_openh264() {
    DECODER.with(|d| d.set(Some("openh264dec")));
    cameras_are_exchanged("cam264-", 47720);
}

fn cameras_are_exchanged(prefix: &str, base: u16) {
    // No microphones but real speakers, like a session where nobody talks yet.
    let mut a = machine_with(&format!("{prefix}a-"), base, A_COLOR, None, true);
    let mut b = machine_with(&format!("{prefix}b-"), base + 20, B_COLOR, None, true);
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
    a.set_peer(Some((LOCALHOST, Ports::from_base(base + 20))));
    b.set_peer(Some((LOCALHOST, Ports::from_base(base))));
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
    wait_for_shared(&a, &b, "A's shared screen on B", smpte_visible);

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
    let names = TrackNames {
        me: "Artem O.".into(),
        peer: "Брат \"Ivan\"".into(),
    };
    a.start_recording(&path, 3000, "ultrafast", &names).unwrap();
    assert!(a.is_recording());
    std::thread::sleep(Duration::from_secs(4));
    let rx = a.stop_recording().unwrap();
    assert!(!a.is_recording());
    let saved = rx.recv_blocking().unwrap().expect("recording finalized");
    assert_eq!(saved, path);

    // Read it back: one H.264 1920x1080 video stream and four named Opus
    // tracks: mix, my voice, partner's voice, shared sound.
    let mut desc = format!(
        "filesrc location={} ! matroskademux name=d \
         d.video_0 ! queue ! h264parse ! video/x-h264,width=1920,height=1080 ! fakesink",
        path.display()
    );
    // Mix and shared sound are stereo, the two voices mono.
    for (i, channels) in [2, 1, 1, 2].into_iter().enumerate() {
        desc += &format!(
            " d.audio_{i} ! queue ! opusparse ! audio/x-opus,channels={channels} ! fakesink name=a{i}"
        );
    }
    let check = gst::parse::launch(&desc)
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

    // Track titles (GStreamer's demuxer only reports the default track names).
    match std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a"])
        .args(["-show_entries", "stream_tags=title", "-of", "csv=p=0"])
        .arg(&path)
        .output()
    {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            let titles: Vec<&str> = text.lines().collect();
            assert_eq!(titles, ["Mix", "Artem_O.", "Ivan", "Shared_sound"]);
        }
        Err(_) => eprintln!("ffprobe not installed; track titles not checked"),
    }
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

#[test]
fn voice_levels_are_measured_on_both_sides() {
    let mut a = machine("lvl-a-", 47460, A_COLOR);
    let mut b = machine("lvl-b-", 47480, B_COLOR);
    a.set_peer(Some((LOCALHOST, Ports::from_base(47480))));
    b.set_peer(Some((LOCALHOST, Ports::from_base(47460))));
    // Test tone: sine at amplitude 0.05 = -26 dBFS peak, -29 dBFS RMS.
    let tone = |l: Option<twofcast_core::levels::Level>| {
        l.is_some_and(|l| (-33.0..-23.0).contains(&l.rms_db) && l.peak_db > l.rms_db)
    };
    wait_until("own and partner's level", || {
        tone(a.voice_levels().0) && tone(a.voice_levels().1) && tone(b.voice_levels().1)
    });

    a.set_mic_muted(true);
    let silent = |l: Option<twofcast_core::levels::Level>| l.is_some_and(|l| l.peak_db < -60.0);
    wait_until("muted mic reads silent on both sides", || {
        silent(a.voice_levels().0) && silent(b.voice_levels().1)
    });

    // Partner leaves: no reading for them any more.
    a.set_peer(None);
    wait_until("partner level gone", || a.voice_levels().1.is_none());
}

fn wait_until(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn rms(l: Option<twofcast_core::levels::Level>) -> f64 {
    l.map(|l| l.rms_db).unwrap_or(f64::NEG_INFINITY)
}

#[test]
fn mic_on_one_input_channel_is_detected_and_centered() {
    use twofcast_core::mic_channel::Pick;
    // Like a Scarlett with the mic on input 1: sound on the left channel only.
    let a = machine_with(
        "pick-a-",
        47500,
        A_COLOR,
        Some(MicSource::ToneLeftOnly(0.05)),
        false,
    );
    // Averaging both channels first: 6 dB below the tone's -29 dBFS RMS.
    wait_until("averaged level", || {
        (-37.0..-33.0).contains(&rms(a.voice_levels().0))
    });
    assert_eq!(a.mic_pick(), Pick::Both);
    // After a second of "speech" only the left channel is used: full level.
    wait_until("switch to the left input", || a.mic_pick() == Pick::Left);
    wait_until("full level after the switch", || {
        (-31.0..-27.0).contains(&rms(a.voice_levels().0))
    });
}

#[test]
fn noise_gate_silences_quiet_input_but_passes_speech() {
    // Room noise: a tone at -56 dBFS RMS, below the -45 dB gate threshold.
    let mut quiet = machine_with(
        "gate-a-",
        47520,
        A_COLOR,
        Some(MicSource::Tone(0.002)),
        false,
    );
    let mut b = machine_with(
        "gate-b-",
        47540,
        B_COLOR,
        Some(MicSource::Tone(0.05)),
        false,
    );
    quiet.set_peer(Some((LOCALHOST, Ports::from_base(47540))));
    b.set_peer(Some((LOCALHOST, Ports::from_base(47520))));
    // B hears A's "noise" as silence; A hears B's "speech" normally.
    wait_until("partner receives voice", || {
        (-33.0..-25.0).contains(&rms(quiet.voice_levels().1))
    });
    wait_until("noise arrives as silence", || {
        b.voice_levels().1.is_some_and(|l| l.peak_db < -60.0)
    });
    assert!(
        rms(quiet.voice_levels().0) < -60.0,
        "own meter shows the gated (silent) voice"
    );
    assert_eq!(quiet.noise_gate_db(), Some(-45.0));
}

#[test]
fn voice_filters_shape_what_the_partner_hears() {
    // A 440 Hz tone at about -29 dBFS: just under the compressor threshold,
    // so it gets the +6 dB make-up gain; the high-pass leaves 440 Hz alone.
    let mut a = machine_cfg("filt-a-", 47640, A_COLOR, |c| {
        c.mic = Some(MicSource::Tone(0.05));
        c.high_pass_hz = Some(80.0);
        c.compressor = true;
        c.duck_db = Some(6.0);
    });
    let mut b = machine_cfg("filt-b-", 47660, B_COLOR, |c| {
        c.mic = Some(MicSource::Tone(0.05));
    });
    a.set_peer(Some((LOCALHOST, Ports::from_base(47660))));
    b.set_peer(Some((LOCALHOST, Ports::from_base(47640))));
    wait_until("B hears A louder (compressor make-up)", || {
        (-27.0..-20.0).contains(&rms(b.voice_levels().1))
    });
    wait_until("A hears B as is", || {
        (-33.0..-25.0).contains(&rms(a.voice_levels().1))
    });
}

#[test]
fn receiver_that_starts_late_still_gets_the_picture() {
    // The partner takes over sharing while we are running: our receiver starts
    // seconds after their stream did (missed the first keyframe).
    let mut a = machine("late-a-", 47560, A_COLOR);
    let mut b = machine("late-b-", 47580, B_COLOR);
    let layout = Layout::preset(Preset::ScreenOnly, &ids());
    let (me_a, me_b) = ("a".to_string(), "b".to_string());
    a.set_peer(Some((LOCALHOST, Ports::from_base(47580))));
    b.set_peer(Some((LOCALHOST, Ports::from_base(47560))));
    a.start_screen(&ScreenSource::Test, None).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    b.apply_pads(&layout.pads(&me_b, Some(&me_a), Some(&me_a)));
    b.set_receive_screen(true);
    wait_for_shared(&a, &b, "late receiver decodes", smpte_visible);
}

#[test]
fn partner_screen_appears_only_with_a_complete_picture() {
    // A's screen stream has no regular keyframes; B starts receiving after
    // A's first one.  Until B's request is answered, frames arrive but none
    // is complete: B's shared space stays black (no grey or broken picture).
    let mut a = machine("kf-a-", 47760, A_COLOR);
    let mut b = machine("kf-b-", 47780, B_COLOR);
    let layout = Layout::preset(Preset::ScreenOnly, &ids());
    let (me_a, me_b) = ("a".to_string(), "b".to_string());
    a.set_peer(Some((LOCALHOST, Ports::from_base(47780))));
    b.set_peer(Some((LOCALHOST, Ports::from_base(47760))));
    a.start_screen(&ScreenSource::Test, None).unwrap();
    std::thread::sleep(Duration::from_secs(2));
    b.apply_pads(&layout.pads(&me_b, Some(&me_a), Some(&me_a)));
    b.set_receive_screen(true);
    assert!(b.screen_keyframe_due(), "asks for a keyframe right away");
    assert!(!b.screen_keyframe_due(), "but not again at once");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        b.refresh_pads();
        std::thread::sleep(Duration::from_millis(100));
    }
    let f = b.latest_frame().expect("preview runs");
    assert!(
        near(pixel(&f, 100.0, 200.0), 0x000000),
        "nothing shown before a keyframe: {:?}",
        pixel(&f, 100.0, 200.0)
    );
    wait_for_shared(&a, &b, "picture after the keyframe", smpte_visible);
    assert!(!b.screen_keyframe_due(), "no more requests once it arrived");
}

/// A PNG of one color.
fn png(dir: &std::path::Path, w: i32, h: i32, rgb: u32) -> std::path::PathBuf {
    use gtk::gdk_pixbuf::{Colorspace, Pixbuf};
    let p = Pixbuf::new(Colorspace::Rgb, false, 8, w, h).unwrap();
    p.fill(rgb << 8 | 0xff);
    let path = dir.join("idle.png");
    p.savev(&path, "png", &[]).unwrap();
    path
}

#[test]
fn idle_picture_shows_while_nobody_shares() {
    const IDLE: u32 = 0xc03080;
    let mut a = machine("idle-a-", 47800, A_COLOR);
    let mut b = machine("idle-b-", 47820, B_COLOR);
    a.set_peer(Some((LOCALHOST, Ports::from_base(47820))));
    b.set_peer(Some((LOCALHOST, Ports::from_base(47800))));
    let layout = Layout::preset(Preset::ScreenOnly, &ids());
    let (me_a, me_b) = ("a".to_string(), "b".to_string());
    let dir = tempfile::tempdir().unwrap();

    assert!(
        a.set_idle_image(Some(&dir.path().join("missing.png")))
            .is_err()
    );
    let size = a
        .set_idle_image(Some(&png(dir.path(), 1920, 1080, IDLE)))
        .unwrap();
    assert_eq!(size, Some((1920, 1080)));
    a.apply_pads(&layout.pads(&me_a, Some(&me_b), None));
    wait_for(&a, "idle picture", |f| near(pixel(f, 100.0, 200.0), IDLE));
    assert!(
        a.set_idle_image(Some(&dir.path().join("missing.png")))
            .is_err()
    );
    assert_eq!(
        a.idle_image_size(),
        Some((1920, 1080)),
        "a bad file keeps the picture"
    );

    // I share: my screen replaces it.
    a.start_screen(&ScreenSource::Test, None).unwrap();
    a.apply_pads(&layout.pads(&me_a, Some(&me_b), Some(&me_a)));
    wait_for(&a, "own screen over the picture", smpte_visible);
    a.stop_screen();

    // The partner shares: the picture stays until their first complete
    // picture arrives (no black gap), then theirs is shown.
    b.start_screen(&ScreenSource::Test, None).unwrap();
    std::thread::sleep(Duration::from_secs(1));
    a.apply_pads(&layout.pads(&me_a, Some(&me_b), Some(&me_b)));
    a.set_receive_screen(true);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(1) {
        a.refresh_pads();
        std::thread::sleep(Duration::from_millis(100));
    }
    let f = a.latest_frame().unwrap();
    assert!(
        near(pixel(&f, 100.0, 200.0), IDLE),
        "{:?}",
        pixel(&f, 100.0, 200.0)
    );
    wait_for_shared(&b, &a, "partner's screen", smpte_visible);

    // Nobody shares again, then the picture is removed: black.
    a.set_receive_screen(false);
    a.apply_pads(&layout.pads(&me_a, Some(&me_b), None));
    wait_for(&a, "idle picture again", |f| {
        near(pixel(f, 100.0, 200.0), IDLE)
    });
    a.set_idle_image(None).unwrap();
    wait_for(&a, "black without a picture", |f| {
        near(pixel(f, 100.0, 200.0), 0x000000)
    });
}

#[test]
fn partner_takes_over_sharing_while_we_record() {
    let mut a = machine_with("take-a-", 47600, A_COLOR, Some(MicSource::Test), true);
    let mut b = machine("take-b-", 47620, B_COLOR);
    let layout = Layout::preset(Preset::ScreenOnly, &ids());
    let (me_a, me_b) = ("a".to_string(), "b".to_string());
    a.set_peer(Some((LOCALHOST, Ports::from_base(47620))));
    b.set_peer(Some((LOCALHOST, Ports::from_base(47600))));
    // A shares and records; B watches.
    a.start_screen(&ScreenSource::Test, Some("test")).unwrap();
    a.apply_pads(&layout.pads(&me_a, Some(&me_b), Some(&me_a)));
    let dir = tempfile::tempdir().unwrap();
    a.start_recording(
        &dir.path().join("take.mkv"),
        3000,
        "ultrafast",
        &TrackNames {
            me: "A".into(),
            peer: "B".into(),
        },
    )
    .unwrap();
    b.set_receive_screen(true);
    b.apply_pads(&layout.pads(&me_b, Some(&me_a), Some(&me_a)));
    wait_for_shared(&a, &b, "B sees A's screen", smpte_visible);
    // B takes over: B captures, A stops and receives.
    b.set_receive_screen(false);
    b.start_screen(&ScreenSource::Test, Some("test")).unwrap();
    a.stop_screen();
    a.set_receive_screen(true);
    a.apply_pads(&layout.pads(&me_a, Some(&me_b), Some(&me_b)));
    let start = Instant::now();
    wait_for_shared(&b, &a, "A sees B's screen", smpte_visible);
    let took = start.elapsed();
    assert!(
        took < Duration::from_secs(3),
        "picture after takeover took {took:?}"
    );
    assert!(a.stop_recording().unwrap().recv_blocking().unwrap().is_ok());
}

#[test]
fn notices_when_nothing_arrives_from_the_peer() {
    let mut a = machine("silent-a-", 47640, A_COLOR);
    assert_eq!(a.peer_silent_for(), None, "not connected");
    // A "peer" that never sends anything (like one behind a firewall).
    a.set_peer(Some((LOCALHOST, Ports::from_base(47700))));
    std::thread::sleep(Duration::from_secs(2));
    assert!(a.peer_silent_for().unwrap() >= Duration::from_millis(1800));
    assert_eq!(a.peer_media_arrives(), None, "too early to tell");
    // A real peer: its camera and voice arrive.
    let mut b = machine("silent-b-", 47660, B_COLOR);
    b.set_peer(Some((LOCALHOST, Ports::from_base(47640))));
    wait_until("packets from the peer", || {
        a.peer_silent_for().unwrap() < Duration::from_millis(500)
    });
    assert_eq!(a.peer_media_arrives(), Some(true));
}
