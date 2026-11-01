# 2fcast

Two people, one shared screen, one recording — over the local network.

2fcast replaces the "voice chat + remote desktop + OBS" combo for a two-person
video podcast on Ubuntu:

* **Finds your partner by itself.** Start the app on both machines in the same
  LAN; they discover each other (mDNS) and connect. No addresses to type.
* **Talk in real time.** Mono Opus voice at 128 kbit/s over UDP with small
  buffers (estimated ~100 ms mouth-to-ear on a LAN).
  Use headphones (there is no echo cancellation).
* **Any microphone setup.** Audio interfaces such as the Focusrite Scarlett put
  input 1 on the left and input 2 on the right channel. 2fcast notices which
  input carries your voice (after about a second of speech) and uses only that
  one; a normal USB/laptop mic (both channels) is averaged. Either way the voice
  is mono and heard in the middle. Override with `microphone_channel`.
* **Noise gate.** While you are not speaking your mic is silenced, so room
  noise is neither sent nor recorded. It opens instantly above −45 dB, stays
  open 0.4 s after you stop (pauses between words are kept) and fades out.
  The dashed blue mark on your level meter shows the threshold.
* **Level meters.** The bottom bar shows both voices on a −60…0 dB scale (bar =
  loudness, thin mark = peak). Keep speech in the green; yellow is loud, red
  (above −3 dB) will distort. A muted mic reads "silent". Your own name is the
  bold one; hover over the meters to see which config file sets it.
* **Webcams.** Both cameras are shown and recorded.
* **Shared space.** Either of you clicks **Share…** and picks a window (a
  browser with a YouTube video, a presentation, …) or the entire screen. The
  other one sees it immediately. Either of you can share at any time; the
  latest one wins: if you share while recording and your brother then shares
  his window, his window replaces yours in the shared space (and in the
  recording, which simply continues) and both of you can draw on it.
  When he stops sharing, your window comes back (unless you closed it or
  clicked **Stop sharing** in the meantime).
  The sound of the shared app (e.g. the YouTube video) is sent along too
  (`share_sound = false` turns that off).
  A shared window that is bigger than the shared-space tile is shrunk (never
  enlarged, un-maximized if needed) to the tile size so it is recorded 1:1, and
  put back as it was when sharing stops. With the "Screen only" and "small
  cameras in corner" layouts the tile is the full 1920×1080 and the window is
  pixel-exact; in smaller tiles it is slightly softer (the shared screen always
  travels as a 1920×1080 frame).
* **Draw, point and write** over the shared space — both of you, in four
  colors (the one who joined first draws red, the second green, until you pick
  another): **Ping** (**Ctrl+P**, the default tool; click and ripples spread
  from that spot for a moment — "look here"; a **right click** pings with any
  tool), **Pen** (free hand), **Arrow** (**Ctrl+A**; drag from tail to tip),
  **Line** (**Ctrl+L**), **Rectangle** (**Ctrl+R**; drag corner to corner) —
  the mouse wheel makes these thinner or thicker, also while drawing — and
  **Text** (**Ctrl+T**; monospace code font, several lines, Tab indents). Text
  appears for both of you as it is typed, so you can write code live;
  **Ctrl+Enter** or **Esc** finishes it. **Undo** or **Ctrl+Z** removes your
  last drawing, **Clear drawings** or **Ctrl+C** removes all.
  Drawings are cleared when someone else starts sharing.
* **Mute** with the **Mic** button or **Ctrl+M**. While your mic is muted a big crossed-out microphone covers the
  shared space in *your* window (it is not recorded). Click it to unmute.
* **Self-view.** If seeing yourself distracts you, click **Self-view on** to switch it to
  **Self-view off**: your camera is covered by a crossed-out camera in *your* window only. Your
  partner still sees you and the recording still includes you. The choice is remembered.
* **Layout.** Pick a preset under **Layout** or press **Ctrl+1** … **Ctrl+4**
  (screen + cameras right, screen + small cameras, cameras only, screen only),
  or use **Arrange** (next to **Layout**) to drag tiles and scroll to resize them. The layout is
  the same on both machines and is remembered between sessions.
* **Picture while nobody shares.** Under **Layout**, **Picture while nobody shares…** picks
  an image (PNG, JPEG, SVG, …) shown — and recorded — in the shared space instead of black
  (also while the partner's shared screen is still arriving). It is remembered. Below the button
  you see how sharp it will look: it is scaled once to the shared space and placed 1:1, so the
  sharpest picture has exactly the shared space's size (1440×810 in "screen + cameras right",
  1920×1080 in "screen only"); a smaller one is enlarged and looks blurry. It is shown on your
  machine (and in your recordings) only.
* **Record.** Either of you presses **Record** (pressing it again asks before
  it stops). The button always shows the free space where recordings go
  ("● Record · 41.3 GB free"), so you know before you start. That machine writes exactly what
  the preview shows (1920×1080, 30 fps, H.264) to `~/Videos/2fcast/*.mkv`, with
  four Opus audio tracks (128 kbit/s each), in this order:
  1. `Mix` — everything together; players and YouTube use this one,
  2. the recording person's voice (mono, titled with their name),
  3. the partner's voice (mono, titled with their name),
  4. `Shared_sound` — the sound of shared apps (videos etc.), stereo.

  So you can upload the file as is, or balance and clean up each voice in an
  editor. Titles keep only `A–Z a–z 0–9 _ + - . / :` (spaces become `_`, a
  GStreamer 1.20 limitation); a name with none of these becomes `Me`/`Partner`.
  The partner's voice is recorded as it arrives over the LAN (128 kbit/s Opus).
  Both machines show who is recording and for how long next to the level
  meters ("● Artem recording 01:05", hours:minutes; one line each if both
  record). The recording machine's line also shows the file size
  ("· 1.2 GB"). MKV stays playable even if
  the app or the computer crashes mid-recording.
* **Folders.** The right of the bottom bar shows where recordings go and where
  the log is; click either path to open its folder.

## Install

On each machine (Ubuntu 22.04 or newer, X11 session):

```sh
sudo apt install ./2fcast_0.6.0_amd64.deb
```

The package is self-contained, like Discord: it brings its own GTK 4,
GStreamer (only the plugins 2fcast uses) and their libraries in
`/opt/2fcast`, and needs from the system only what every Ubuntu desktop has
(glibc, OpenGL, X11, fonts). It does not depend on "universe" or on library
names that change between Ubuntu releases. Tested on clean Ubuntu 22.04,
24.04 and 26.10. Check an installation with `2fcast --self-test`.

Build the package yourself (on the *oldest* Ubuntu it should run on):

```sh
curl https://sh.rustup.rs -sSf | sh       # Rust, once
make deps                                 # build dependencies, once (sudo)
make deb                                  # -> target/2fcast_0.6.0_amd64.deb
make deb V=0.3.1                          # bump the version (crates, Cargo.lock, README), then build
make docker-test                          # install it in clean Ubuntu containers + self-test
```

`make deb-system` builds a small alternative package that uses the system's
GTK/GStreamer packages instead (needs "universe"; if the app finds packages
missing at start it offers to install them and explains apt errors).

Or run from source: `make run-release`.

`make help` lists all tasks: `deps`, `build`, `run`, `test`, `lint`, `deb`,
`install` (build + `apt install` the package), and `demo` (two connected
instances on one machine — your webcam as "Artem", a test camera as
"Brother", no microphones).

## Use

1. Both start **2fcast**. The title shows "Connected to …" within a few seconds.
2. Talk. Share windows, draw, arrange the layout.
3. When ready, one of you presses **● Record**; press **■ Stop** at the end.
   The status line at the bottom shows where the file was saved; click the
   **Recordings** path to open the folder.

## Settings

`~/.config/2fcast/config.toml` is created on first start:

| key | default | meaning |
|---|---|---|
| `name` | your user name | shown to the other person |
| `camera` | `"auto"` | `"auto"`, `"/dev/video2"`, `"none"` or `"test"` |
| `microphone` | `"default"` | PulseAudio source (`pactl list sources short`), `"none"`, `"test"` |
| `microphone_channel` | `"auto"` | `"auto"`, `"both"`, `"left"` (input 1) or `"right"` (input 2) |
| `noise_gate` | `true` | silence the mic while you are not speaking |
| `noise_gate_db` | `-45.0` | gate threshold; raise it (e.g. `-38`) in a noisy room, lower it for a quiet voice |
| `high_pass_hz` | `80.0` | remove rumble, bumps and hum below this from the mic; `0` = off |
| `compressor` | `true` | even out loud and quiet speech (+6 dB for normal speech) and keep peaks from clipping |
| `duck_partner_db` | `6.0` | while you speak, the partner is this much quieter on *your speakers* (never in recordings); `0` = off |
| `share_sound` | `true` | also send and record the sound of the shared window |
| `discovery` | `true` | find the partner automatically (mDNS); `false`: only `peer_address` |
| `output_dir` | `~/Videos/2fcast` | where recordings go |
| `base_port` | `47100` | TCP control port; UDP media uses `base_port+2 … +8` |
| `peer_address` | none | `"192.168.1.20:47100"` if your network blocks multicast (set it on one machine) |
| `record_bitrate_kbps` | `6000` | video bitrate of the recording (~2.7 GB/hour max) |
| `record_preset` | `"veryfast"` | x264 speed: `superfast` uses less CPU, `faster` gives better quality |

Button choices that should survive a restart (currently **Self-view**) are
kept in `ui.json` next to it, so clicking buttons never touches your
hand-edited `config.toml`.
The layout is kept in `layout.json`.

Run a second instance on one machine (for trying things out):
`2fcast --config-dir /tmp/other` with a different `base_port` in its config.

## Control API

Start with `2fcast --control /run/user/$UID/2fcast.sock` (or set
`TWOFCAST_CONTROL`) and the app accepts JSON commands on that Unix socket
(only your user can connect). They run the same code as the buttons, and every
reply contains the app state. Send one with `2fcast --ctl SOCKET 'JSON'`:

```sh
S=/run/user/$UID/2fcast.sock
2fcast --ctl $S '{"cmd":"state"}'
2fcast --ctl $S '{"cmd":"windows"}'                      # shareable windows (xid, title)
2fcast --ctl $S '{"cmd":"share","xid":106954755}'        # omit xid: entire screen
2fcast --ctl $S '{"cmd":"stop_sharing"}'
2fcast --ctl $S '{"cmd":"record","on":true}'             # and false to stop
2fcast --ctl $S '{"cmd":"mute","on":true}'
2fcast --ctl $S '{"cmd":"hide_self_view","on":true}'     # my camera hidden on my screen only
2fcast --ctl $S '{"cmd":"idle_image","path":"/home/me/brb.png"}'  # omit path: black again
2fcast --ctl $S '{"cmd":"preset","name":"cams_only"}'    # screen_cams_right, screen_cams_overlay, cams_only, screen_only
2fcast --ctl $S '{"cmd":"draw","points":[[0.1,0.5],[0.9,0.5]]}'
2fcast --ctl $S '{"cmd":"text","x":0.1,"y":0.2,"text":"hello"}'
2fcast --ctl $S '{"cmd":"arrow","from":[0.2,0.8],"to":[0.5,0.5],"width":0.01}'
2fcast --ctl $S '{"cmd":"arrow","from":[0.2,0.2],"to":[0.5,0.5],"shape":"rect"}'  # or "line"
2fcast --ctl $S '{"cmd":"ping","x":0.5,"y":0.5}'
2fcast --ctl $S '{"cmd":"undo"}'
2fcast --ctl $S '{"cmd":"clear"}'
```

The state includes, among others, `recording` / `peer_recording`,
`recording_secs` / `peer_recording_secs` (how long each side has been
recording), `recording_file_bytes` (this machine's recording),
`disk_free_bytes` (where recordings go), `muted` and `self_view_hidden`.

Useful for hotkeys (e.g. a keyboard shortcut that starts recording) and for
testing the real app without simulating mouse clicks.

## Troubleshooting

* **They don't find each other** — a firewall may block mDNS or the ports:
  `sudo ufw allow 5353/udp && sudo ufw allow 47100:47110/tcp && sudo ufw allow 47100:47110/udp`.
  Or set `peer_address` on one machine.
* **Camera busy** — close other apps using it (OBS, browser tabs).
* **Wayland session** — window capture needs X11 ("Ubuntu on Xorg" at login).
* **Your voice is only in one ear / too quiet** — see `microphone_channel`.
* **Words are cut off at the start** — lower `noise_gate_db` (or `noise_gate = false`).
* **A red warning under the toolbar** says what is wrong and how to fix it:
  nothing arrives from the partner (firewall on this computer), the partner
  receives nothing from you (firewall on theirs), or a stream arrives but
  cannot be decoded.
* **Logs:** every run writes `~/.cache/2fcast/2fcast.log` (the previous run:
  `2fcast.prev.log`; click the **Log** path in the bottom bar to open the folder): versions, settings, connection, sharing, recording,
  errors, and per incoming stream when the first packet and the first decoded
  frame arrived. Send both machines' logs when reporting a problem.
  `TWOFCAST_DEBUG=1 2fcast` also prints it; `GST_DEBUG=2` adds GStreamer's own.
* `2fcast --self-test` checks the installation, incl. that video sent over the
  network loopback actually decodes.

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
recorder. LAN video uses x264 `ultrafast`/`zerolatency` with every frame capped
at 100 ms of bitrate (no big bursts). The camera has a keyframe every second;
the shared screen gets one only when the receiver asks (starting to receive,
lost packets), because a capped keyframe of sharp screen content is visibly
blurrier and regular ones made the shared screen pulse once a second. The
recording uses x264 `veryfast` at 6 Mbit/s.

The shared app's sound is moved into a private PulseAudio null sink whose
monitor is captured (a loopback keeps it audible locally), so the partner's
voice played by 2fcast is never sent back to them.

Measured on a 4-core i7-7700HQ laptop: about 0.9 core while just talking,
about 1.5–2 cores while sharing a 1080p window and recording, ~0.3–0.4 GB RAM
(~0.6 GB on the machine that records), constant over time.

## Tests

```sh
cargo test                                         # everything (~10 s)
cargo test -p twofcast-core -- --ignored           # + real mDNS discovery
```

* `crates/core`: protocol, session convergence (simultaneous presenting,
  restarts, reconnects, getting your share back after the partner's ends,
  recording times), layout, annotation rendering, networking over
  localhost (connect, disconnect detection, reconnect, self/duplicate
  rejection), pactl parsing (with real PipeWire output), configuration, level meters,
  microphone channel detection, the noise gate, the control protocol.
* `crates/app/tests/media.rs`: two complete media stacks in one process talking
  over UDP — each sees the other's camera, a shared screen reaches the peer with
  annotations drawn on it, a recording is a valid 1080p H.264 MKV with four
  named Opus tracks (voices mono), voice levels are measured on both sides
  (including mute and a partner leaving), a mic on one input channel is
  detected and centered, the noise gate silences quiet input, a partner can
  take over sharing while we record (picture within a few seconds), and a
  receiver that starts late still gets the picture.
* `crates/app/src/control.rs`: the control socket (round trip, permissions,
  never replacing a regular file).
* `make docker-test`: the `.deb` installed on clean Ubuntu releases, then
  `2fcast --self-test` (all GStreamer elements, camera/mic/screen over a UDP
  loopback, a recording checked for 1 video + 4 audio tracks, a GTK window).

## Limits of version 1

Exactly two participants · X11 only · no echo cancellation (use headphones) ·
no remote control of the partner's mouse · the recording machine records the
partner's camera and screen at LAN-stream quality.

License: MIT.
