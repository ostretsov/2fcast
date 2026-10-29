//! Shared session state of the two participants and how it is kept in sync.
//!
//! Presenter and layout are last-writer-wins registers ordered by
//! [`Stamp`] `(seq, author id)`, so simultaneous changes converge to the same
//! value on both machines.  Annotations are an append-only list of operations.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::PeerId;
use crate::annotations::{AnnotOp, Annotations};
use crate::layout::Layout;
use crate::protocol::Msg;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Stamp {
    pub seq: u64,
    pub by: PeerId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lww<T> {
    pub value: T,
    pub stamp: Stamp,
}

impl<T: PartialEq> Lww<T> {
    pub fn new(value: T, by: &PeerId) -> Self {
        Lww {
            value,
            stamp: Stamp {
                seq: 0,
                by: by.clone(),
            },
        }
    }

    pub fn set_local(&mut self, value: T, me: &PeerId) -> Stamp {
        self.value = value;
        self.stamp = Stamp {
            seq: self.stamp.seq + 1,
            by: me.clone(),
        };
        self.stamp.clone()
    }

    /// Applies a remote write if it is newer.  Returns true if it was accepted.
    pub fn merge(&mut self, value: T, stamp: Stamp) -> bool {
        if stamp > self.stamp {
            self.value = value;
            self.stamp = stamp;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Peer {
    pub id: PeerId,
    pub name: String,
    pub ip: IpAddr,
    pub base_port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    PeerChanged,
    PresenterChanged,
    LayoutChanged,
    AnnotationsChanged,
    RecordingChanged,
}

/// Result of handling an input: messages to send to the peer and what changed.
#[derive(Default, Debug, PartialEq)]
pub struct Out {
    pub send: Vec<Msg>,
    pub events: Vec<Event>,
}

impl Out {
    fn event(mut self, e: Event) -> Self {
        if !self.events.contains(&e) {
            self.events.push(e);
        }
        self
    }

    fn send(mut self, m: Msg) -> Self {
        self.send.push(m);
        self
    }

    pub fn has(&self, e: Event) -> bool {
        self.events.contains(&e)
    }
}

pub struct Session {
    pub me: PeerId,
    pub my_name: String,
    pub peer: Option<Peer>,
    presenter: Lww<Option<PeerId>>,
    layout: Lww<Layout>,
    pub annotations: Annotations,
    /// True while this machine actually captures and sends its window.
    sharing: bool,
    pub recording: bool,
    pub peer_recording: bool,
}

impl Session {
    pub fn new(me: PeerId, my_name: String, mut layout: Layout) -> Self {
        layout.ensure_cams(std::slice::from_ref(&me));
        Session {
            presenter: Lww::new(None, &me),
            layout: Lww::new(layout, &me),
            me,
            my_name,
            peer: None,
            annotations: Annotations::default(),
            sharing: false,
            recording: false,
            peer_recording: false,
        }
    }

    pub fn layout(&self) -> &Layout {
        &self.layout.value
    }

    /// Ids of everybody in the session (me first).
    pub fn active_ids(&self) -> Vec<PeerId> {
        let mut v = vec![self.me.clone()];
        v.extend(self.peer.as_ref().map(|p| p.id.clone()));
        v
    }

    pub fn peer_id(&self) -> Option<&PeerId> {
        self.peer.as_ref().map(|p| &p.id)
    }

    /// The presenter, if that participant is currently here.
    pub fn presenter(&self) -> Option<&PeerId> {
        match &self.presenter.value {
            Some(p) if *p == self.me || Some(p) == self.peer_id() => Some(p),
            _ => None,
        }
    }

    pub fn is_presenting(&self) -> bool {
        self.presenter() == Some(&self.me)
    }

    pub fn peer_is_presenting(&self) -> bool {
        self.presenter().is_some() && self.presenter() == self.peer_id()
    }

    pub fn display_name(&self, id: &PeerId) -> String {
        if *id == self.me {
            self.my_name.clone()
        } else {
            self.peer
                .as_ref()
                .filter(|p| p.id == *id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| id.clone())
        }
    }

    fn presenter_msg(&self) -> Msg {
        Msg::Presenter {
            presenter: self.presenter.value.clone(),
            stamp: self.presenter.stamp.clone(),
        }
    }

    fn layout_msg(&self) -> Msg {
        Msg::Layout {
            layout: self.layout.value.clone(),
            stamp: self.layout.stamp.clone(),
        }
    }

    // ---- connection lifecycle ----

    pub fn connected(&mut self, peer: Peer) -> Out {
        self.peer = Some(peer);
        let ids = self.active_ids();
        let mut out = Out::default()
            .event(Event::PeerChanged)
            .event(Event::PresenterChanged);
        if self.layout.value.ensure_cams(&ids) {
            out = out.event(Event::LayoutChanged);
        }
        out = out
            .send(self.presenter_msg())
            .send(self.layout_msg())
            .send(Msg::AnnotSync {
                items: self.annotations.items().to_vec(),
            });
        if self.recording {
            out = out.send(Msg::Recording { active: true });
        }
        out
    }

    pub fn disconnected(&mut self) -> Out {
        let was_presenting = self.peer_is_presenting();
        self.peer = None;
        let mut out = Out::default().event(Event::PeerChanged);
        if was_presenting {
            out = out.event(Event::PresenterChanged);
        }
        if std::mem::take(&mut self.peer_recording) {
            out = out.event(Event::RecordingChanged);
        }
        out
    }

    // ---- local actions ----

    pub fn start_presenting(&mut self) -> Out {
        self.sharing = true;
        let stamp = self.presenter.set_local(Some(self.me.clone()), &self.me);
        self.annotations.clear();
        Out::default()
            .event(Event::PresenterChanged)
            .event(Event::AnnotationsChanged)
            .send(Msg::Presenter {
                presenter: Some(self.me.clone()),
                stamp,
            })
    }

    pub fn stop_presenting(&mut self) -> Out {
        self.sharing = false;
        if self.presenter.value.as_ref() != Some(&self.me) {
            return Out::default();
        }
        let stamp = self.presenter.set_local(None, &self.me);
        self.annotations.clear();
        Out::default()
            .event(Event::PresenterChanged)
            .event(Event::AnnotationsChanged)
            .send(Msg::Presenter {
                presenter: None,
                stamp,
            })
    }

    pub fn annotate(&mut self, op: AnnotOp) -> Out {
        self.annotations.apply(&op);
        Out::default()
            .event(Event::AnnotationsChanged)
            .send(Msg::Annot { op })
    }

    pub fn set_layout(&mut self, layout: Layout) -> Out {
        if layout == self.layout.value {
            return Out::default();
        }
        self.layout.set_local(layout, &self.me);
        Out::default()
            .event(Event::LayoutChanged)
            .send(self.layout_msg())
    }

    pub fn set_recording(&mut self, active: bool) -> Out {
        self.recording = active;
        Out::default()
            .event(Event::RecordingChanged)
            .send(Msg::Recording { active })
    }

    // ---- remote messages ----

    pub fn handle(&mut self, msg: Msg) -> Out {
        match msg {
            Msg::Presenter { presenter, stamp } => {
                let before = self.presenter.value.clone();
                if !self.presenter.merge(presenter, stamp) {
                    return Out::default();
                }
                let mut out = Out::default();
                if self.presenter.value.as_ref() == Some(&self.me) && !self.sharing {
                    // Stale state from before a restart: we are not sharing anything.
                    let stamp = self.presenter.set_local(None, &self.me);
                    out = out.send(Msg::Presenter {
                        presenter: None,
                        stamp,
                    });
                }
                if self.presenter.value.as_ref() != Some(&self.me) {
                    self.sharing = false;
                }
                if self.presenter.value != before {
                    self.annotations.clear();
                    out = out
                        .event(Event::PresenterChanged)
                        .event(Event::AnnotationsChanged);
                }
                out
            }
            Msg::Layout { layout, stamp } => {
                if !self.layout.merge(layout, stamp) {
                    return Out::default();
                }
                let ids = self.active_ids();
                self.layout.value.ensure_cams(&ids);
                Out::default().event(Event::LayoutChanged)
            }
            Msg::Annot { op } => {
                self.annotations.apply(&op);
                Out::default().event(Event::AnnotationsChanged)
            }
            Msg::AnnotSync { items } => {
                self.annotations.merge(&items);
                Out::default().event(Event::AnnotationsChanged)
            }
            Msg::Recording { active } => {
                self.peer_recording = active;
                Out::default().event(Event::RecordingChanged)
            }
            Msg::Hello { .. } | Msg::Ping => Out::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotations::Pt;
    use crate::layout::{Preset, TileId};

    fn peer(id: &str) -> Peer {
        Peer {
            id: id.into(),
            name: id.to_uppercase(),
            ip: "127.0.0.1".parse().unwrap(),
            base_port: 1,
        }
    }

    /// Two sessions wired together: delivers all messages until quiet.
    struct Pair {
        a: Session,
        b: Session,
    }

    impl Pair {
        fn new() -> Pair {
            let mut p = Pair {
                a: Session::new("a".into(), "A".into(), Layout::default()),
                b: Session::new("b".into(), "B".into(), Layout::default()),
            };
            let oa = p.a.connected(peer("b"));
            let ob = p.b.connected(peer("a"));
            p.deliver(oa.send, ob.send);
            p
        }

        /// Delivers messages sent by A and by B until nothing more is sent.
        fn deliver(&mut self, mut to_b: Vec<Msg>, mut to_a: Vec<Msg>) {
            while !to_a.is_empty() || !to_b.is_empty() {
                let mut next_a = vec![];
                let mut next_b = vec![];
                for m in to_b.drain(..) {
                    next_a.extend(self.b.handle(m).send);
                }
                for m in to_a.drain(..) {
                    next_b.extend(self.a.handle(m).send);
                }
                to_a = next_a;
                to_b = next_b;
            }
        }

        fn converged(&self) -> bool {
            self.a.presenter() == self.b.presenter()
                && self.a.layout() == self.b.layout()
                && self.a.annotations == self.b.annotations
        }
    }

    #[test]
    fn lww_orders_by_seq_then_author() {
        let mut r = Lww::new(0, &"a".into());
        assert!(!r.merge(
            1,
            Stamp {
                seq: 0,
                by: "a".into()
            }
        ));
        assert!(r.merge(
            1,
            Stamp {
                seq: 0,
                by: "b".into()
            }
        ));
        assert!(r.merge(
            2,
            Stamp {
                seq: 1,
                by: "a".into()
            }
        ));
        assert!(!r.merge(
            3,
            Stamp {
                seq: 1,
                by: "a".into()
            }
        ));
        assert_eq!(r.set_local(9, &"a".into()).seq, 2);
    }

    #[test]
    fn connect_adds_both_cameras_and_converges() {
        let p = Pair::new();
        assert!(p.converged());
        assert!(p.a.layout().cams.contains_key("a") && p.a.layout().cams.contains_key("b"));
        assert_ne!(
            p.a.layout().cams["a"].rect,
            p.a.layout().cams["b"].rect,
            "cameras do not overlap"
        );
        assert_eq!(p.a.display_name(&"b".into()), "B");
    }

    #[test]
    fn presenting_switches_between_peers() {
        let mut p = Pair::new();
        let out = p.a.start_presenting();
        assert!(out.has(Event::PresenterChanged));
        p.deliver(out.send, vec![]);
        assert!(p.a.is_presenting() && p.b.peer_is_presenting());

        // B takes over: A must stop sharing.
        let out = p.b.start_presenting();
        let ev = p.a.handle(out.send[0].clone());
        assert!(ev.has(Event::PresenterChanged));
        assert!(!p.a.is_presenting() && p.a.peer_is_presenting());
        assert!(p.converged());

        let out = p.b.stop_presenting();
        p.deliver(vec![], out.send);
        assert_eq!(p.a.presenter(), None);
        assert!(p.converged());
    }

    #[test]
    fn stopping_when_not_presenter_is_noop() {
        let mut p = Pair::new();
        let out = p.b.start_presenting();
        p.deliver(vec![], out.send);
        assert_eq!(p.a.stop_presenting(), Out::default());
        assert!(p.a.peer_is_presenting());
    }

    #[test]
    fn simultaneous_start_converges_to_one_presenter() {
        let mut p = Pair::new();
        let oa = p.a.start_presenting();
        let ob = p.b.start_presenting();
        p.deliver(oa.send, ob.send);
        assert!(p.converged());
        let presenter = p.a.presenter().cloned().unwrap();
        assert_eq!(presenter, "b", "higher id wins a tie");
        assert!(!p.a.is_presenting() && p.b.is_presenting());
    }

    #[test]
    fn presenter_change_clears_annotations() {
        let mut p = Pair::new();
        let o = p.a.start_presenting();
        p.deliver(o.send, vec![]);
        let o = p.b.annotate(AnnotOp::StrokeBegin {
            id: 1,
            color: [1, 2, 3],
            width: 0.01,
            at: Pt { x: 0.5, y: 0.5 },
        });
        p.deliver(vec![], o.send);
        assert_eq!(p.a.annotations.items().len(), 1);
        let o = p.b.start_presenting();
        p.deliver(vec![], o.send);
        assert!(p.a.annotations.is_empty() && p.b.annotations.is_empty());
    }

    #[test]
    fn layout_changes_sync_and_simultaneous_edits_converge() {
        let mut p = Pair::new();
        let mut l = p.a.layout().clone();
        l.move_by(&TileId::Cam("a".into()), -100.0, 0.0);
        let o = p.a.set_layout(l.clone());
        p.deliver(o.send, vec![]);
        assert_eq!(p.b.layout(), &l);

        let oa =
            p.a.set_layout(Layout::preset(Preset::CamsOnly, &p.a.active_ids()));
        let ob =
            p.b.set_layout(Layout::preset(Preset::ScreenOnly, &p.b.active_ids()));
        p.deliver(oa.send, ob.send);
        assert!(p.converged());
        assert_eq!(
            p.a.set_layout(p.a.layout().clone()),
            Out::default(),
            "unchanged layout is not sent"
        );
    }

    #[test]
    fn disconnect_hides_remote_presenter_and_reconnect_restores() {
        let mut p = Pair::new();
        let o = p.b.start_presenting();
        p.deliver(vec![], o.send);
        let o = p.a.disconnected();
        assert!(o.has(Event::PresenterChanged));
        assert_eq!(p.a.presenter(), None);
        let oa = p.a.connected(peer("b"));
        let ob = p.b.connected(peer("a"));
        p.deliver(oa.send, ob.send);
        assert!(p.a.peer_is_presenting());
    }

    #[test]
    fn restarted_peer_drops_stale_presenter_claim() {
        let mut p = Pair::new();
        let o = p.b.start_presenting();
        p.deliver(vec![], o.send);
        // B restarts: fresh session, not sharing anything.
        p.a.disconnected();
        p.b = Session::new("b".into(), "B".into(), Layout::default());
        let oa = p.a.connected(peer("b"));
        let ob = p.b.connected(peer("a"));
        p.deliver(oa.send, ob.send);
        assert_eq!(p.a.presenter(), None);
        assert_eq!(p.b.presenter(), None);
        assert!(p.converged());
    }

    #[test]
    fn annotations_resync_after_reconnect() {
        let mut p = Pair::new();
        let o = p.a.start_presenting();
        p.deliver(o.send, vec![]);
        p.a.disconnected();
        p.b.disconnected();
        p.a.annotate(AnnotOp::Text {
            id: 5,
            color: [1, 1, 1],
            size: 0.05,
            at: Pt { x: 0.1, y: 0.1 },
            text: "x".into(),
        });
        let oa = p.a.connected(peer("b"));
        let ob = p.b.connected(peer("a"));
        p.deliver(oa.send, ob.send);
        assert_eq!(p.b.annotations.items().len(), 1);
    }

    #[test]
    fn recording_state_is_shared() {
        let mut p = Pair::new();
        let o = p.a.set_recording(true);
        p.deliver(o.send, vec![]);
        assert!(p.b.peer_recording && p.a.recording);
        let o = p.b.disconnected();
        assert!(o.has(Event::RecordingChanged) && !p.b.peer_recording);
        // A reconnecting while recording tells B again.
        let oa = p.a.connected(peer("b"));
        p.b.connected(peer("a"));
        p.deliver(oa.send, vec![]);
        assert!(p.b.peer_recording);
    }
}
