//! Local control API: JSON commands, one per line, over a Unix socket that
//! only the user can open (`2fcast --control PATH`).  Every command runs the
//! same code as the corresponding button, so scripts and tests drive the real
//! app without simulating mouse clicks.  Each reply carries the app state.
//!
//! ```text
//! {"cmd":"state"}
//! {"cmd":"windows"}
//! {"cmd":"share","xid":12345}        // omit xid for the entire screen
//! {"cmd":"stop_sharing"}
//! {"cmd":"record","on":true}
//! {"cmd":"mute","on":true}
//! {"cmd":"hide_self_view","on":true}  // my camera hidden on my screen only
//! {"cmd":"idle_image","path":"/home/me/logo.png"}  // omit path: black again
//! {"cmd":"preset","name":"cams_only"}
//! {"cmd":"draw","points":[[0.1,0.1],[0.5,0.5]]}
//! {"cmd":"text","x":0.1,"y":0.2,"text":"hello"}
//! {"cmd":"arrow","from":[0.1,0.1],"to":[0.5,0.5],"width":0.01}
//! {"cmd":"arrow","from":[0.1,0.1],"to":[0.5,0.5],"shape":"rect"}  // or "line"
//! {"cmd":"ping","x":0.5,"y":0.5}
//! {"cmd":"undo"}
//! {"cmd":"clear"}
//! ```

use serde::{Deserialize, Serialize};

use crate::annotations::Shape;
use crate::layout::{Layout, Preset};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    State,
    /// Windows that can be shared.
    Windows,
    /// Share a window (`xid`) or, without one, the entire screen.
    Share {
        #[serde(default)]
        xid: Option<u32>,
    },
    StopSharing,
    Record {
        on: bool,
    },
    Mute {
        on: bool,
    },
    /// Covers my own camera on my screen; the partner and the recording
    /// still get it.
    HideSelfView {
        on: bool,
    },
    Preset {
        name: String,
    },
    /// Picture shown in the shared space while nobody shares (remembered);
    /// without `path`, none (black).
    IdleImage {
        #[serde(default)]
        path: Option<String>,
    },
    /// A stroke on the shared space, points normalized to 0..1.
    Draw {
        points: Vec<(f32, f32)>,
    },
    Text {
        x: f32,
        y: f32,
        text: String,
    },
    /// An arrow (or a line or rectangle with `shape`); `width` is a fraction
    /// of the shared-space height.
    Arrow {
        from: (f32, f32),
        to: (f32, f32),
        #[serde(default)]
        width: Option<f32>,
        #[serde(default)]
        shape: Option<Shape>,
    },
    /// Ripples at a point of the shared space ("look here").
    Ping {
        x: f32,
        y: f32,
    },
    /// Removes my last drawing (like Ctrl+Z).
    Undo,
    Clear,
}

pub fn parse_preset(name: &str) -> Option<Preset> {
    match name {
        "screen_cams_right" => Some(Preset::ScreenCamsRight),
        "screen_cams_overlay" => Some(Preset::ScreenCamsOverlay),
        "cams_only" => Some(Preset::CamsOnly),
        "screen_only" => Some(Preset::ScreenOnly),
        _ => None,
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct WindowEntry {
    pub xid: u32,
    pub title: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SharedWindow {
    pub xid: u32,
    /// Size before it was shrunk for sharing (restored afterwards).
    pub original_width: u32,
    pub original_height: u32,
    pub was_maximized: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct State {
    pub me: String,
    pub peer: Option<String>,
    /// Name of whoever shares the screen.
    pub presenter: Option<String>,
    pub presenting: bool,
    pub shared_window: Option<SharedWindow>,
    pub recording: bool,
    pub peer_recording: bool,
    /// How long this machine / the peer has been recording.
    #[serde(default)]
    pub recording_secs: Option<u64>,
    #[serde(default)]
    pub peer_recording_secs: Option<u64>,
    /// Size of the file being recorded here.
    #[serde(default)]
    pub recording_file_bytes: Option<u64>,
    /// Free space on the disk recordings go to (also when not recording).
    #[serde(default)]
    pub disk_free_bytes: Option<u64>,
    pub muted: bool,
    #[serde(default)]
    pub self_view_hidden: bool,
    /// Picture shown while nobody shares, and how sharp it looks.
    #[serde(default)]
    pub idle_image: Option<String>,
    #[serde(default)]
    pub idle_image_check: String,
    /// "Both", "Left" or "Right": microphone input channel(s) in use.
    pub mic_channel: Option<String>,
    pub my_level_db: Option<f64>,
    pub peer_level_db: Option<f64>,
    pub noise_gate_db: Option<f64>,
    pub annotations: usize,
    pub layout: Option<Layout>,
    /// Last error or status message shown in the window.
    pub message: String,
    /// Whether the peer's camera/voice packets arrive here.
    pub receiving_from_peer: Option<bool>,
    /// What the peer reports about receiving ours.
    pub peer_receives_us: Option<bool>,
    /// Connection warning shown in the window (empty if none).
    pub warning: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<WindowEntry>,
    pub state: State,
}

impl Reply {
    pub fn ok(state: State) -> Reply {
        Reply {
            ok: true,
            state,
            ..Default::default()
        }
    }

    pub fn error(msg: impl Into<String>, state: State) -> Reply {
        Reply {
            ok: false,
            error: Some(msg.into()),
            state,
            ..Default::default()
        }
    }
}

pub fn parse(line: &str) -> Result<Command, String> {
    serde_json::from_str(line.trim()).map_err(|e| format!("bad command: {e}"))
}

pub fn encode(reply: &Reply) -> String {
    let mut s = serde_json::to_string(reply).expect("Reply is serializable");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(parse(r#"{"cmd":"state"}"#), Ok(Command::State));
        assert_eq!(
            parse(r#"{"cmd":"share"}"#),
            Ok(Command::Share { xid: None })
        );
        assert_eq!(
            parse(r#"{"cmd":"share","xid":42}"#),
            Ok(Command::Share { xid: Some(42) })
        );
        assert_eq!(
            parse(r#" {"cmd":"record","on":true} "#),
            Ok(Command::Record { on: true })
        );
        assert_eq!(
            parse(r#"{"cmd":"hide_self_view","on":false}"#),
            Ok(Command::HideSelfView { on: false })
        );
        assert_eq!(
            parse(r#"{"cmd":"idle_image","path":"/tmp/a b.png"}"#),
            Ok(Command::IdleImage {
                path: Some("/tmp/a b.png".into())
            })
        );
        assert_eq!(
            parse(r#"{"cmd":"idle_image"}"#),
            Ok(Command::IdleImage { path: None })
        );
        assert_eq!(
            parse(r#"{"cmd":"draw","points":[[0.1,0.2],[0.3,0.4]]}"#),
            Ok(Command::Draw {
                points: vec![(0.1, 0.2), (0.3, 0.4)]
            })
        );
        assert_eq!(
            parse(r#"{"cmd":"arrow","from":[0.1,0.2],"to":[0.3,0.4]}"#),
            Ok(Command::Arrow {
                from: (0.1, 0.2),
                to: (0.3, 0.4),
                width: None,
                shape: None,
            })
        );
        assert_eq!(
            parse(r#"{"cmd":"arrow","from":[0,0],"to":[1,1],"shape":"rect"}"#),
            Ok(Command::Arrow {
                from: (0.0, 0.0),
                to: (1.0, 1.0),
                width: None,
                shape: Some(Shape::Rect),
            })
        );
        assert_eq!(parse(r#"{"cmd":"undo"}"#), Ok(Command::Undo));
        assert_eq!(
            parse(r#"{"cmd":"ping","x":0.5,"y":0.25}"#),
            Ok(Command::Ping { x: 0.5, y: 0.25 })
        );
        assert!(parse(r#"{"cmd":"explode"}"#).is_err());
        assert!(parse("not json").unwrap_err().starts_with("bad command"));
    }

    #[test]
    fn presets_by_name() {
        assert_eq!(parse_preset("cams_only"), Some(Preset::CamsOnly));
        assert_eq!(
            parse_preset("screen_cams_right"),
            Some(Preset::ScreenCamsRight)
        );
        assert_eq!(parse_preset("nope"), None);
    }

    #[test]
    fn replies_are_single_json_lines() {
        let r = Reply::error(
            "no such window",
            State {
                me: "Artem".into(),
                ..Default::default()
            },
        );
        let line = encode(&r);
        assert!(line.ends_with('\n') && line.matches('\n').count() == 1);
        let back: Reply = serde_json::from_str(&line).unwrap();
        assert_eq!(back, r);
        assert!(!line.contains("\"windows\""), "empty window list omitted");
    }
}
