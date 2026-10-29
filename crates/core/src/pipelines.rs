//! GStreamer pipeline descriptions (gst-launch syntax).
//!
//! Each machine runs a few small pipelines connected through `inter*` elements:
//!
//! ```text
//!  camera ──► cam_local ─┐                      ┌─► preview (UI)
//!   └─(LAN)─► peer       │                      │
//!  mic ─────► mic_local  │   program:           ├─► program ─► record ─► .mkv
//!   └─(LAN)─► peer       ├─► compositor ─► overlay (annotations)
//!  screen ──► screen_local                       mic/share audio ─► record
//!   └─(LAN)─► peer       │
//!  receive ─► cam_remote, mic_remote (+ speakers)
//!  receive screen ─► screen_remote, share_remote (+ speakers)
//! ```
//!
//! The `inter*` sinks run with `sync=false`: they only hand data to another
//! pipeline, whose `inter*` source re-times it.  Sinks of live branches use
//! `async=false` so one idle branch (e.g. the speakers while the peer's mic is
//! silent) never holds the whole pipeline in preroll.
//!
//! The camera, mic and screen pipelines send to the LAN through a `valve`
//! (closed while nobody is connected) and a `multiudpsink`.

use crate::camera::CamMode;

pub const CAM_W: u32 = 960;
pub const CAM_H: u32 = 540;
pub const SCREEN_W: u32 = 1920;
pub const SCREEN_H: u32 = 1080;
pub const FPS: u32 = 30;
pub const PREVIEW_W: u32 = 1280;
pub const PREVIEW_H: u32 = 720;

pub const AUDIO_CAPS: &str = "audio/x-raw,format=S16LE,rate=48000,channels=2,layout=interleaved";
const VIDEO_PT: u32 = 96;
const AUDIO_PT: u32 = 97;

fn video_caps(w: u32, h: u32) -> String {
    format!("video/x-raw,format=I420,width={w},height={h},pixel-aspect-ratio=1/1,framerate={FPS}/1")
}

pub fn cam_caps() -> String {
    video_caps(CAM_W, CAM_H)
}

pub fn screen_caps() -> String {
    video_caps(SCREEN_W, SCREEN_H)
}

pub fn program_caps() -> String {
    format!(
        "video/x-raw,format=BGRx,width={SCREEN_W},height={SCREEN_H},pixel-aspect-ratio=1/1,framerate={FPS}/1"
    )
}

/// UDP ports used by one machine (it listens on them; the peer sends to them).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ports {
    pub control: u16,
    pub cam: u16,
    pub mic: u16,
    pub screen: u16,
    pub share: u16,
}

impl Ports {
    pub fn from_base(base: u16) -> Ports {
        Ports {
            control: base,
            cam: base + 2,
            mic: base + 4,
            screen: base + 6,
            share: base + 8,
        }
    }
}

/// Names of the in-process channels.  The prefix lets several instances run
/// in one process (tests).
#[derive(Clone, Debug)]
pub struct Channels {
    pub prefix: String,
}

impl Channels {
    pub fn new(prefix: &str) -> Self {
        Channels {
            prefix: prefix.to_string(),
        }
    }

    pub fn get(&self, name: &str) -> String {
        format!("{}{}", self.prefix, name)
    }
}

/// The interaudio elements buffer up to `buffer-time` when no one reads;
/// keep it short so the recording's audio does not lag behind video.
const INTERAUDIO_SRC_PROPS: &str =
    "buffer-time=80000000 latency-time=20000000 period-time=10000000";

/// H.264 for the LAN.  Intra refresh instead of big keyframes: a 1080p
/// keyframe is a burst of hundreds of kilobytes, more than the default UDP
/// receive buffer, and the lost packets show up as green blocks.
fn h264_sender(valve: &str, net: &str, bitrate_kbps: u32) -> String {
    format!(
        "queue leaky=downstream max-size-buffers=2 ! valve name={valve} drop=true drop-mode=forward-sticky-events ! \
         x264enc tune=zerolatency speed-preset=ultrafast bitrate={bitrate_kbps} key-int-max={FPS} intra-refresh=true \
         vbv-buf-capacity=200 ! \
         rtph264pay config-interval=-1 pt={VIDEO_PT} ! multiudpsink name={net} sync=false async=false"
    )
}

fn opus_sender(valve: &str, net: &str, bitrate: u32) -> String {
    format!(
        "queue leaky=downstream max-size-time=200000000 ! valve name={valve} drop=true drop-mode=forward-sticky-events ! \
         audioconvert ! opusenc bitrate={bitrate} frame-size=10 ! rtpopuspay pt={AUDIO_PT} ! \
         multiudpsink name={net} sync=false async=false"
    )
}

#[derive(Clone, Debug, PartialEq)]
pub enum CamSource {
    Device {
        path: String,
        mode: Option<CamMode>,
    },
    /// A moving ball on a solid background of the given 0xRRGGBB color.
    Test(u32),
}

/// Local camera: shown/recorded via `cam_local`, sent to the peer as H.264.
pub fn camera(ch: &Channels, src: &CamSource) -> String {
    let source = match src {
        CamSource::Device {
            path,
            mode: Some(m),
        } => {
            format!(
                "v4l2src device={path} do-timestamp=true ! {} ! {}",
                m.caps(),
                if m.is_jpeg() { "jpegdec !" } else { "" }
            )
        }
        CamSource::Device { path, mode: None } => {
            format!("v4l2src device={path} do-timestamp=true ! decodebin !")
        }
        CamSource::Test(rgb) => format!(
            "videotestsrc is-live=true pattern=ball background-color={:#010x} ! video/x-raw,width={CAM_W},height={CAM_H} !",
            0xff00_0000u32 | rgb
        ),
    };
    format!(
        "{source} videoconvert ! videoscale add-borders=true ! videorate ! {caps} ! tee name=tcam \
         tcam. ! queue leaky=downstream max-size-buffers=2 ! intervideosink sync=false async=false channel={local} \
         tcam. ! {send}",
        caps = cam_caps(),
        local = ch.get("cam_local"),
        send = h264_sender("valve_cam", "net_cam", 2500),
    )
}

#[derive(Clone, Debug, PartialEq)]
pub enum MicSource {
    Default,
    Device(String),
    Test,
}

/// Local microphone, with a `volume` named "micvol" for muting.
pub fn microphone(ch: &Channels, src: &MicSource) -> String {
    let source = match src {
        MicSource::Default => "pulsesrc buffer-time=20000 latency-time=10000".to_string(),
        MicSource::Device(d) => format!("pulsesrc device={d} buffer-time=20000 latency-time=10000"),
        MicSource::Test => "audiotestsrc is-live=true wave=sine freq=440 volume=0.05".to_string(),
    };
    format!(
        "{source} ! audioconvert ! audioresample ! {AUDIO_CAPS} ! volume name=micvol ! tee name=tmic \
         tmic. ! queue leaky=downstream max-size-time=200000000 ! interaudiosink sync=false async=false channel={local} \
         tmic. ! {send}",
        local = ch.get("mic_local"),
        send = opus_sender("valve_mic", "net_mic", 64000),
    )
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScreenSource {
    /// An X11 window id.
    Window(u64),
    /// The whole X11 screen.
    Screen,
    Test,
}

/// Shared space of the presenter, plus (optionally) the sound of the shared app
/// captured from the PulseAudio source `audio_source`.
pub fn screen(ch: &Channels, src: &ScreenSource, audio_source: Option<&str>) -> String {
    let source = match src {
        ScreenSource::Window(xid) => {
            format!("ximagesrc xid={xid} use-damage=false show-pointer=true")
        }
        ScreenSource::Screen => "ximagesrc use-damage=false show-pointer=true".to_string(),
        ScreenSource::Test => "videotestsrc is-live=true pattern=smpte".to_string(),
    };
    let mut s = format!(
        "{source} ! video/x-raw,framerate={FPS}/1 ! videoconvert ! videoscale add-borders=true ! {caps} ! tee name=tscreen \
         tscreen. ! queue leaky=downstream max-size-buffers=2 ! intervideosink sync=false async=false channel={local} \
         tscreen. ! {send}",
        caps = screen_caps(),
        local = ch.get("screen_local"),
        send = h264_sender("valve_screen", "net_screen", 8000),
    );
    if let Some(a) = audio_source {
        let src = if a == "test" {
            "audiotestsrc is-live=true wave=ticks volume=0.05".to_string()
        } else {
            format!("pulsesrc device={a} buffer-time=20000 latency-time=10000")
        };
        s += &format!(
            " {src} ! audioconvert ! audioresample ! {AUDIO_CAPS} ! tee name=tshare \
             tshare. ! queue leaky=downstream max-size-time=200000000 ! interaudiosink sync=false async=false channel={local} \
             tshare. ! {send}",
            local = ch.get("share_local"),
            send = opus_sender("valve_share", "net_share", 128000),
        );
    }
    s
}

fn receive_pair(
    video_port: u16,
    video_channel: &str,
    video_caps: &str,
    audio_port: u16,
    audio_channel: &str,
    playback: bool,
) -> String {
    let tee = format!("t_{audio_channel}");
    let speakers = if playback {
        format!(
            "{tee}. ! queue leaky=downstream max-size-time=200000000 ! pulsesink buffer-time=40000 latency-time=10000 sync=false async=false"
        )
    } else {
        format!("{tee}. ! queue ! fakesink sync=false async=false")
    };
    format!(
        "udpsrc port={video_port} buffer-size=4194304 caps=\"application/x-rtp,media=video,clock-rate=90000,encoding-name=H264,payload={VIDEO_PT}\" ! \
         rtpjitterbuffer latency=50 drop-on-latency=true ! rtph264depay ! h264parse ! avdec_h264 ! \
         videoconvert ! videoscale add-borders=true ! videorate ! {video_caps} ! \
         queue leaky=downstream max-size-buffers=2 ! intervideosink sync=false async=false channel={video_channel} \
         udpsrc port={audio_port} caps=\"application/x-rtp,media=audio,clock-rate=48000,encoding-name=OPUS,payload={AUDIO_PT}\" ! \
         rtpjitterbuffer latency=40 drop-on-latency=true ! rtpopusdepay ! opusdec plc=true ! \
         audioconvert ! audioresample ! {AUDIO_CAPS} ! tee name={tee} \
         {speakers} \
         {tee}. ! queue leaky=downstream max-size-time=200000000 ! interaudiosink sync=false async=false channel={audio_channel}"
    )
}

/// Peer's camera and voice (voice is also played on the speakers).
pub fn receive(ch: &Channels, ports: &Ports, playback: bool) -> String {
    receive_pair(
        ports.cam,
        &ch.get("cam_remote"),
        &cam_caps(),
        ports.mic,
        &ch.get("mic_remote"),
        playback,
    )
}

/// Peer's shared space and its sound.
pub fn receive_screen(ch: &Channels, ports: &Ports, playback: bool) -> String {
    receive_pair(
        ports.screen,
        &ch.get("screen_remote"),
        &screen_caps(),
        ports.share,
        &ch.get("share_remote"),
        playback,
    )
}

/// Names of the queues feeding the compositor, in pad order.
pub const PROGRAM_INPUTS: [&str; 4] = ["screen_local", "screen_remote", "cam_local", "cam_remote"];

/// Composes the final picture.  `overlay` (cairooverlay) draws annotations,
/// `preview` (appsink) feeds the UI, the result goes to channel `program`.
pub fn program(ch: &Channels) -> String {
    let mut s = format!(
        "compositor name=comp background=black ! {caps} ! cairooverlay name=overlay ! tee name=tprog \
         tprog. ! queue leaky=downstream max-size-buffers=2 ! videoscale ! videoconvert ! \
         video/x-raw,format=BGRA,width={PREVIEW_W},height={PREVIEW_H} ! appsink name=preview drop=true max-buffers=1 sync=false async=false \
         tprog. ! queue leaky=downstream max-size-buffers=2 ! intervideosink sync=false async=false channel={program}",
        caps = program_caps(),
        program = ch.get("program"),
    );
    for input in PROGRAM_INPUTS {
        let caps = if input.starts_with("cam") {
            cam_caps()
        } else {
            screen_caps()
        };
        s += &format!(
            " intervideosrc channel={} ! {caps} ! queue name=q_{input} max-size-buffers=2 leaky=downstream ! comp.",
            ch.get(input)
        );
    }
    s
}

pub const RECORD_AUDIO_INPUTS: [&str; 4] =
    ["mic_local", "mic_remote", "share_local", "share_remote"];

/// Records the program picture and all audio into Matroska (H.264 + Opus).
/// The output file is set on the element named "file".
pub fn record(ch: &Channels, bitrate_kbps: u32, preset: &str) -> String {
    let preset = match preset {
        "ultrafast" | "superfast" | "veryfast" | "faster" | "fast" | "medium" => preset,
        _ => "veryfast",
    };
    // Generous queues only for compressed data and audio (cheap); raw 1080p
    // frames are 8 MB each, so the queue in front of the encoder stays short.
    let big = "max-size-time=3000000000 max-size-buffers=0 max-size-bytes=0";
    let mut s = format!(
        "intervideosrc channel={program} ! {caps} ! queue max-size-buffers=8 max-size-time=0 max-size-bytes=0 ! \
         videoconvert ! video/x-raw,format=I420 ! \
         x264enc speed-preset={preset} bitrate={bitrate_kbps} key-int-max={gop} bframes=2 rc-lookahead=20 ! \
         video/x-h264,profile=high ! h264parse ! queue {big} ! mux. \
         audiomixer name=amix ! audioconvert ! audio/x-raw,channels=2 ! opusenc bitrate=128000 ! queue {big} ! mux. \
         matroskamux name=mux ! filesink name=file",
        program = ch.get("program"),
        caps = program_caps(),
        gop = FPS * 2,
    );
    for input in RECORD_AUDIO_INPUTS {
        s += &format!(
            " interaudiosrc channel={} {INTERAUDIO_SRC_PROPS} ! {AUDIO_CAPS} ! queue {big} ! amix.",
            ch.get(input)
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch() -> Channels {
        Channels::new("t1-")
    }

    #[test]
    fn inter_sinks_never_sync() {
        let all = [
            camera(&ch(), &CamSource::Test(0)),
            microphone(&ch(), &MicSource::Test),
            screen(&ch(), &ScreenSource::Test, Some("test")),
            receive(&ch(), &Ports::from_base(1000), true),
            receive_screen(&ch(), &Ports::from_base(1000), true),
            program(&ch()),
        ]
        .join(" ");
        let sinks = all.matches("intervideosink").count() + all.matches("interaudiosink").count();
        assert_eq!(
            sinks,
            all.matches("sink sync=false async=false channel=").count()
        );
        assert!(sinks >= 9);
    }

    #[test]
    fn ports_do_not_overlap() {
        let p = Ports::from_base(47100);
        let mut v = vec![p.control, p.cam, p.mic, p.screen, p.share];
        v.dedup();
        assert_eq!(v.len(), 5);
        // RTP convention: even ports, leaving odd ones for RTCP.
        assert!([p.cam, p.mic, p.screen, p.share].iter().all(|x| x % 2 == 0));
    }

    #[test]
    fn camera_uses_chosen_mode() {
        let mode = CamMode {
            format: "jpeg".into(),
            width: 960,
            height: 540,
            fps_n: 30,
            fps_d: 1,
        };
        let s = camera(
            &ch(),
            &CamSource::Device {
                path: "/dev/video0".into(),
                mode: Some(mode),
            },
        );
        assert!(s.contains("v4l2src device=/dev/video0"));
        assert!(s.contains("image/jpeg,width=960") && s.contains("jpegdec"));
        assert!(
            s.contains("channel=t1-cam_local")
                && s.contains("name=valve_cam")
                && s.contains("name=net_cam")
        );
        assert!(camera(&ch(), &CamSource::Test(0x204060)).contains("background-color=0xff204060"));
    }

    #[test]
    fn screen_with_and_without_audio() {
        let s = screen(&ch(), &ScreenSource::Window(0x3a00007), None);
        assert!(s.contains("ximagesrc xid=60817415"));
        assert!(!s.contains("share_local"));
        let s = screen(&ch(), &ScreenSource::Screen, Some("twofcast_share.monitor"));
        assert!(
            s.contains("pulsesrc device=twofcast_share.monitor")
                && s.contains("channel=t1-share_local")
        );
    }

    #[test]
    fn program_has_all_inputs_and_outputs() {
        let s = program(&ch());
        for i in PROGRAM_INPUTS {
            assert!(
                s.contains(&format!("channel=t1-{i} ")) && s.contains(&format!("name=q_{i} ")),
                "{i}"
            );
        }
        assert!(
            s.contains("cairooverlay name=overlay")
                && s.contains("appsink name=preview")
                && s.contains("channel=t1-program")
        );
    }

    #[test]
    fn record_mixes_all_audio() {
        let s = record(&ch(), 6000, "rm -rf");
        assert!(
            s.contains("speed-preset=veryfast"),
            "unknown preset falls back"
        );
        for i in RECORD_AUDIO_INPUTS {
            assert!(s.contains(&format!("channel=t1-{i} ")), "{i}");
        }
        assert!(s.contains("matroskamux") && s.contains("filesink name=file"));
        let before_encoder = &s[..s.find("x264enc").unwrap()];
        assert!(
            !before_encoder.contains("max-size-time=3000000000"),
            "no big raw-video queue"
        );
    }

    #[test]
    fn lan_video_avoids_keyframe_bursts() {
        assert!(camera(&ch(), &CamSource::Test(0)).contains("intra-refresh=true"));
        assert!(screen(&ch(), &ScreenSource::Screen, None).contains("intra-refresh=true"));
    }

    #[test]
    fn receive_routes_to_remote_channels() {
        let s = receive(&ch(), &Ports::from_base(47100), true);
        assert!(s.contains("port=47102") && s.contains("port=47104") && s.contains("pulsesink"));
        assert!(
            s.contains("sync=false async=false")
                && !s.contains("pulsesink buffer-time=40000 latency-time=10000 sync=false !")
        );
        assert!(s.contains("t1-cam_remote") && s.contains("t1-mic_remote"));
        let s = receive_screen(&ch(), &Ports::from_base(47100), false);
        assert!(
            s.contains("port=47106") && s.contains("t1-screen_remote") && !s.contains("pulsesink")
        );
    }
}
