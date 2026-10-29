//! Routes the sound of the shared application into a private PulseAudio sink
//! so it can be captured without also capturing our own playback of the
//! peer's voice.  See [`twofcast_core::pulse`].

use std::process::Command;

use anyhow::{Context, bail};
use twofcast_core::pulse::{
    SHARE_SINK, Target, inputs_to_move, our_modules, parse_short, parse_sink_inputs, sink_index,
};

fn pactl(args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("pactl")
        .args(args)
        .output()
        .context("running pactl")?;
    if !out.status.success() {
        bail!(
            "pactl {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Unloads modules left over by a previous run (e.g. after a crash).
pub fn cleanup_stale() {
    if let Ok(text) = pactl(&["list", "modules", "short"]) {
        for m in our_modules(&parse_short(&text)) {
            let _ = pactl(&["unload-module", &m.to_string()]);
        }
    }
}

pub struct ShareAudio {
    target: Target,
    modules: Vec<u32>,
    sink: u32,
}

impl ShareAudio {
    pub fn start(target: Target) -> anyhow::Result<ShareAudio> {
        cleanup_stale();
        let mut s = ShareAudio {
            target,
            modules: vec![],
            sink: u32::MAX,
        };
        let null = format!("sink_name={SHARE_SINK}");
        s.modules.push(
            pactl(&[
                "load-module",
                "module-null-sink",
                &null,
                "sink_properties=device.description=2fcast-shared-sound",
            ])?
            .trim()
            .parse()?,
        );
        let source = format!("source={SHARE_SINK}.monitor");
        s.modules.push(
            pactl(&["load-module", "module-loopback", &source, "latency_msec=30"])?
                .trim()
                .parse()?,
        );
        s.sink = sink_index(
            &parse_short(&pactl(&["list", "sinks", "short"])?),
            SHARE_SINK,
        )
        .context("share sink did not appear")?;
        s.poll();
        Ok(s)
    }

    pub fn monitor_source() -> String {
        format!("{SHARE_SINK}.monitor")
    }

    /// Moves newly started streams of the shared app (call periodically).
    pub fn poll(&self) {
        let Ok(text) = pactl(&["list", "sink-inputs"]) else {
            return;
        };
        for index in inputs_to_move(
            &parse_sink_inputs(&text),
            &self.target,
            std::process::id(),
            self.sink,
            &self.modules,
        ) {
            let _ = pactl(&["move-sink-input", &index.to_string(), SHARE_SINK]);
        }
    }
}

impl Drop for ShareAudio {
    fn drop(&mut self) {
        // Streams in the removed sink fall back to the default output.
        for m in self.modules.iter().rev() {
            let _ = pactl(&["unload-module", &m.to_string()]);
        }
    }
}
