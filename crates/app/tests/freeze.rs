//! A lost RTP packet freezes the received picture until the next keyframe
//! instead of letting the decoder show grey blocks and smeared frames.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gst::prelude::*;
use twofcast::media::{freeze_on_loss, h264_decoder};
use twofcast_core::pipelines::{VIDEO_DEPAY, VIDEO_PARSE};

const FPS: u64 = 30;
const KEY_INT: u64 = 30;
const FRAMES: u64 = 3 * KEY_INT;
/// The frame whose first packet is lost: between the keyframes 30 and 60.
const LOST: u64 = 40;

/// Frame number of a timestamp (x264enc adds 1000 hours to them).
fn frame_of(pts: gst::ClockTime) -> u64 {
    ((pts.nseconds() * FPS + 500_000_000) / 1_000_000_000) % (3600 * FPS)
}

/// Streams a moving picture through encoder, RTP and the receive chain,
/// drops the first packet of frame [`LOST`] and returns the numbers of the
/// frames that came out of the decoder.
fn decoded_frames(freeze: bool) -> Vec<u64> {
    gst::init().unwrap();
    let desc = format!(
        "videotestsrc is-live=true pattern=ball num-buffers={FRAMES} ! \
         video/x-raw,width=320,height=240,framerate={FPS}/1 ! \
         x264enc tune=zerolatency speed-preset=ultrafast key-int-max={KEY_INT} ! \
         rtph264pay name=pay config-interval=1 mtu=400 ! \
         rtpjitterbuffer latency=50 ! rtph264depay name={VIDEO_DEPAY} ! h264parse name={VIDEO_PARSE} ! \
         {} ! fakesink name=out sync=false",
        h264_decoder()
    );
    let pipeline = gst::parse::launch(&desc)
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
    let pay = pipeline.by_name("pay").unwrap().static_pad("src").unwrap();
    let dropped = AtomicBool::new(false);
    pay.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(b)) = &info.data
            && b.pts().is_some_and(|p| frame_of(p) == LOST)
            && !dropped.swap(true, Ordering::Relaxed)
        {
            return gst::PadProbeReturn::Drop;
        }
        gst::PadProbeReturn::Ok
    });
    if freeze {
        freeze_on_loss(&pipeline, "test");
    }
    let frames = Arc::new(Mutex::new(Vec::new()));
    let out = pipeline.by_name("out").unwrap().static_pad("sink").unwrap();
    let f = frames.clone();
    out.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(b)) = &info.data
            && let Some(pts) = b.pts()
        {
            f.lock().unwrap().push(frame_of(pts));
        }
        gst::PadProbeReturn::Ok
    });
    pipeline.set_state(gst::State::Playing).unwrap();
    let bus = pipeline.bus().unwrap();
    let msg = bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(20),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );
    pipeline.set_state(gst::State::Null).unwrap();
    match msg.as_ref().map(|m| m.view()) {
        Some(gst::MessageView::Eos(_)) => {}
        other => panic!("stream did not finish: {other:?}"),
    }
    frames.lock().unwrap().clone()
}

fn between(frames: &[u64], from: u64, to: u64) -> usize {
    frames.iter().filter(|&&f| (from..to).contains(&f)).count()
}

#[test]
fn without_the_freeze_damaged_frames_are_shown() {
    let frames = decoded_frames(false);
    assert!(
        between(&frames, LOST + 1, 2 * KEY_INT) > 0,
        "the decoder should show the damaged frames (else the test proves nothing): {frames:?}"
    );
}

#[test]
fn a_lost_packet_holds_the_last_picture_until_the_next_keyframe() {
    let frames = decoded_frames(true);
    assert!(
        between(&frames, 0, LOST) == LOST as usize,
        "frames before the loss: {frames:?}"
    );
    assert_eq!(
        between(&frames, LOST, 2 * KEY_INT),
        0,
        "nothing from the loss to the keyframe: {frames:?}"
    );
    assert!(
        between(&frames, 2 * KEY_INT, FRAMES) > KEY_INT as usize / 2,
        "moving again after the keyframe: {frames:?}"
    );
}
