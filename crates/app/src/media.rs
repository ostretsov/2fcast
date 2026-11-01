//! Owns the GStreamer pipelines of one machine (see
//! [`twofcast_core::pipelines`] for the overall graph).

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use gst::prelude::*;
use gtk::glib;
use twofcast_core::annotations::Annotations;
use twofcast_core::camera::{self, CamMode};
use twofcast_core::idle_image;
use twofcast_core::keyframe;
use twofcast_core::layout::{PadParams, Pads, Rect};
use twofcast_core::levels::Level;
use twofcast_core::mic_channel::{Mode as MicMode, Pick, Picker};
use twofcast_core::pipelines::{
    self, CamSource, Channels, MicSource, Ports, ScreenSource, TrackNames,
};
use twofcast_core::voice::{Ducker, Voice, VoiceSettings};

pub type ErrorFn = Arc<dyn Fn(String) + Send + Sync>;

/// Latest reading of each `level` element, by element name.
type LevelStore = Arc<Mutex<HashMap<String, (Level, Instant)>>>;

/// What every pipeline's bus handler reports to.
#[derive(Clone)]
struct Hooks {
    on_error: ErrorFn,
    levels: LevelStore,
    /// Chooses the microphone channel(s) from the input levels.
    picker: Arc<Mutex<Picker>>,
    /// I am speaking (mic not muted): the partner is ducked on my speakers.
    speaking: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
}

/// A reading older than this means the source stopped (e.g. peer left).
const LEVEL_STALE: Duration = Duration::from_millis(500);

/// Per-channel values of one field (`rms`, `peak`) of a `level` message.
fn level_channels(s: &gst::StructureRef, field: &str) -> Option<Vec<f64>> {
    let array = s.value(field).ok()?.get::<glib::ValueArray>().ok()?;
    Some(array.iter().filter_map(|v| v.get::<f64>().ok()).collect())
}

fn parse_level(s: &gst::StructureRef) -> Option<Level> {
    Some(Level::from_channels(
        &level_channels(s, "rms")?,
        &level_channels(s, "peak")?,
    ))
}

/// Rewrites every stereo S16LE buffer leaving `pad`: the channel(s) chosen by
/// `picker` mixed into one voice, noise-gated, written to both channels.
/// (Done here because `audioconvert`'s `mix-matrix` ignores changes while
/// playing.)
/// Picks the mic channel(s) and cleans up the voice ([`Voice`]); also tells
/// the ducker whether I am speaking.
fn install_mic_processing(pad: &gst::Pad, hooks: &Hooks, voice: Voice) {
    let (picker, speaking) = (hooks.picker.clone(), hooks.speaking.clone());
    let voice = Mutex::new(voice);
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data
            && let Ok(mut map) = buffer.make_mut().map_writable()
        {
            let stereo: Vec<i16> = map
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b))
                .collect();
            let gains = picker.lock().unwrap().current().gains();
            let mut samples = pipelines::mix_to_mono(&stereo, gains);
            let mut v = voice.lock().unwrap();
            v.process(&mut samples);
            speaking.store(v.is_speaking(), Ordering::Relaxed);
            drop(v);
            for (frame, x) in map.as_chunks_mut::<4>().0.iter_mut().zip(samples) {
                let [lo, hi] = x.to_le_bytes();
                *frame = [lo, hi, lo, hi];
            }
        }
        gst::PadProbeReturn::Ok
    });
}

/// Lowers the partner's voice (mono S16) on its way to the speakers while
/// I speak.
fn install_duck(pad: &gst::Pad, hooks: &Hooks, ducker: Ducker) {
    let (speaking, muted) = (hooks.speaking.clone(), hooks.muted.clone());
    let ducker = Mutex::new(ducker);
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data
            && let Ok(mut map) = buffer.make_mut().map_writable()
        {
            let i_speak = speaking.load(Ordering::Relaxed) && !muted.load(Ordering::Relaxed);
            let mut samples: Vec<i16> = map
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b))
                .collect();
            ducker.lock().unwrap().process(&mut samples, i_speak);
            for (b, x) in map.as_chunks_mut::<2>().0.iter_mut().zip(samples) {
                *b = x.to_le_bytes();
            }
        }
        gst::PadProbeReturn::Ok
    });
}

/// Diagnostics on stderr when `TWOFCAST_DEBUG` is set (with a timestamp).
fn debug(msg: &str) {
    crate::log::write(msg);
}

/// Logs (debug) when the first buffer passes `pad`.
fn log_first_buffer(pad: &gst::Pad, what: &'static str) {
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
        debug(what);
        gst::PadProbeReturn::Remove
    });
}

/// Keeps `requests` up to date for a receive pipeline: a keyframe is wanted
/// from the start and whenever the depayloader notices lost packets (it then
/// asks upstream for a keyframe, which never reaches the sender by itself);
/// it is no longer wanted once one passes the parser.
///
/// `ready` is set once a picture decoded from a keyframe on leaves the
/// receive pipeline.
fn watch_keyframes(
    pipe: &Pipe,
    requests: &Arc<Mutex<keyframe::Requests>>,
    ready: &Arc<AtomicBool>,
) {
    requests.lock().unwrap().want();
    let keyframe_seen = Arc::new(AtomicBool::new(false));
    if let Some(depay) = pipe.by_name(pipelines::VIDEO_DEPAY) {
        if depay.find_property("request-keyframe").is_some() {
            depay.set_property("request-keyframe", true);
        }
        if let Some(pad) = depay.static_pad("sink") {
            let r = requests.clone();
            pad.add_probe(gst::PadProbeType::EVENT_UPSTREAM, move |_, info| {
                if let Some(gst::PadProbeData::Event(ev)) = &info.data
                    && gst_video::UpstreamForceKeyUnitEvent::parse(ev).is_ok()
                {
                    let mut r = r.lock().unwrap();
                    if !r.wanted() {
                        debug("receive-screen: packets lost, need a keyframe");
                    }
                    r.want();
                }
                gst::PadProbeReturn::Ok
            });
        }
    }
    if let Some(pad) = pipe
        .by_name(pipelines::VIDEO_PARSE)
        .and_then(|e| e.static_pad("src"))
    {
        let r = requests.clone();
        let seen = keyframe_seen.clone();
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let Some(gst::PadProbeData::Buffer(b)) = &info.data
                && !b.flags().contains(gst::BufferFlags::DELTA_UNIT)
            {
                seen.store(true, Ordering::Relaxed);
                let mut r = r.lock().unwrap();
                if r.wanted() {
                    debug("receive-screen: keyframe arrived");
                }
                r.got();
            }
            gst::PadProbeReturn::Ok
        });
    }
    let sink = pipe
        .get()
        .iterate_elements()
        .into_iter()
        .flatten()
        .find(|e| e.factory().is_some_and(|f| f.name() == "intervideosink"));
    if let Some(pad) = sink.and_then(|e| e.static_pad("sink")) {
        let ready = ready.clone();
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            if keyframe_seen.load(Ordering::Relaxed) {
                ready.store(true, Ordering::Relaxed);
                return gst::PadProbeReturn::Remove;
            }
            gst::PadProbeReturn::Ok
        });
    }
}

/// Logs (debug) every 5 s how far the composed picture is behind the clock.
fn log_lag(pad: &gst::Pad, pipeline: gst::Pipeline) {
    let seen = std::sync::atomic::AtomicU32::new(0);
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if seen
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .is_multiple_of(150)
            && let Some(gst::PadProbeData::Buffer(b)) = &info.data
            && let (Some(pts), Some(clock), Some(base)) =
                (b.pts(), pipeline.clock(), pipeline.base_time())
        {
            let running = clock.time().saturating_sub(base);
            debug(&format!(
                "program lag: {} ms",
                running.saturating_sub(pts).mseconds()
            ));
        }
        gst::PadProbeReturn::Ok
    });
}

/// When an incoming stream started, last got a packet, last produced
/// decoded output.
#[derive(Clone, Copy)]
struct StreamTimes {
    started: Instant,
    packet: Option<Instant>,
    decoded: Option<Instant>,
}

type StreamStats = Arc<Mutex<HashMap<&'static str, StreamTimes>>>;

/// Watches a receive pipeline's two streams: packets on the UDP ports and
/// decoded output at the `inter*` sinks.  The first of each is logged, so a
/// log shows "arrives" vs. "arrives but cannot be decoded".
fn track_streams(
    pipeline: &gst::Pipeline,
    stats: &StreamStats,
    video: &'static str,
    audio: &'static str,
    video_port: u16,
) {
    {
        let mut st = stats.lock().unwrap();
        for name in [video, audio] {
            st.insert(
                name,
                StreamTimes {
                    started: Instant::now(),
                    packet: None,
                    decoded: None,
                },
            );
        }
    }
    for e in pipeline.iterate_elements().into_iter().flatten() {
        let Some(factory) = e.factory() else { continue };
        let (name, is_packet, pad) = match factory.name().as_str() {
            "udpsrc" => {
                let name = if e.property::<i32>("port") == video_port as i32 {
                    video
                } else {
                    audio
                };
                (name, true, e.static_pad("src"))
            }
            "intervideosink" => (video, false, e.static_pad("sink")),
            "interaudiosink" => (audio, false, e.static_pad("sink")),
            _ => continue,
        };
        let Some(pad) = pad else { continue };
        let stats = stats.clone();
        let first = std::sync::atomic::AtomicBool::new(true);
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            if first.swap(false, std::sync::atomic::Ordering::Relaxed) {
                debug(&format!(
                    "{name}: first {}",
                    if is_packet {
                        "packet arrived"
                    } else {
                        "decoded output"
                    }
                ));
            }
            if let Some(t) = stats.lock().unwrap().get_mut(name) {
                if is_packet {
                    t.packet = Some(Instant::now())
                } else {
                    t.decoded = Some(Instant::now())
                }
            }
            gst::PadProbeReturn::Ok
        });
    }
}

/// A running pipeline; stopped when dropped.
struct Pipe {
    pipeline: Option<gst::Pipeline>,
    /// Serializes pipeline-wide events: `recalculate_latency` (from a helper
    /// thread) and the end-of-stream when a recording stops deadlock if they
    /// run at the same time.
    control: Arc<Mutex<()>>,
}

impl Pipe {
    fn launch(name: &str, desc: &str, hooks: &Hooks) -> anyhow::Result<Pipe> {
        let pipeline = gst::parse::launch(desc)
            .with_context(|| format!("building {name} pipeline"))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("{name}: not a pipeline"))?;
        pipeline.set_property("name", name);
        // All pipelines share the monotonic system clock.  Otherwise a pipeline
        // with a pulsesink would follow the sound card clock, which stands
        // still while no audio arrives and freezes the video in that pipeline.
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));
        let Hooks {
            on_error,
            levels,
            picker,
            ..
        } = hooks.clone();
        let label = name.to_string();
        // Errors are forwarded from the bus' sync handler: works with or
        // without a GLib main loop (tests have none).
        let weak = pipeline.downgrade();
        let control = Arc::new(Mutex::new(()));
        let control_for_latency = control.clone();
        pipeline.bus().unwrap().set_sync_handler(move |_, msg| {
            match msg.view() {
                gst::MessageView::Error(e) => {
                    on_error(format!("{label}: {}", e.error()));
                    crate::log::write(&format!("ERROR {label}: {} ({:?})", e.error(), e.debug()));
                }
                // Sources such as cameras and jitterbuffers learn their latency
                // late; without recalculating, sinks drop every frame as "late".
                // Not allowed from a streaming thread, hence the extra thread.
                gst::MessageView::Latency(_) => {
                    if let Some(p) = weak.upgrade() {
                        let control = control_for_latency.clone();
                        std::thread::spawn(move || {
                            let _guard = control.lock().unwrap();
                            let _ = p.recalculate_latency();
                        });
                    }
                }
                gst::MessageView::Element(e) => {
                    // Input channels of the mic: maybe switch the channel pick.
                    if let (Some(st), Some(src)) = (e.structure(), msg.src())
                        && st.name() == "level"
                        && src.name() == pipelines::LEVEL_MIC_INPUT
                        && let Some(rms) = level_channels(st, "rms")
                        && let Some(pick) = picker.lock().unwrap().feed(&rms)
                    {
                        eprintln!("[2fcast] microphone: using {pick:?} input channel(s)");
                    }
                    if let Some(level) = e
                        .structure()
                        .filter(|s| s.name() == "level")
                        .and_then(parse_level)
                        && let Some(src) = msg.src()
                    {
                        levels
                            .lock()
                            .unwrap()
                            .insert(src.name().to_string(), (level, Instant::now()));
                    }
                }
                _ => {}
            }
            // Keep only what `stop_recording` waits for; nobody else reads the
            // bus, so anything else would just pile up during a long session.
            match msg.type_() {
                gst::MessageType::Eos | gst::MessageType::Error => gst::BusSyncReply::Pass,
                _ => gst::BusSyncReply::Drop,
            }
        });
        Ok(Pipe {
            pipeline: Some(pipeline),
            control,
        })
    }

    fn play(self) -> anyhow::Result<Pipe> {
        self.get()
            .set_state(gst::State::Playing)
            .map_err(|_| anyhow!("{} failed to start", self.get().name()))?;
        Ok(self)
    }

    fn get(&self) -> &gst::Pipeline {
        self.pipeline.as_ref().unwrap()
    }

    fn by_name(&self, name: &str) -> Option<gst::Element> {
        self.get().by_name(name)
    }

    fn into_pipeline(mut self) -> (gst::Pipeline, Arc<Mutex<()>>) {
        (self.pipeline.take().unwrap(), self.control.clone())
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        if let Some(p) = self.pipeline.take() {
            let _ = p.set_state(gst::State::Null);
        }
    }
}

/// A LAN sender: pipeline, multiudpsink name, valve name, peer port.
type SenderTarget<'a> = (&'a Option<Pipe>, &'a str, &'a str, fn(&Ports) -> u16);

/// One preview frame (BGRA).
#[derive(Clone)]
pub struct Frame {
    pub data: glib::Bytes,
    pub stride: usize,
    pub width: i32,
    pub height: i32,
    pub serial: u64,
}

#[derive(Default)]
pub struct OverlayState {
    pub annotations: Annotations,
    pub screen: Option<Rect>,
}

pub struct MediaConfig {
    pub channels: Channels,
    /// Ports this machine receives on.
    pub ports: Ports,
    pub camera: Option<CamSource>,
    pub mic: Option<MicSource>,
    /// Which input channel(s) make up the voice.
    pub mic_channel: MicMode,
    /// Noise gate threshold (dBFS); `None` = no gate.
    pub noise_gate_db: Option<f64>,
    /// Mic high-pass cutoff (Hz); `None` = off.
    pub high_pass_hz: Option<f64>,
    /// Mic compressor and limiter.
    pub compressor: bool,
    /// Partner's voice this much quieter on the speakers while I speak.
    pub duck_db: Option<f64>,
    /// Play the peer's sound on the speakers.
    pub playback: bool,
    /// H.264 decoder for the peer's video; `None` picks the best installed.
    pub h264_decoder: Option<&'static str>,
}

pub struct Media {
    cfg: MediaConfig,
    hooks: Hooks,
    program: Pipe,
    camera: Option<Pipe>,
    mic: Option<Pipe>,
    screen: Option<Pipe>,
    receive: Option<Pipe>,
    receive_screen: Option<Pipe>,
    recorder: Option<(Pipe, PathBuf)>,
    peer: Option<(IpAddr, Ports)>,
    overlay: Arc<Mutex<OverlayState>>,
    frame: Arc<Mutex<Option<Frame>>>,
    mic_muted: bool,
    /// When the receiver started, and when the last packet from the peer
    /// arrived (camera or voice).
    peer_packets: Arc<Mutex<(Instant, Option<Instant>)>>,
    /// Per incoming stream: arriving and decoding (see `track_streams`).
    streams: StreamStats,
    /// Whether to ask the presenter for a keyframe of their screen.
    screen_keyframes: Arc<Mutex<keyframe::Requests>>,
    /// A complete picture of the partner's screen arrived (decoded from a
    /// keyframe on); until then their screen is not shown, so the program
    /// never shows a stale or half-decoded picture when they start sharing.
    screen_ready: Arc<AtomicBool>,
    /// The pads last asked for, and whether they were applied with the
    /// partner's screen ready.
    pads: Mutex<Option<(Pads, bool)>>,
    /// Picture shown in the shared space while nobody shares.
    idle: Option<Idle>,
}

/// The idle picture: the original, and the size it was last sent at.
struct Idle {
    pipe: Pipe,
    pixbuf: gtk::gdk_pixbuf::Pixbuf,
    sent: std::cell::Cell<Option<(u32, u32)>>,
}

impl Idle {
    fn size(&self) -> (u32, u32) {
        (self.pixbuf.width() as u32, self.pixbuf.height() as u32)
    }

    /// Scales the picture once to fit `tile` (canvas pixels) and sends a
    /// frame of exactly that size, so the compositor shows it 1:1.
    fn send(&self, tile: (u32, u32)) {
        if self.sent.get() == Some(tile) || tile.0 == 0 || tile.1 == 0 {
            return;
        }
        let (w, h) = idle_image::fit(self.size(), tile);
        let scaled = if (w, h) == self.size() {
            Some(self.pixbuf.clone())
        } else {
            self.pixbuf
                .scale_simple(w as i32, h as i32, gtk::gdk_pixbuf::InterpType::Hyper)
        };
        let Some(scaled) = scaled else { return };
        let frame = idle_image::compose_bgrx(
            &scaled.read_pixel_bytes(),
            (w, h),
            scaled.rowstride() as usize,
            scaled.n_channels() as usize,
            tile,
        );
        let Some(src) = self
            .pipe
            .by_name("src")
            .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
        else {
            return;
        };
        let Ok(info) = gst_video::VideoInfo::builder(gst_video::VideoFormat::Bgrx, tile.0, tile.1)
            .fps(gst::Fraction::new(pipelines::FPS as i32, 1))
            .build()
        else {
            return;
        };
        src.set_caps(info.to_caps().ok().as_ref());
        if src.push_buffer(gst::Buffer::from_mut_slice(frame)).is_ok() {
            debug(&format!("idle picture sent at {}x{}", tile.0, tile.1));
            self.sent.set(Some(tile));
        }
    }
}

impl Media {
    pub fn new(cfg: MediaConfig, on_error: ErrorFn) -> anyhow::Result<Media> {
        let hooks = Hooks {
            on_error,
            levels: Arc::default(),
            picker: Arc::new(Mutex::new(Picker::new(cfg.mic_channel))),
            speaking: Arc::default(),
            muted: Arc::default(),
        };
        let program = Pipe::launch("program", &pipelines::program(&cfg.channels), &hooks)?;
        let overlay = Arc::new(Mutex::new(OverlayState::default()));
        let state = overlay.clone();
        program
            .by_name("overlay")
            .unwrap()
            .connect("draw", false, move |args| {
                if let Ok(cr) = args[1].get::<gtk::cairo::Context>() {
                    let s = state.lock().unwrap();
                    if let Some(rect) = s.screen {
                        s.annotations.draw(&cr, rect);
                    }
                }
                None
            });
        let frame = Arc::new(Mutex::new(None::<Frame>));
        let slot = frame.clone();
        let sink = program
            .by_name("preview")
            .unwrap()
            .downcast::<gst_app::AppSink>()
            .unwrap();
        let mut serial = 0;
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let (Some(buffer), Some(caps)) = (sample.buffer(), sample.caps()) else {
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    let Ok(info) = gst_video::VideoInfo::from_caps(caps) else {
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    let Ok(map) = buffer.map_readable() else {
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    serial += 1;
                    *slot.lock().unwrap() = Some(Frame {
                        data: glib::Bytes::from(map.as_slice()),
                        stride: info.stride()[0] as usize,
                        width: info.width() as i32,
                        height: info.height() as i32,
                        serial,
                    });
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        let program = program.play()?;
        if let Some(pad) = program.by_name("tprog").and_then(|t| t.static_pad("sink")) {
            log_lag(&pad, program.get().clone());
        }

        let mut media = Media {
            cfg,
            hooks,
            program,
            camera: None,
            mic: None,
            screen: None,
            receive: None,
            receive_screen: None,
            recorder: None,
            peer: None,
            overlay,
            frame,
            mic_muted: false,
            peer_packets: Arc::new(Mutex::new((Instant::now(), None))),
            streams: StreamStats::default(),
            screen_keyframes: Arc::default(),
            screen_ready: Arc::default(),
            pads: Mutex::new(None),
            idle: None,
        };
        // A missing/busy camera or microphone must not prevent talking.
        if let Some(src) = media.cfg.camera.clone() {
            match Pipe::launch(
                "camera",
                &pipelines::camera(&media.cfg.channels, &src),
                &media.hooks,
            )
            .and_then(Pipe::play)
            {
                Ok(p) => media.camera = Some(p),
                Err(e) => (media.hooks.on_error)(format!("camera: {e:#}")),
            }
        }
        if let Some(src) = media.cfg.mic.clone() {
            match Pipe::launch(
                "microphone",
                &pipelines::microphone(&media.cfg.channels, &src),
                &media.hooks,
            )
            .and_then(|p| {
                let pad = p
                    .by_name(pipelines::MIC_PROCESS)
                    .and_then(|e| e.static_pad("src"))
                    .ok_or_else(|| anyhow!("microphone: no {} element", pipelines::MIC_PROCESS))?;
                let voice = Voice::new(VoiceSettings {
                    high_pass_hz: media.cfg.high_pass_hz,
                    gate_db: media.cfg.noise_gate_db,
                    compressor: media.cfg.compressor,
                    rate: 48_000,
                });
                install_mic_processing(&pad, &media.hooks, voice);
                p.play()
            }) {
                Ok(p) => media.mic = Some(p),
                Err(e) => (media.hooks.on_error)(format!("microphone: {e:#}")),
            }
        }
        Ok(media)
    }

    pub fn latest_frame(&self) -> Option<Frame> {
        self.frame.lock().unwrap().clone()
    }

    pub fn has_camera(&self) -> bool {
        self.camera.is_some()
    }

    /// Current level of my voice (after mute) and of the partner's voice;
    /// `None` while that voice is not running.
    pub fn voice_levels(&self) -> (Option<Level>, Option<Level>) {
        let levels = self.hooks.levels.lock().unwrap();
        let get = |name: &str| {
            levels
                .get(name)
                .filter(|(_, at)| at.elapsed() < LEVEL_STALE)
                .map(|(l, _)| *l)
        };
        (get(pipelines::LEVEL_MIC), get(pipelines::LEVEL_PEER))
    }

    /// How long nothing (no camera, no voice packet) has arrived from the
    /// peer; `None` while not connected.  Packets that never come usually mean
    /// a firewall drops incoming UDP.
    pub fn peer_silent_for(&self) -> Option<Duration> {
        self.receive.as_ref()?;
        let (started, last) = *self.peer_packets.lock().unwrap();
        Some(last.unwrap_or(started).elapsed())
    }

    /// Whether the peer's camera/voice arrive: `Some(true)` once packets come
    /// in, `Some(false)` after 5 s without any, `None` before we can tell.
    pub fn peer_media_arrives(&self) -> Option<bool> {
        self.receive.as_ref()?;
        let (started, last) = *self.peer_packets.lock().unwrap();
        let limit = Duration::from_secs(5);
        match last {
            Some(t) => Some(t.elapsed() < limit),
            None if started.elapsed() >= limit => Some(false),
            None => None,
        }
    }

    fn forget_streams(&self, names: &[&'static str]) {
        let mut st = self.streams.lock().unwrap();
        for n in names {
            st.remove(n);
        }
    }

    /// Incoming streams whose packets arrive but cannot be decoded here.
    pub fn undecodable_streams(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self
            .streams
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, t)| {
                twofcast_core::health::arrives_but_not_decoded(
                    t.started.elapsed(),
                    t.packet.map(|p| p.elapsed()),
                    t.decoded.map(|d| d.elapsed()),
                )
            })
            .map(|(name, _)| *name)
            .collect();
        v.sort();
        v
    }

    /// Which microphone channel(s) are in use now.
    pub fn mic_pick(&self) -> Pick {
        self.hooks.picker.lock().unwrap().current()
    }

    pub fn noise_gate_db(&self) -> Option<f64> {
        self.cfg.noise_gate_db.filter(|_| self.mic.is_some())
    }

    pub fn has_mic(&self) -> bool {
        self.mic.is_some()
    }

    /// Points all LAN senders at the peer (or closes them) and starts/stops
    /// receiving the peer's camera and voice.
    pub fn set_peer(&mut self, peer: Option<(IpAddr, Ports)>) {
        if peer == self.peer {
            return;
        }
        self.peer = peer;
        debug(&format!("media peer: {peer:?}"));
        self.update_senders();
        self.receive = None;
        self.forget_streams(&["camera", "voice"]);
        if peer.is_some() {
            let desc = pipelines::receive(
                &self.cfg.channels,
                &self.cfg.ports,
                self.cfg.playback,
                self.cfg.h264_decoder.unwrap_or_else(h264_decoder),
            );
            *self.peer_packets.lock().unwrap() = (Instant::now(), None);
            match Pipe::launch("receive", &desc, &self.hooks).and_then(Pipe::play) {
                Ok(p) => {
                    debug("receive: started");
                    if let Some(db) = self.cfg.duck_db
                        && let Some(pad) =
                            p.by_name(pipelines::DUCK).and_then(|e| e.static_pad("src"))
                    {
                        install_duck(&pad, &self.hooks, Ducker::new(db, 48_000));
                    }
                    track_streams(
                        p.get(),
                        &self.streams,
                        "camera",
                        "voice",
                        self.cfg.ports.cam,
                    );
                    for src in p.get().iterate_sources().into_iter().flatten() {
                        if let Some(pad) = src.static_pad("src") {
                            let packets = self.peer_packets.clone();
                            pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                                packets.lock().unwrap().1 = Some(Instant::now());
                                gst::PadProbeReturn::Ok
                            });
                        }
                    }
                    self.receive = Some(p)
                }
                Err(e) => (self.hooks.on_error)(format!("{e:#}")),
            }
        } else {
            self.receive_screen = None;
            self.forget_streams(&["shared screen", "shared sound"]);
        }
    }

    fn update_senders(&self) {
        let targets: [SenderTarget; 4] = [
            (&self.camera, "net_cam", "valve_cam", |p| p.cam),
            (&self.mic, "net_mic", "valve_mic", |p| p.mic),
            (&self.screen, "net_screen", "valve_screen", |p| p.screen),
            (&self.screen, "net_share", "valve_share", |p| p.share),
        ];
        for (pipe, sink, valve, port) in targets {
            let Some(pipe) = pipe else { continue };
            let (Some(sink), Some(valve)) = (pipe.by_name(sink), pipe.by_name(valve)) else {
                continue;
            };
            let clients = self
                .peer
                .map(|(ip, ports)| format!("{ip}:{}", port(&ports)))
                .unwrap_or_default();
            sink.set_property("clients", clients);
            valve.set_property("drop", self.peer.is_none());
        }
    }

    pub fn start_screen(
        &mut self,
        src: &ScreenSource,
        audio_source: Option<&str>,
    ) -> anyhow::Result<()> {
        self.screen = None;
        let desc = pipelines::screen(&self.cfg.channels, src, audio_source);
        let pipe = Pipe::launch("screen", &desc, &self.hooks)?;
        debug("screen: started");
        if let Some(pad) = pipe
            .by_name("net_screen")
            .and_then(|e| e.static_pad("sink"))
        {
            log_first_buffer(&pad, "screen: first packet to the network");
        }
        self.screen = Some(pipe.play()?);
        self.update_senders();
        Ok(())
    }

    pub fn stop_screen(&mut self) {
        if self.screen.is_some() {
            debug("screen: stopping");
            self.screen = None;
            debug("screen: stopped");
        }
    }

    pub fn is_sharing(&self) -> bool {
        self.screen.is_some()
    }

    /// Receive the peer's shared space while the peer presents.
    pub fn set_receive_screen(&mut self, on: bool) {
        if on == self.receive_screen.is_some() {
            return;
        }
        self.receive_screen = None;
        self.forget_streams(&["shared screen", "shared sound"]);
        *self.screen_keyframes.lock().unwrap() = keyframe::Requests::default();
        self.screen_ready.store(false, Ordering::Relaxed);
        self.refresh_pads();
        if on {
            let desc = pipelines::receive_screen(
                &self.cfg.channels,
                &self.cfg.ports,
                self.cfg.playback,
                self.cfg.h264_decoder.unwrap_or_else(h264_decoder),
            );
            match Pipe::launch("receive-screen", &desc, &self.hooks).and_then(Pipe::play) {
                Ok(p) => {
                    debug("receive-screen: started");
                    track_streams(
                        p.get(),
                        &self.streams,
                        "shared screen",
                        "shared sound",
                        self.cfg.ports.screen,
                    );
                    if let Some(pad) = p
                        .by_name(pipelines::VIDEO_DEPAY)
                        .and_then(|e| e.static_pad("sink"))
                    {
                        log_first_buffer(&pad, "receive-screen: first RTP packet");
                    }
                    watch_keyframes(&p, &self.screen_keyframes, &self.screen_ready);
                    if let Some(pad) = p
                        .get()
                        .iterate_elements()
                        .into_iter()
                        .flatten()
                        .find(|e| e.factory().is_some_and(|f| f.name() == "intervideosink"))
                        .and_then(|e| e.static_pad("sink"))
                    {
                        log_first_buffer(&pad, "receive-screen: first decoded frame");
                    }
                    self.receive_screen = Some(p)
                }
                Err(e) => (self.hooks.on_error)(format!("{e:#}")),
            }
        }
    }

    /// Whether to ask the presenter for a keyframe now (we receive their
    /// screen and have had none since starting or losing packets).  Rate
    /// limited: call it often.
    pub fn screen_keyframe_due(&self) -> bool {
        self.receive_screen.is_some() && self.screen_keyframes.lock().unwrap().due(Instant::now())
    }

    /// The partner asked for a keyframe of the screen we share.
    pub fn force_screen_keyframe(&self) {
        let Some(pad) = self
            .screen
            .as_ref()
            .and_then(|p| p.by_name(pipelines::SCREEN_ENCODER))
            .and_then(|e| e.static_pad("src"))
        else {
            return;
        };
        debug("screen: keyframe requested by the partner");
        pad.send_event(
            gst_video::UpstreamForceKeyUnitEvent::builder()
                .all_headers(true)
                .build(),
        );
    }

    /// Sets the compositor inputs; the partner's screen stays hidden until a
    /// complete picture of it arrived (see [`Media::refresh_pads`]).
    pub fn apply_pads(&self, pads: &Pads) {
        let ready = self.screen_ready.load(Ordering::Relaxed);
        *self.pads.lock().unwrap() = Some((*pads, ready));
        let mut shown = pads.show_remote_screen(ready);
        match &self.idle {
            Some(idle) if shown.idle.alpha > 0.0 => {
                idle.send((shown.idle.w.max(0) as u32, shown.idle.h.max(0) as u32));
            }
            _ => shown.idle.alpha = 0.0,
        }
        self.set_pads(&shown);
    }

    fn reapply_pads(&self) {
        let pads = self.pads.lock().unwrap().map(|(p, _)| p);
        if let Some(p) = pads {
            self.apply_pads(&p);
        }
    }

    /// Shows the picture at `path` in the shared space while nobody shares
    /// (`None`: black).  Returns its size in pixels.
    pub fn set_idle_image(&mut self, path: Option<&Path>) -> anyhow::Result<Option<(u32, u32)>> {
        let Some(path) = path else {
            self.idle = None;
            self.reapply_pads();
            return Ok(None);
        };
        // A picture that cannot be read keeps the current one.
        let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_file(path)
            .with_context(|| format!("cannot read the picture {}", path.display()))?;
        // Photos are often stored sideways with an orientation tag.
        let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
        // The old feeder goes first: stopping it clears the channel.
        self.idle = None;
        let pipe = Pipe::launch(
            "idle",
            &pipelines::idle_picture(&self.cfg.channels),
            &self.hooks,
        )
        .and_then(Pipe::play);
        let idle = match pipe {
            Ok(pipe) => Idle {
                pipe,
                pixbuf,
                sent: Default::default(),
            },
            Err(e) => {
                self.reapply_pads();
                return Err(e);
            }
        };
        let size = idle.size();
        debug(&format!(
            "idle picture {}: {}x{}",
            path.display(),
            size.0,
            size.1
        ));
        self.idle = Some(idle);
        self.reapply_pads();
        Ok(Some(size))
    }

    /// Size of the idle picture, if one is set.
    pub fn idle_image_size(&self) -> Option<(u32, u32)> {
        self.idle.as_ref().map(Idle::size)
    }

    /// Shows the partner's screen once its first complete picture arrived
    /// (or hides it again when it stopped).  Cheap: call it often.
    pub fn refresh_pads(&self) {
        let ready = self.screen_ready.load(Ordering::Relaxed);
        let pads = match *self.pads.lock().unwrap() {
            Some((pads, applied)) if applied != ready => pads,
            _ => return,
        };
        if ready {
            debug("receive-screen: complete picture arrived, showing it");
        }
        self.apply_pads(&pads);
    }

    fn set_pads(&self, pads: &Pads) {
        thread_local!(static LAST: std::cell::Cell<(f64, f64)> = const { std::cell::Cell::new((-1.0, -1.0)) });
        let alphas = (pads.screen_local.alpha, pads.screen_remote.alpha);
        if LAST.with(|l| l.replace(alphas)) != alphas {
            debug(&format!(
                "pads: screen local alpha {} / remote alpha {}",
                alphas.0, alphas.1
            ));
        }
        let all = [
            pads.screen_local,
            pads.screen_remote,
            pads.cam_local,
            pads.cam_remote,
        ];
        for (input, p) in pipelines::PROGRAM_INPUTS.iter().zip(all) {
            let Some(queue) = self.program.by_name(&format!("q_{input}")) else {
                continue;
            };
            let Some(pad) = queue.static_pad("src").and_then(|p| p.peer()) else {
                continue;
            };
            set_pad(&pad, &p);
        }
        if let Some(pad) = self
            .program
            .by_name(&format!("q_{}", pipelines::IDLE_INPUT))
            .and_then(|q| q.static_pad("src"))
            .and_then(|p| p.peer())
        {
            set_pad(&pad, &pads.idle);
        }
    }

    pub fn set_overlay(&self, annotations: &Annotations, screen: Option<Rect>) {
        let mut s = self.overlay.lock().unwrap();
        s.annotations = annotations.clone();
        s.screen = screen;
    }

    pub fn is_mic_muted(&self) -> bool {
        self.mic_muted
    }

    pub fn set_mic_muted(&mut self, muted: bool) {
        self.mic_muted = muted;
        self.hooks.muted.store(muted, Ordering::Relaxed);
        if let Some(v) = self.mic.as_ref().and_then(|m| m.by_name("micvol")) {
            v.set_property("mute", muted);
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recorder.is_some()
    }

    pub fn start_recording(
        &mut self,
        path: &Path,
        bitrate_kbps: u32,
        preset: &str,
        names: &TrackNames,
    ) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let pipe = Pipe::launch(
            "recorder",
            &pipelines::record(&self.cfg.channels, bitrate_kbps, preset),
            &self.hooks,
        )?;
        pipe.by_name("file")
            .unwrap()
            .set_property("location", path.to_string_lossy().as_ref());
        for (encoder, title) in names.titles() {
            if let Some(pad) = pipe.by_name(encoder).and_then(|e| e.static_pad("src")) {
                set_track_title(&pad, title);
            }
        }
        self.recorder = Some((pipe.play()?, path.to_path_buf()));
        Ok(())
    }

    /// Finishes the file (EOS) in a background thread.  The receiver yields
    /// the path once the file is complete.
    pub fn stop_recording(&mut self) -> Option<async_channel::Receiver<anyhow::Result<PathBuf>>> {
        let (pipe, path) = self.recorder.take()?;
        let (pipeline, control) = pipe.into_pipeline();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            {
                let _guard = control.lock().unwrap();
                pipeline.send_event(gst::event::Eos::new());
            }
            let bus = pipeline.bus().unwrap();
            let msg = bus.timed_pop_filtered(
                gst::ClockTime::from_seconds(15),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            );
            let _ = pipeline.set_state(gst::State::Null);
            let result = match msg.as_ref().map(|m| m.view()) {
                Some(gst::MessageView::Eos(_)) => Ok(path),
                Some(gst::MessageView::Error(e)) => Err(anyhow!("recording failed: {}", e.error())),
                _ => Err(anyhow!(
                    "recording did not finish in time; {} may be incomplete",
                    path.display()
                )),
            };
            let _ = tx.send_blocking(result);
        });
        Some(rx)
    }
}

/// Adds `title` to the stream tags leaving an encoder, which matroskamux
/// writes as the track name.  Done on the tag event itself rather than with
/// `taginject`, whose tag-string parsing mangles spaces and quotes.
fn set_track_title(pad: &gst::Pad, title: String) {
    pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
        if let Some(gst::PadProbeData::Event(ev)) = &mut info.data
            && let gst::EventView::Tag(t) = ev.view()
            && t.tag().scope() == gst::TagScope::Stream
        {
            let mut tags = t.tag_owned();
            tags.make_mut()
                .add::<gst::tags::Title>(&title.as_str(), gst::TagMergeMode::Replace);
            *ev = gst::event::Tag::new(tags);
        }
        gst::PadProbeReturn::Ok
    });
}

fn set_pad(pad: &gst::Pad, p: &PadParams) {
    pad.set_property("xpos", p.x);
    pad.set_property("ypos", p.y);
    pad.set_property("width", p.w);
    pad.set_property("height", p.h);
    pad.set_property("alpha", p.alpha);
    pad.set_property("zorder", p.zorder);
}

/// The best H.264 decoder this installation has.
pub fn h264_decoder() -> &'static str {
    // TWOFCAST_H264_DECODER=openh264dec tests what the bundled package uses.
    if let Ok(forced) = std::env::var("TWOFCAST_H264_DECODER")
        && let Some(d) = pipelines::H264_DECODERS.iter().find(|d| **d == forced)
    {
        return d;
    }
    pipelines::H264_DECODERS
        .into_iter()
        .find(|d| gst::ElementFactory::find(d).is_some())
        .unwrap_or(pipelines::H264_DECODERS[0])
}

/// Resolves the `camera` setting ("auto", a device path, "test", "none").
pub fn camera_source(setting: &str) -> Option<CamSource> {
    match setting {
        "none" => None,
        "test" => Some(CamSource::Test(0x204060)),
        "auto" => {
            let (path, modes) = video_devices().into_iter().next()?;
            Some(CamSource::Device {
                path,
                mode: camera::choose(&modes),
            })
        }
        path => {
            let modes = video_devices()
                .into_iter()
                .find(|(p, _)| p == path)
                .map(|(_, m)| m)
                .unwrap_or_default();
            Some(CamSource::Device {
                path: path.to_string(),
                mode: camera::choose(&modes),
            })
        }
    }
}

pub fn mic_source(setting: &str) -> Option<MicSource> {
    match setting {
        "none" => None,
        "default" | "" => Some(MicSource::Default),
        "test" => Some(MicSource::Test),
        d => Some(MicSource::Device(d.to_string())),
    }
}

/// V4L2 cameras: (device path, supported modes).
pub fn video_devices() -> Vec<(String, Vec<CamMode>)> {
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Video/Source"), None);
    if monitor.start().is_err() {
        return vec![];
    }
    let mut out = vec![];
    for dev in monitor.devices() {
        let Some(props) = dev.properties() else {
            continue;
        };
        let path = props
            .get::<String>("api.v4l2.path")
            .or_else(|_| props.get::<String>("device.path"));
        let api_ok = props
            .get::<String>("device.api")
            .map(|a| a == "v4l2")
            .unwrap_or(true);
        let (Ok(path), true) = (path, api_ok) else {
            continue;
        };
        let modes = dev.caps().map(|c| modes_from_caps(&c)).unwrap_or_default();
        if !modes.is_empty() {
            out.push((path, modes));
        }
    }
    monitor.stop();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

pub fn modes_from_caps(caps: &gst::CapsRef) -> Vec<CamMode> {
    let mut out = vec![];
    for s in caps.iter() {
        let format = match s.name().as_str() {
            "image/jpeg" => "jpeg".to_string(),
            "video/x-raw" => match s.get::<String>("format") {
                Ok(f) => f,
                Err(_) => continue,
            },
            _ => continue,
        };
        let (Ok(width), Ok(height)) = (s.get::<i32>("width"), s.get::<i32>("height")) else {
            continue;
        };
        let rates: Vec<gst::Fraction> = if let Ok(f) = s.get::<gst::Fraction>("framerate") {
            vec![f]
        } else if let Ok(list) = s.get::<gst::List>("framerate") {
            list.iter()
                .filter_map(|v| v.get::<gst::Fraction>().ok())
                .collect()
        } else {
            vec![]
        };
        for f in rates {
            out.push(CamMode {
                format: format.clone(),
                width,
                height,
                fps_n: f.numer(),
                fps_d: f.denom(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_caps() {
        gst::init().unwrap();
        let caps: gst::Caps = "image/jpeg, width=(int)1280, height=(int)720, framerate=(fraction){ 30/1, 15/1 }; \
                               video/x-raw, format=(string)YUY2, width=(int)640, height=(int)480, framerate=(fraction)30/1; \
                               video/x-raw, format=(string)YUY2, width=(int)[ 1, 100 ], height=(int)100, framerate=(fraction)30/1"
            .parse()
            .unwrap();
        let modes = modes_from_caps(&caps);
        assert_eq!(modes.len(), 3, "ranges are skipped: {modes:?}");
        assert_eq!(
            modes[0],
            CamMode {
                format: "jpeg".into(),
                width: 1280,
                height: 720,
                fps_n: 30,
                fps_d: 1
            }
        );
        assert_eq!(modes[2].format, "YUY2");
    }

    #[test]
    fn source_settings() {
        assert_eq!(camera_source("none"), None);
        assert!(matches!(camera_source("test"), Some(CamSource::Test(_))));
        assert_eq!(mic_source("default"), Some(MicSource::Default));
        assert_eq!(
            mic_source("alsa_input.x"),
            Some(MicSource::Device("alsa_input.x".into()))
        );
        assert_eq!(mic_source("none"), None);
    }
}
