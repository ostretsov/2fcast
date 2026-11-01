# Notes for coding agents

2fcast: two-person LAN videocasting app for Ubuntu (Rust, GTK 4, GStreamer).
`README.md` describes the features; this file is about working on the code.

## Layout

- `crates/core` — all logic that can be tested without a display or devices:
  protocol, session sync, layout, annotations, networking/mDNS, GStreamer
  pipeline *descriptions*, level meters, mic channel detection, noise gate,
  control API types. Put new logic here, with unit tests.
- `crates/app` — GTK UI (`ui.rs`), GStreamer runtime (`media.rs`), X11 window
  list/resize (`windows.rs`), PulseAudio routing (`share_audio.rs`), control
  socket (`control.rs`). `tests/media.rs` runs two complete media stacks in one
  process over localhost UDP with test sources.

## Build and test

- `make help` lists everything. `make test`, `make lint` (fmt + clippy with
  `-D warnings`) must pass before you finish.
- `make check` tells you if Rust or the `-dev` packages are missing
  (`make deps` installs them; needs the user's sudo).
- Free disk space is often tight on this machine: check `df -h /` before big
  builds; `target/` grows to several GB.

## Driving the running app: use the control API, never fake input

Do **not** click or type with XTest/xdotool or similar. Synthetic input races
the user's own clicks and lands on their real windows (it once shared and
resized the user's terminal). Use the control API instead (README, "Control
API"); every command runs the same code as the corresponding button:

```sh
S=/tmp/<scratch>/a.sock
setsid ./target/debug/2fcast --config-dir /tmp/<scratch>/a --control $S > a.log 2>&1 < /dev/null & disown
./target/debug/2fcast --ctl $S '{"cmd":"state"}'
./target/debug/2fcast --ctl $S '{"cmd":"windows"}'
./target/debug/2fcast --ctl $S '{"cmd":"share","xid":123}'
```

If something can't be done through the API yet, add a command to
`crates/core/src/control.rs` (protocol + tests) and `App::handle_control` in
`ui.rs` rather than simulating input. Screenshots (`import -window <id>`) are
fine for *looking*.

Two instances on one machine (as "Artem" and "Brother"): separate
`--config-dir` with `base_port = 47200` in the second config, `camera = "test"`,
`microphone = "test"` or `"none"`. They find each other via mDNS.

Be careful with the user's desktop and session:

- Every test instance must use `discovery = false` (and a separate
  `base_port`) unless it is meant to meet another *test* instance: with mDNS
  it finds the user's real running 2fcast (even from inside Docker) and joins
  their session. Also set `XDG_CACHE_HOME` to a scratch dir, or it overwrites
  the user's real log.

- Route test audio away from the speakers:
  `pactl load-module module-null-sink sink_name=quiet_test_sink` and start the
  app with `PULSE_SINK=quiet_test_sink`; unload the module afterwards.
- Share only windows you created for the test (e.g. a small GTK window from
  `python3 -c ...`), never the user's own windows.
- Clean up afterwards: `pkill -x 2fcast`; check
  `pactl list modules short | grep twofcast` is empty (the app also removes
  leftovers on start).
- Start background apps with `setsid … & disown`, otherwise they die with the
  tool's shell. Don't use `pkill -f <pattern>`: it matches your own shell
  command and kills it. The shell is zsh: `$CMD args` does not word-split, so
  put multi-step scripts in a file and run them with `bash`.

- `make lint` only *checks*; run `make build` (or `cargo build`) before
  driving `target/debug/2fcast`, or you test the previous binary.
- Two instances on one machine share one PulseAudio: starting a share clears
  the other instance's share-sound modules. That is a test artifact (two real
  machines each have their own); don't chase it.
- `TWOFCAST_DEBUG=1` logs timestamped events, incl. "program lag" (how far the
  composed picture is behind the clock: must stay well under a second).

## Packaging

- `make deb` builds the self-contained package (`scripts/build-bundle-deb.sh`):
  everything except the libraries matched by `EXCLUDE` is copied into
  `/opt/2fcast/lib`; `Depends` is derived with `dpkg -S` from the excluded
  ones (plus `libgl1`/`libegl1`, which libepoxy loads with dlopen). When adding
  a GStreamer element, add its plugin to `PLUGINS` there and to
  `deps::ELEMENTS`.
- `make docker-test` installs the `.deb` in clean Ubuntu containers and runs
  `2fcast --self-test`. Run it after packaging changes.

## Real-machine problems

The user can run commands on both machines and paste output. Ask for
`~/.cache/2fcast/2fcast.log` from both sides (and `2fcast --self-test`):
per stream it shows "first packet arrived" / "first decoded output". Don't
claim a container run proves anything about the user's real network.

## GStreamer lessons (each cost a debugging session)

- OpenH264 (the bundle's decoder) decodes nothing when the payloader inserts
  SPS/PPS before every keyframe (`rtph264pay config-interval=-1`); use `1`.
  Tests run the camera exchange with both decoders; `--self-test` checks a
  decoded picture comes back, not just that a frame exists.
- Raw queues right after the recorder's sources are leaky: a source blocked
  on a full queue keeps EOS from ever arriving (stop-recording hang).

- The program pipeline must stay real-time: its preview sink syncs with QoS so
  the compositor drops late frames. With only `sync=false` sinks a slow frame
  makes it lag the clock more and more (preview and recording seconds behind
  the sound).
- Never send pipeline-wide events from two threads at once
  (`recalculate_latency` vs. EOS deadlocked); `Pipe::control` serializes them.

- Handle `Latency` bus messages (`recalculate_latency`), or live sinks drop every
  frame from cameras/jitterbuffers as "late".
- Pin all pipelines to the system clock; a pipeline with `pulsesink` otherwise
  follows the sound card clock, which stops while no audio arrives.
- Sinks of independent live branches need `async=false`, or one idle branch
  (speakers with a silent peer) holds the whole pipeline in preroll.
- `inter*` sinks use `sync=false`; each `inter*` channel has one reader only.
- `audioconvert`'s `mix-matrix` ignores changes while playing; sample-level
  processing is done in a pad probe (`install_mic_processing`).
- matroskamux (1.20) mangles track titles with spaces/non-ASCII; see
  `pipelines::mkv_title`.
- LAN H.264: keyframes with the frame size capped by `vbv-buf-capacity`
  (uncapped 1080p keyframes overflow the UDP receive buffer: green blocks).
  A capped keyframe of screen content is much blurrier than the frames before
  it: with one every second the partner's shared screen visibly pulsed in
  recordings.  So the screen gets keyframes on request (`Msg::ScreenKeyframe`,
  `crate::keyframe`); the camera keeps one per second.  Tests passing a screen
  between two `Media` must relay requests (`wait_for_shared`).  Intra
  refresh avoids bursts but late receivers then stay grey for long.
- The idle picture is scaled once (gdk-pixbuf) to its pad's exact size and
  fed through an `intervideosrc` without a fixed size, so the compositor
  never scales it again (sharpness).
