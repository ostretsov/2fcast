# 2fcast

Two people, one shared screen, one recording — over the local network.

2fcast replaces the "voice chat + remote desktop + OBS" combo for a two-person
video podcast on Ubuntu:

* **Finds your partner by itself.** Start the app on both machines in the same
  LAN; they discover each other (mDNS) and connect. No addresses to type.
* **Talk in real time.** Opus voice over UDP with small buffers (estimated
  ~100 ms mouth-to-ear on a LAN).
  Use headphones (there is no echo cancellation).
* **Webcams.** Both cameras are shown and recorded.
* **Shared space.** Either of you clicks **Share…** and picks a window (a
  browser with a YouTube video, a presentation, …) or the entire screen. The
  other one sees it immediately. Sharing takes over from whoever shared before.
  The sound of the shared app (e.g. the YouTube video) is sent along too.
* **Draw and write** over the shared space — both of you, in four colors.
  **Undo** removes your last drawing, **Clear drawings** removes all.
  Drawings are cleared when someone else starts sharing.
* **Layout.** Pick a preset under **Layout**, or use **Arrange** to drag tiles
  and scroll to resize them. The layout is the same on both machines and is
  remembered between sessions.
* **Record.** Either of you presses **Record**. That machine writes exactly what
  the preview shows (1920×1080, 30 fps, H.264 + Opus in `.mkv`) to
  `~/Videos/2fcast/`. The other side sees "● … is recording". MKV stays
  playable even if the app or the computer crashes mid-recording.

## Install

On each machine (Ubuntu 22.04 or newer, X11 session):

```sh
sudo apt install ./2fcast_0.1.0_amd64.deb
```

`apt` pulls in everything the app needs (GStreamer plugins, GTK 4, `pactl`).
If something is still missing when the app starts, it lists the packages and
offers an **Install** button (asks for your password).

Build the package yourself:

```sh
curl https://sh.rustup.rs -sSf | sh       # Rust, once
scripts/install-build-deps.sh             # build dependencies, once
scripts/build-deb.sh                      # -> target/2fcast_0.1.0_amd64.deb
```

Or run from source: `cargo run --release`.

## Use

1. Both start **2fcast**. The title shows "Connected to …" within a few seconds.
2. Talk. Share windows, draw, arrange the layout.
3. When ready, one of you presses **● Record**; press **■ Stop** at the end.
   The status line at the bottom shows where the file was saved.

## Settings

`~/.config/2fcast/config.toml` is created on first start:

| key | default | meaning |
|---|---|---|
| `name` | your user name | shown to the other person |
| `camera` | `"auto"` | `"auto"`, `"/dev/video2"`, `"none"` or `"test"` |
| `microphone` | `"default"` | PulseAudio source (`pactl list sources short`), `"none"`, `"test"` |
| `output_dir` | `~/Videos/2fcast` | where recordings go |
| `base_port` | `47100` | TCP control port; UDP media uses `base_port+2 … +8` |
| `peer_address` | none | `"192.168.1.20:47100"` if your network blocks multicast (set it on one machine) |
| `record_bitrate_kbps` | `6000` | video bitrate of the recording (~2.7 GB/hour max) |
| `record_preset` | `"veryfast"` | x264 speed: `superfast` uses less CPU, `faster` gives better quality |

Run a second instance on one machine (for trying things out):
`2fcast --config-dir /tmp/other` with a different `base_port` in its config.

## Troubleshooting

* **They don't find each other** — a firewall may block mDNS or the ports:
  `sudo ufw allow 5353/udp && sudo ufw allow 47100:47110/tcp && sudo ufw allow 47100:47110/udp`.
  Or set `peer_address` on one machine.
* **Camera busy** — close other apps using it (OBS, browser tabs).
* **Wayland session** — window capture needs X11 ("Ubuntu on Xorg" at login).
* Diagnostics: `TWOFCAST_DEBUG=1 2fcast`, `GST_DEBUG=2 2fcast`.

## How it works

```
crates/core   pure logic, fully unit-tested, no display/camera needed
  protocol    JSON-lines control messages over TCP
  session     shared state; presenter + layout are last-writer-wins registers
              (seq, id), so simultaneous changes converge on both machines
  net         TCP connection, mDNS discovery, keep-alive, reconnect
  layout      16:9 tiles on the 1920x1080 canvas, presets, drag/resize, hit tests
  annotations strokes/text in shared-space-relative coordinates, cairo drawing
  pipelines   GStreamer pipeline descriptions
  camera      picks a webcam mode;  pulse: pactl parsing;  deps: package check
crates/app    GTK 4 UI + GStreamer glue
  media       runs the pipelines;  ui: window, tools, glue
  windows     X11 window list;     share_audio: routes the shared app's sound
```

Each machine runs small GStreamer pipelines connected through `inter*`
elements: camera, microphone and shared screen are captured once and go both
to the local compositor and (H.264/Opus over RTP/UDP) to the partner. The
**program** pipeline composes the shared space and both cameras according to
the layout, draws the annotations on top, and feeds both the preview and the
recorder. LAN video uses x264 `ultrafast`/`zerolatency` with intra refresh
(no big keyframe bursts); the recording uses x264 `veryfast` at 6 Mbit/s.

The shared app's sound is moved into a private PulseAudio null sink whose
monitor is captured (a loopback keeps it audible locally), so the partner's
voice played by 2fcast is never sent back to them.

Measured on a 4-core i7-7700HQ laptop: about 0.9 core while just talking,
about 1.5–2 cores while sharing a 1080p window and recording, ~0.4 GB RAM (0.8 GB
while recording), constant over time.

## Tests

```sh
cargo test                                         # everything (~10 s)
cargo test -p twofcast-core -- --ignored           # + real mDNS discovery
```

* `crates/core`: protocol, session convergence (simultaneous presenting,
  restarts, reconnects), layout, annotation rendering, networking over
  localhost (connect, disconnect detection, reconnect, self/duplicate
  rejection), pactl parsing (with real PipeWire output), configuration.
* `crates/app/tests/media.rs`: two complete media stacks in one process talking
  over UDP — each sees the other's camera, a shared screen reaches the peer with
  annotations drawn on it, a recording is a valid 1080p H.264 + stereo Opus MKV.

## Limits of version 1

Exactly two participants · X11 only · no echo cancellation (use headphones) ·
no remote control of the partner's mouse · the recording machine records the
partner's camera and screen at LAN-stream quality.

License: MIT.
