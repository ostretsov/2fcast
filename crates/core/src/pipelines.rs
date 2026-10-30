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
pub const PREVIEW_W: u32 = 960;
pub const PREVIEW_H: u32 = 540;

/// Shared sound (and the final mix) keeps stereo.
pub const AUDIO_CAPS: &str = "audio/x-raw,format=S16LE,rate=48000,channels=2,layout=interleaved";
/// A voice is mono: one microphone, heard in the middle.
pub const VOICE_CAPS: &str = "audio/x-raw,format=S16LE,rate=48000,channels=1,layout=interleaved";
const VIDEO_PT: u32 = 96;

/// `level` elements measuring my voice (after mute) and the partner's voice;
/// they post a bus message every [`LEVEL_INTERVAL_NS`].
pub const LEVEL_MIC: &str = "level_mic";
/// Measures the two input channels before they become one voice; feeds the
/// automatic channel choice ([`crate::mic_channel`]).
pub const LEVEL_MIC_INPUT: &str = "level_mic_in";
pub const LEVEL_PEER: &str = "level_peer";
pub const LEVEL_INTERVAL_NS: u64 = 50_000_000;

/// An `identity` whose stereo buffers the app rewrites: the chosen input
/// channel(s) mixed into one voice ([`crate::mic_channel`]), noise-gated
/// ([`crate::gate`]) and written to both channels; the following
/// `audioconvert` then makes it mono.
pub const MIC_PROCESS: &str = "mic_process";
/// `identity` before the speakers playing the partner's voice: lowers it
/// while I speak (only there; the recording takes the voice before it).
pub const DUCK: &str = "duck";

/// Mixes interleaved stereo frames into mono with (left, right) weights.
pub fn mix_to_mono(stereo: &[i16], gains: [f32; 2]) -> Vec<i16> {
    stereo
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[l, r]| {
            (*l as f32 * gains[0] + *r as f32 * gains[1])
                .round()
                .clamp(-32768.0, 32767.0) as i16
        })
        .collect()
}
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

/// H.264 for the LAN.  A keyframe every second, so a receiver that starts
/// late (the partner takes over sharing, or restarts the app) has a complete
/// picture within a second.  The VBV buffer (100 ms) caps every frame at about
/// a tenth of a second of bitrate: an uncapped 1080p keyframe is a burst of
/// hundreds of kilobytes, more than the default UDP receive buffer (~200 KB),
/// and the lost packets show up as green blocks.  SPS/PPS are repeated every
/// second (`config-interval=1`); `-1` (before every keyframe) makes OpenH264
/// (the decoder of the self-contained package) decode nothing at all.
fn h264_sender(valve: &str, net: &str, bitrate_kbps: u32) -> String {
    format!(
        "queue leaky=downstream max-size-buffers=2 ! valve name={valve} drop=true drop-mode=forward-sticky-events ! \
         x264enc tune=zerolatency speed-preset=ultrafast bitrate={bitrate_kbps} key-int-max={FPS} \
         vbv-buf-capacity=100 ! \
         rtph264pay config-interval=1 pt={VIDEO_PT} ! multiudpsink name={net} sync=false async=false"
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
    /// A test tone at this amplitude (0..1).
    Tone(f64),
    /// A test tone on the left channel only, like a mic on input 1 of an
    /// audio interface.
    ToneLeftOnly(f64),
}

/// Local microphone as a mono voice.  The input is taken as two channels (a
/// mono mic is simply duplicated) and rewritten at [`MIC_PROCESS`]; a
/// `volume` named "micvol" mutes.
pub fn microphone(ch: &Channels, src: &MicSource) -> String {
    let source = match src {
        MicSource::Default => "pulsesrc buffer-time=20000 latency-time=10000".to_string(),
        MicSource::Device(d) => format!("pulsesrc device={d} buffer-time=20000 latency-time=10000"),
        MicSource::Test => "audiotestsrc is-live=true wave=sine freq=440 volume=0.05".to_string(),
        MicSource::Tone(v) => format!("audiotestsrc is-live=true wave=sine freq=440 volume={v}"),
        MicSource::ToneLeftOnly(v) => format!(
            "audiotestsrc is-live=true wave=sine freq=440 volume={v} ! audio/x-raw,channels=1,rate=48000 ! \
             audioconvert mix-matrix=\"<<(float)1.0>, <(float)0.0>>\" ! audio/x-raw,channels=2"
        ),
    };
    format!(
        "{source} ! audioconvert ! audioresample ! {AUDIO_CAPS} ! \
         level name={LEVEL_MIC_INPUT} interval={LEVEL_INTERVAL_NS} ! identity name={MIC_PROCESS} ! \
         audioconvert ! {VOICE_CAPS} ! volume name=micvol ! level name={LEVEL_MIC} interval={LEVEL_INTERVAL_NS} ! tee name=tmic \
         tmic. ! queue leaky=downstream max-size-time=200000000 ! interaudiosink sync=false async=false channel={local} \
         tmic. ! {send}",
        local = ch.get("mic_local"),
        send = opus_sender("valve_mic", "net_mic", 128000),
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

/// One received stream: UDP port, `inter*` channel to publish to, raw caps.
struct Incoming<'a> {
    port: u16,
    channel: String,
    caps: &'a str,
}

/// H.264 decoders in order of preference: FFmpeg's (usual system install) or
/// OpenH264 (small; used by the self-contained package).
pub const H264_DECODERS: [&str; 2] = ["avdec_h264", "openh264dec"];

fn receive_pair(
    video: Incoming,
    audio: Incoming,
    playback: bool,
    level: Option<&str>,
    duck: bool,
    decoder: &str,
) -> String {
    let tee = format!("t_{}", audio.channel);
    let level = level
        .map(|name| format!("level name={name} interval={LEVEL_INTERVAL_NS} ! "))
        .unwrap_or_default();
    let speakers = if playback {
        let duck = if duck {
            format!("identity name={DUCK} ! ")
        } else {
            String::new()
        };
        format!(
            "{tee}. ! queue leaky=downstream max-size-time=200000000 ! {duck}pulsesink buffer-time=40000 latency-time=10000 sync=false async=false"
        )
    } else {
        format!("{tee}. ! queue ! fakesink sync=false async=false")
    };
    format!(
        "udpsrc port={vport} buffer-size=4194304 caps=\"application/x-rtp,media=video,clock-rate=90000,encoding-name=H264,payload={VIDEO_PT}\" ! \
         rtpjitterbuffer latency=50 drop-on-latency=true ! rtph264depay ! h264parse ! {decoder} ! \
         videoconvert ! videoscale add-borders=true ! videorate ! {vcaps} ! \
         queue leaky=downstream max-size-buffers=2 ! intervideosink sync=false async=false channel={vchannel} \
         udpsrc port={aport} caps=\"application/x-rtp,media=audio,clock-rate=48000,encoding-name=OPUS,payload={AUDIO_PT}\" ! \
         rtpjitterbuffer latency=40 drop-on-latency=true ! rtpopusdepay ! opusdec plc=true ! \
         audioconvert ! audioresample ! {acaps} ! {level}tee name={tee} \
         {speakers} \
         {tee}. ! queue leaky=downstream max-size-time=200000000 ! interaudiosink sync=false async=false channel={achannel}",
        vport = video.port,
        vcaps = video.caps,
        vchannel = video.channel,
        aport = audio.port,
        acaps = audio.caps,
        achannel = audio.channel,
    )
}

/// Peer's camera and voice (voice is also played on the speakers).
pub fn receive(ch: &Channels, ports: &Ports, playback: bool, decoder: &str) -> String {
    let cam_caps = cam_caps();
    receive_pair(
        Incoming {
            port: ports.cam,
            channel: ch.get("cam_remote"),
            caps: &cam_caps,
        },
        Incoming {
            port: ports.mic,
            channel: ch.get("mic_remote"),
            caps: VOICE_CAPS,
        },
        playback,
        Some(LEVEL_PEER),
        true,
        decoder,
    )
}

/// Peer's shared space and its sound.
pub fn receive_screen(ch: &Channels, ports: &Ports, playback: bool, decoder: &str) -> String {
    let screen_caps = screen_caps();
    receive_pair(
        Incoming {
            port: ports.screen,
            channel: ch.get("screen_remote"),
            caps: &screen_caps,
        },
        Incoming {
            port: ports.share,
            channel: ch.get("share_remote"),
            caps: AUDIO_CAPS,
        },
        playback,
        None,
        false,
        decoder,
    )
}

/// Names of the queues feeding the compositor, in pad order.
pub const PROGRAM_INPUTS: [&str; 4] = ["screen_local", "screen_remote", "cam_local", "cam_remote"];

/// Composes the final picture.  `overlay` (cairooverlay) draws annotations,
/// `preview` (appsink) feeds the UI, the result goes to channel `program`.
///
/// Keeping it real-time matters: if composing ever takes longer than a frame,
/// the output would fall further and further behind (the preview and the
/// recorded picture lagging the sound by seconds).  So the preview sink syncs
/// to the clock with QoS, which makes the compositor drop late frames instead;
/// and each input converts to the compositor's format in its own thread.
pub fn program(ch: &Channels) -> String {
    let mut s = format!(
        "compositor name=comp background=black ! {caps} ! cairooverlay name=overlay ! tee name=tprog \
         tprog. ! queue leaky=downstream max-size-buffers=2 ! videoscale ! videoconvert ! \
         video/x-raw,format=BGRA,width={PREVIEW_W},height={PREVIEW_H} ! appsink name=preview drop=true max-buffers=1 sync=true qos=true async=false \
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
            " intervideosrc channel={} ! {caps} ! videoconvert ! video/x-raw,format=BGRx ! \
             queue name=q_{input} max-size-buffers=2 leaky=downstream ! comp.",
            ch.get(input)
        );
    }
    s
}

pub const RECORD_AUDIO_INPUTS: [&str; 4] =
    ["mic_local", "mic_remote", "share_local", "share_remote"];

pub const RECORD_AUDIO_BITRATE: u32 = 128_000;

/// Names of the audio tracks in the recording, shown by players and editors.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackNames {
    pub me: String,
    pub peer: String,
}

impl TrackNames {
    /// (encoder element name, track title) in track order.  The titles are
    /// applied from code (not in the description) so no escaping is needed.
    pub fn titles(&self) -> [(&'static str, String); 4] {
        [
            ("enc_mix", "Mix".into()),
            ("enc_mic_local", mkv_title(&self.me, "Me")),
            ("enc_mic_remote", mkv_title(&self.peer, "Partner")),
            ("enc_shared", "Shared_sound".into()),
        ]
    }
}

/// GStreamer 1.20's matroskamux stores a title containing anything but
/// `[A-Za-z0-9_+-./:]` in escaped, quoted form (`"Artem\ O."`), so titles are
/// reduced to those characters: spaces become `_`, the rest is dropped.
pub fn mkv_title(name: &str, fallback: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() || "_+-./:".contains(c) {
            out.push(c);
        } else if c.is_whitespace() && !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        fallback.to_string()
    } else {
        out
    }
}

/// Records the program picture and the sound into Matroska: H.264 video and
/// four Opus tracks, in this order:
///
/// 1. everything mixed (what YouTube and players use by default),
/// 2. my voice, 3. the partner's voice, 4. the shared apps' sound,
///
/// so the voices can be balanced or cleaned up separately in an editor.
/// The output file is set on the element named "file".
pub fn record(ch: &Channels, bitrate_kbps: u32, preset: &str) -> String {
    let preset = match preset {
        "ultrafast" | "superfast" | "veryfast" | "faster" | "fast" | "medium" => preset,
        _ => "veryfast",
    };
    // Generous queues only for compressed data and audio (cheap); raw 1080p
    // frames are 8 MB each, so the queue in front of the encoder stays short.
    let big = "max-size-time=3000000000 max-size-buffers=0 max-size-bytes=0";
    // Raw data right after the sources is queued *leaky*: if the muxer ever
    // stalls (an overloaded machine), sources drop data instead of blocking,
    // and a blocked source would keep the end-of-stream from ever arriving
    // (stopping the recording would hang).
    let raw = format!("leaky=downstream {big}");
    let track = |encoder: &str, channels: u32| {
        format!(
            "queue {raw} ! audioconvert ! audio/x-raw,channels={channels} ! opusenc name={encoder} bitrate={RECORD_AUDIO_BITRATE} ! \
             queue {big} ! mux."
        )
    };
    let mut s = format!(
        "intervideosrc channel={program} ! {caps} ! queue leaky=downstream max-size-buffers=8 max-size-time=0 max-size-bytes=0 ! \
         videoconvert ! video/x-raw,format=I420 ! \
         x264enc speed-preset={preset} bitrate={bitrate_kbps} key-int-max={gop} bframes=2 rc-lookahead=10 threads=4 ! \
         video/x-h264,profile=high ! h264parse ! queue {big} ! mux. \
         audiomixer name=amix ! {mix} \
         matroskamux name=mux ! filesink name=file",
        program = ch.get("program"),
        caps = program_caps(),
        gop = FPS * 2,
        mix = track("enc_mix", 2),
    );
    // Each channel can only be read once, so every input is split with a tee:
    // one copy into the mix, one into its own track (or the shared-sound mix).
    for input in RECORD_AUDIO_INPUTS {
        let tee = format!("ta_{input}");
        let voice = input.starts_with("mic");
        let own = if voice {
            track(&format!("enc_{input}"), 1)
        } else {
            format!("queue {raw} ! shared.")
        };
        let caps = if voice { VOICE_CAPS } else { AUDIO_CAPS };
        // Voices are mono; in the stereo mix they sit in the middle.
        s += &format!(
            " interaudiosrc channel={} {INTERAUDIO_SRC_PROPS} ! {caps} ! tee name={tee} \
             {tee}. ! queue {raw} ! audioconvert ! {AUDIO_CAPS} ! amix. \
             {tee}. ! {own}",
            ch.get(input)
        );
    }
    // Last, so it becomes the last track.
    s += &format!(" audiomixer name=shared ! {}", track("enc_shared", 2));
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
            receive(&ch(), &Ports::from_base(1000), true, "avdec_h264"),
            receive_screen(&ch(), &Ports::from_base(1000), true, "avdec_h264"),
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
    fn program_stays_real_time() {
        let s = program(&ch());
        assert!(
            s.contains("appsink name=preview drop=true max-buffers=1 sync=true qos=true"),
            "late frames are dropped"
        );
        assert_eq!(
            s.matches("videoconvert ! video/x-raw,format=BGRx ! queue name=q_")
                .count(),
            4,
            "inputs convert in their own threads"
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
        // Mix, my voice, partner's voice, shared sound - in this order.
        let names = TrackNames {
            me: " Artem \"A\" ".into(),
            peer: "".into(),
        };
        let titles = names.titles();
        let mut last = 0;
        for (enc, _) in &titles {
            let at = s
                .find(&format!("opusenc name={enc} "))
                .unwrap_or_else(|| panic!("{enc}"));
            assert!(at > last || *enc == "enc_mix", "{enc} out of order");
            last = at;
        }
        assert_eq!(titles[1].1, "Artem_A");
        assert_eq!(titles[2].1, "Partner");
        assert_eq!(
            s.matches(&format!("bitrate={RECORD_AUDIO_BITRATE}"))
                .count(),
            4
        );
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
            before_encoder.contains("queue leaky=downstream"),
            "raw video never blocks the source"
        );
        assert_eq!(
            s.matches("interaudiosrc").count(),
            s.matches("! tee name=ta_").count()
        );
        assert!(
            !before_encoder.contains("max-size-time=3000000000"),
            "no big raw-video queue"
        );
    }

    #[test]
    fn lan_video_avoids_keyframe_bursts() {
        let s = screen(&ch(), &ScreenSource::Screen, None);
        assert!(
            s.contains("key-int-max=30 ") && s.contains("vbv-buf-capacity=100"),
            "{s}"
        );
        assert!(
            s.contains("rtph264pay config-interval=1 "),
            "OpenH264 cannot decode config-interval=-1"
        );
    }

    #[test]
    fn mkv_titles_are_safe() {
        assert_eq!(mkv_title("Artem O.", "Me"), "Artem_O.");
        assert_eq!(mkv_title("  Брат  \"Ivan\" ", "Partner"), "Ivan");
        assert_eq!(mkv_title("Брат", "Partner"), "Partner");
        assert_eq!(mkv_title("", "Me"), "Me");
    }

    #[test]
    fn voices_are_metered() {
        let mic = microphone(&ch(), &MicSource::Default);
        assert!(mic.contains("volume name=micvol ! level name=level_mic "));
        assert!(
            mic.contains(
                "level name=level_mic_in interval=50000000 ! identity name=mic_process ! "
            )
        );
        assert!(
            mic.contains(&format!(
                "mic_process ! audioconvert ! {VOICE_CAPS} ! volume"
            )),
            "processed, then mono, then mute"
        );
        assert!(
            receive(&ch(), &Ports::from_base(1), true, "avdec_h264")
                .contains("level name=level_peer ")
        );
        assert!(
            !receive_screen(&ch(), &Ports::from_base(1), true, "avdec_h264")
                .contains("level name=")
        );
    }

    #[test]
    fn decoder_is_configurable() {
        let s = receive(&ch(), &Ports::from_base(1), false, "openh264dec");
        assert!(s.contains("h264parse ! openh264dec ! ") && !s.contains("avdec_h264"));
    }

    #[test]
    fn stereo_mixes_to_mono() {
        let st = [1000, 0, -2000, 4000, 32767, 32767];
        assert_eq!(mix_to_mono(&st, [1.0, 0.0]), vec![1000, -2000, 32767]);
        assert_eq!(mix_to_mono(&st, [0.0, 1.0]), vec![0, 4000, 32767]);
        assert_eq!(mix_to_mono(&st, [0.5, 0.5]), vec![500, 1000, 32767]);
        assert!(
            mix_to_mono(&[1], [1.0, 0.0]).is_empty(),
            "incomplete frame ignored"
        );
    }

    #[test]
    fn voice_is_sent_at_128k() {
        assert!(microphone(&ch(), &MicSource::Test).contains("opusenc bitrate=128000"));
    }

    #[test]
    fn receive_routes_to_remote_channels() {
        let s = receive(&ch(), &Ports::from_base(47100), true, "avdec_h264");
        assert!(s.contains("port=47102") && s.contains("port=47104") && s.contains("pulsesink"));
        assert!(
            s.contains("sync=false async=false")
                && !s.contains("pulsesink buffer-time=40000 latency-time=10000 sync=false !")
        );
        assert!(s.contains("t1-cam_remote") && s.contains("t1-mic_remote"));
        assert!(
            s.contains("! identity name=duck ! pulsesink"),
            "only the speakers are ducked"
        );
        let s = receive_screen(&ch(), &Ports::from_base(47100), false, "avdec_h264");
        assert!(
            s.contains("port=47106") && s.contains("t1-screen_remote") && !s.contains("pulsesink")
        );
        let s = receive_screen(&ch(), &Ports::from_base(47100), true, "avdec_h264");
        assert!(!s.contains("name=duck"), "shared sound is not ducked");
    }
}
