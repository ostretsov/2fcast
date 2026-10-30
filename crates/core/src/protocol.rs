//! Control messages exchanged over the TCP connection between the two peers.
//! Framing: one JSON object per line.

use serde::{Deserialize, Serialize};

use crate::PeerId;
use crate::annotations::{AnnotOp, Item};
use crate::layout::Layout;
use crate::session::Stamp;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Msg {
    /// First message on every connection.
    Hello {
        id: PeerId,
        name: String,
        base_port: u16,
        version: u32,
        /// How long the sender has been running: whoever runs longer joined
        /// first (decides the default drawing colors).
        #[serde(default)]
        uptime_ms: u64,
    },
    /// Who owns the shared space (last writer wins by stamp).
    Presenter {
        presenter: Option<PeerId>,
        stamp: Stamp,
    },
    /// Shared layout of the recording canvas (last writer wins by stamp).
    Layout { layout: Layout, stamp: Stamp },
    /// One drawing operation on the shared space.
    Annot { op: AnnotOp },
    /// Full set of annotations, sent after connecting.
    AnnotSync { items: Vec<Item> },
    /// Sender started or stopped recording.
    Recording { active: bool },
    /// Whether the sender receives our camera/voice (false: packets do not
    /// arrive, usually a firewall on the sender's side).
    MediaReceived { ok: bool },
    /// Keep-alive.
    Ping,
}

pub const PROTOCOL_VERSION: u32 = 1;

pub fn encode(msg: &Msg) -> String {
    let mut s = serde_json::to_string(msg).expect("Msg is always serializable");
    s.push('\n');
    s
}

pub fn decode(line: &str) -> anyhow::Result<Msg> {
    Ok(serde_json::from_str(line.trim_end())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotations::Pt;
    use crate::layout::{Layout, Preset};

    fn roundtrip(m: Msg) {
        let line = encode(&m);
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1, "must be a single line");
        assert_eq!(decode(&line).unwrap(), m);
    }

    #[test]
    fn all_messages_roundtrip() {
        let stamp = Stamp {
            seq: 3,
            by: "a".into(),
        };
        roundtrip(Msg::Hello {
            id: "a".into(),
            name: "Artem\nX".into(),
            base_port: 47100,
            version: 1,
            uptime_ms: 5,
        });
        roundtrip(Msg::Presenter {
            presenter: Some("b".into()),
            stamp: stamp.clone(),
        });
        roundtrip(Msg::Presenter {
            presenter: None,
            stamp: stamp.clone(),
        });
        roundtrip(Msg::Layout {
            layout: Layout::preset(Preset::ScreenCamsRight, &["a".into(), "b".into()]),
            stamp,
        });
        roundtrip(Msg::Annot {
            op: AnnotOp::StrokeTo {
                id: 7,
                pts: vec![Pt { x: 0.5, y: 0.25 }],
            },
        });
        roundtrip(Msg::AnnotSync { items: vec![] });
        roundtrip(Msg::Recording { active: true });
        roundtrip(Msg::MediaReceived { ok: false });
        roundtrip(Msg::Ping);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode("not json").is_err());
        assert!(decode(r#"{"type":"unknown"}"#).is_err());
    }

    #[test]
    fn wire_format_is_tagged() {
        assert_eq!(encode(&Msg::Ping), "{\"type\":\"ping\"}\n");
    }
}
