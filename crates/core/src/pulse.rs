//! Parsing `pactl` output, used to route the shared window's sound.
//!
//! When someone shares a window, the sound of that application (e.g. a YouTube
//! video in the browser) is moved into a private null sink `twofcast_share`.
//! Its monitor is captured and sent to the peer / recorded, and a loopback
//! plays it on the presenter's speakers.  Our own playback of the peer's voice
//! never goes there, so it is not echoed back.

use std::collections::HashMap;

pub const SHARE_SINK: &str = "twofcast_share";

#[derive(Clone, Debug, PartialEq, Default)]
pub struct SinkInput {
    pub index: u32,
    pub sink: u32,
    pub owner_module: Option<u32>,
    pub props: HashMap<String, String>,
}

impl SinkInput {
    fn prop(&self, k: &str) -> Option<&str> {
        self.props.get(k).map(String::as_str)
    }
}

/// Parses `pactl list sink-inputs`.
pub fn parse_sink_inputs(text: &str) -> Vec<SinkInput> {
    let mut out = vec![];
    let mut cur: Option<SinkInput> = None;
    let mut in_props = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Sink Input #") {
            out.extend(cur.take());
            cur = rest.trim().parse().ok().map(|index| SinkInput {
                index,
                ..Default::default()
            });
            in_props = false;
            continue;
        }
        let Some(c) = cur.as_mut() else { continue };
        let t = line.trim();
        if let Some(m) = t.strip_prefix("Owner Module:") {
            c.owner_module = m.trim().parse().ok();
            in_props = false;
        } else if let Some(s) = t.strip_prefix("Sink:") {
            c.sink = s.trim().parse().unwrap_or(u32::MAX);
            in_props = false;
        } else if t == "Properties:" {
            in_props = true;
        } else if in_props && let Some((k, v)) = t.split_once(" = ") {
            c.props
                .insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        } else if !line.starts_with("\t\t") && !line.starts_with("        ") {
            in_props = false;
        }
    }
    out.extend(cur);
    out
}

/// One line of `pactl list modules short` / `pactl list sinks short`.
#[derive(Clone, Debug, PartialEq)]
pub struct Short {
    pub index: u32,
    pub name: String,
    pub rest: String,
}

pub fn parse_short(text: &str) -> Vec<Short> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            let index = it.next()?.trim().parse().ok()?;
            let name = it.next()?.to_string();
            Some(Short {
                index,
                name,
                rest: it.next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// Modules we loaded earlier (e.g. before a crash) that should be unloaded.
pub fn our_modules(modules: &[Short]) -> Vec<u32> {
    modules
        .iter()
        .filter(|m| m.rest.contains(SHARE_SINK))
        .map(|m| m.index)
        .collect()
}

pub fn sink_index(sinks: &[Short], name: &str) -> Option<u32> {
    sinks.iter().find(|s| s.name == name).map(|s| s.index)
}

/// Whose sound to share.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// The application owning the shared window.
    App { pid: u32, binary: String },
    /// Entire screen: every application except us.
    Everything,
}

/// Process names as the kernel reports them (`/proc/<pid>/comm`) are cut at
/// 15 characters and some apps add "-bin"; compare loosely.
pub fn same_binary(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> String {
        let s = s.rsplit('/').next().unwrap_or(s);
        let s = s.strip_suffix("-bin").unwrap_or(s);
        s.chars().take(15).collect()
    }
    !a.is_empty() && !b.is_empty() && norm(a) == norm(b)
}

/// Sink inputs that should be moved into the share sink.  Streams owned by
/// `our_modules` (the loopback) are never moved, that would loop the sound.
pub fn inputs_to_move(
    inputs: &[SinkInput],
    target: &Target,
    own_pid: u32,
    share_sink: u32,
    our_modules: &[u32],
) -> Vec<u32> {
    inputs
        .iter()
        .filter(|i| i.sink != share_sink)
        .filter(|i| !i.owner_module.is_some_and(|m| our_modules.contains(&m)))
        .filter(|i| i.prop("application.process.id") != Some(own_pid.to_string().as_str()))
        .filter(|i| match target {
            Target::Everything => true,
            Target::App { pid, binary } => {
                i.prop("application.process.id") == Some(pid.to_string().as_str())
                    || i.prop("application.process.binary")
                        .is_some_and(|b| same_binary(b, binary))
            }
        })
        .map(|i| i.index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Sink Input #71
\tDriver: protocol-native.c
\tOwner Module: n/a
\tClient: 70
\tSink: 56
\tSample Specification: float32le 2ch 48000Hz
\tChannel Map: front-left,front-right
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
\t        balance 0.00
\tProperties:
\t\tclient.api = \"pipewire-pulse\"
\t\tapplication.name = \"Firefox\"
\t\tapplication.process.id = \"4242\"
\t\tapplication.process.binary = \"firefox\"
\t\tmedia.name = \"AudioStream\"

Sink Input #80
\tDriver: protocol-native.c
\tSink: 56
\tProperties:
\t\tapplication.name = \"2fcast\"
\t\tapplication.process.id = \"999\"
\t\tapplication.process.binary = \"2fcast\"

Sink Input #81
\tSink: 90
\tProperties:
\t\tapplication.process.id = \"5000\"
\t\tapplication.process.binary = \"chrome\"
";

    #[test]
    fn parses_sink_inputs() {
        let v = parse_sink_inputs(SAMPLE);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].index, 71);
        assert_eq!(v[0].sink, 56);
        assert_eq!(v[0].prop("application.name"), Some("Firefox"));
        assert_eq!(v[0].prop("media.name"), Some("AudioStream"));
        assert_eq!(v[0].prop("Volume"), None);
        assert_eq!(v[2].sink, 90);
        assert!(parse_sink_inputs("").is_empty());
    }

    #[test]
    fn selects_inputs_of_target_app() {
        let v = parse_sink_inputs(SAMPLE);
        let ff = Target::App {
            pid: 1,
            binary: "firefox".into(),
        };
        assert_eq!(
            inputs_to_move(&v, &ff, 999, 90, &[]),
            vec![71],
            "matched by binary"
        );
        let by_pid = Target::App {
            pid: 5000,
            binary: String::new(),
        };
        assert_eq!(inputs_to_move(&v, &by_pid, 999, 1, &[]), vec![81]);
        assert!(
            inputs_to_move(&v, &by_pid, 999, 90, &[]).is_empty(),
            "already moved"
        );
        assert_eq!(
            inputs_to_move(&v, &Target::Everything, 999, 90, &[]),
            vec![71],
            "never our own playback"
        );
    }

    /// Real output of a module-loopback stream under PipeWire 0.3.48.
    const LOOPBACK: &str = "Sink Input #897
\tDriver: PipeWire
\tOwner Module: 536870914
\tClient: n/a
\tSink: 50
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
\t        balance 0.00
\tProperties:
\t\tnode.group = \"loopback-536870914\"
\t\tpulse.module.id = \"536870914\"
\t\tmedia.name = \"loopback-12 output\"
";

    #[test]
    fn never_moves_our_loopback() {
        let v = parse_sink_inputs(LOOPBACK);
        assert_eq!(v[0].owner_module, Some(536870914));
        assert_eq!(v[0].sink, 50);
        assert!(
            inputs_to_move(&v, &Target::Everything, 1, 889, &[536870913, 536870914]).is_empty()
        );
        assert_eq!(
            inputs_to_move(&v, &Target::Everything, 1, 889, &[]),
            vec![897]
        );
    }

    #[test]
    fn binary_matching() {
        assert!(same_binary("firefox-bin", "firefox"));
        assert!(same_binary(
            "/usr/lib/chromium-browser/chromium-browser",
            "chromium-browse"
        ));
        assert!(!same_binary("", ""));
        assert!(!same_binary("chrome", "firefox"));
    }

    #[test]
    fn parses_short_lists() {
        let mods = "1\tlibpipewire-module-rt\t{\n536870913\tmodule-null-sink\tsink_name=twofcast_share sink_properties=x\n536870914\tmodule-loopback\tsource=twofcast_share.monitor\n";
        let m = parse_short(mods);
        assert_eq!(our_modules(&m), vec![536870913, 536870914]);
        let sinks = "56\talsa_output.usb\tPipeWire\ts32le 4ch 48000Hz\tRUNNING\n90\ttwofcast_share\tPipeWire\tfloat32le 2ch 48000Hz\tIDLE\n";
        assert_eq!(sink_index(&parse_short(sinks), SHARE_SINK), Some(90));
        assert_eq!(sink_index(&parse_short(sinks), "nope"), None);
    }
}
