//! Owns the GStreamer pipelines of one machine (see
//! [`twofcast_core::pipelines`] for the overall graph).

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, anyhow};
use gst::prelude::*;
use gtk::glib;
use twofcast_core::annotations::Annotations;
use twofcast_core::camera::{self, CamMode};
use twofcast_core::layout::{PadParams, Pads, Rect};
use twofcast_core::pipelines::{self, CamSource, Channels, MicSource, Ports, ScreenSource};

pub type ErrorFn = Arc<dyn Fn(String) + Send + Sync>;

/// A running pipeline; stopped when dropped.
struct Pipe {
    pipeline: Option<gst::Pipeline>,
}

impl Pipe {
    fn launch(name: &str, desc: &str, on_error: &ErrorFn) -> anyhow::Result<Pipe> {
        let pipeline = gst::parse::launch(desc)
            .with_context(|| format!("building {name} pipeline"))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("{name}: not a pipeline"))?;
        pipeline.set_property("name", name);
        // All pipelines share the monotonic system clock.  Otherwise a pipeline
        // with a pulsesink would follow the sound card clock, which stands
        // still while no audio arrives and freezes the video in that pipeline.
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));
        let on_error = on_error.clone();
        let label = name.to_string();
        // Errors are forwarded from the bus' sync handler: works with or
        // without a GLib main loop (tests have none).
        let weak = pipeline.downgrade();
        pipeline.bus().unwrap().set_sync_handler(move |_, msg| {
            match msg.view() {
                gst::MessageView::Error(e) => {
                    on_error(format!("{label}: {}", e.error()));
                    eprintln!("[2fcast] {label}: {} ({:?})", e.error(), e.debug());
                }
                // Sources such as cameras and jitterbuffers learn their latency
                // late; without recalculating, sinks drop every frame as "late".
                // Not allowed from a streaming thread, hence the extra thread.
                gst::MessageView::Latency(_) => {
                    if let Some(p) = weak.upgrade() {
                        std::thread::spawn(move || {
                            let _ = p.recalculate_latency();
                        });
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

    fn into_pipeline(mut self) -> gst::Pipeline {
        self.pipeline.take().unwrap()
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
    /// Play the peer's sound on the speakers.
    pub playback: bool,
}

pub struct Media {
    cfg: MediaConfig,
    on_error: ErrorFn,
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
}

impl Media {
    pub fn new(cfg: MediaConfig, on_error: ErrorFn) -> anyhow::Result<Media> {
        let program = Pipe::launch("program", &pipelines::program(&cfg.channels), &on_error)?;
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

        let mut media = Media {
            cfg,
            on_error,
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
        };
        // A missing/busy camera or microphone must not prevent talking.
        if let Some(src) = media.cfg.camera.clone() {
            match Pipe::launch(
                "camera",
                &pipelines::camera(&media.cfg.channels, &src),
                &media.on_error,
            )
            .and_then(Pipe::play)
            {
                Ok(p) => media.camera = Some(p),
                Err(e) => (media.on_error)(format!("camera: {e:#}")),
            }
        }
        if let Some(src) = media.cfg.mic.clone() {
            match Pipe::launch(
                "microphone",
                &pipelines::microphone(&media.cfg.channels, &src),
                &media.on_error,
            )
            .and_then(Pipe::play)
            {
                Ok(p) => media.mic = Some(p),
                Err(e) => (media.on_error)(format!("microphone: {e:#}")),
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
        self.update_senders();
        self.receive = None;
        if peer.is_some() {
            let desc = pipelines::receive(&self.cfg.channels, &self.cfg.ports, self.cfg.playback);
            match Pipe::launch("receive", &desc, &self.on_error).and_then(Pipe::play) {
                Ok(p) => self.receive = Some(p),
                Err(e) => (self.on_error)(format!("{e:#}")),
            }
        } else {
            self.receive_screen = None;
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
        self.screen = Some(Pipe::launch("screen", &desc, &self.on_error)?.play()?);
        self.update_senders();
        Ok(())
    }

    pub fn stop_screen(&mut self) {
        self.screen = None;
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
        if on {
            let desc =
                pipelines::receive_screen(&self.cfg.channels, &self.cfg.ports, self.cfg.playback);
            match Pipe::launch("receive-screen", &desc, &self.on_error).and_then(Pipe::play) {
                Ok(p) => self.receive_screen = Some(p),
                Err(e) => (self.on_error)(format!("{e:#}")),
            }
        }
    }

    pub fn apply_pads(&self, pads: &Pads) {
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
    }

    pub fn set_overlay(&self, annotations: &Annotations, screen: Option<Rect>) {
        let mut s = self.overlay.lock().unwrap();
        s.annotations = annotations.clone();
        s.screen = screen;
    }

    pub fn set_mic_muted(&mut self, muted: bool) {
        self.mic_muted = muted;
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
    ) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let pipe = Pipe::launch(
            "recorder",
            &pipelines::record(&self.cfg.channels, bitrate_kbps, preset),
            &self.on_error,
        )?;
        pipe.by_name("file")
            .unwrap()
            .set_property("location", path.to_string_lossy().as_ref());
        self.recorder = Some((pipe.play()?, path.to_path_buf()));
        Ok(())
    }

    /// Finishes the file (EOS) in a background thread.  The receiver yields
    /// the path once the file is complete.
    pub fn stop_recording(&mut self) -> Option<async_channel::Receiver<anyhow::Result<PathBuf>>> {
        let (pipe, path) = self.recorder.take()?;
        let pipeline = pipe.into_pipeline();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            pipeline.send_event(gst::event::Eos::new());
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

fn set_pad(pad: &gst::Pad, p: &PadParams) {
    pad.set_property("xpos", p.x);
    pad.set_property("ypos", p.y);
    pad.set_property("width", p.w);
    pad.set_property("height", p.h);
    pad.set_property("alpha", p.alpha);
    pad.set_property("zorder", p.zorder);
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
