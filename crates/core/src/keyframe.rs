//! Keyframes of the shared screen on request.
//!
//! The presenter's screen stream has no regular keyframes: on screen content
//! (sharp text, a mostly still picture) a keyframe squeezed into the LAN
//! frame-size cap is much blurrier than the frames before it, so a keyframe
//! every second made the shared screen visibly pulse once a second (in the
//! partner's preview and recording).  Instead the receiver asks for one
//! ([`crate::protocol::Msg::ScreenKeyframe`]) when it starts receiving and
//! when packets were lost, and keeps asking until one arrives.

use std::time::{Duration, Instant};

/// How often to repeat an unanswered request.
pub const RETRY: Duration = Duration::from_millis(500);

/// Receiver side: whether (and when) to ask the presenter for a keyframe.
#[derive(Debug, Default)]
pub struct Requests {
    wanted: bool,
    last_request: Option<Instant>,
}

impl Requests {
    /// A keyframe is needed: the stream (re)started or packets were lost.
    pub fn want(&mut self) {
        self.wanted = true;
    }

    /// A keyframe arrived.
    pub fn got(&mut self) {
        self.wanted = false;
    }

    pub fn wanted(&self) -> bool {
        self.wanted
    }

    /// Whether to send a request now; at most one per [`RETRY`].
    pub fn due(&mut self, now: Instant) -> bool {
        if !self.wanted
            || self
                .last_request
                .is_some_and(|t| now.saturating_duration_since(t) < RETRY)
        {
            return false;
        }
        self.last_request = Some(now);
        true
    }
}

/// Receiver side: holds the last good picture while packets are missing.
///
/// After a lost RTP packet the decoder would conceal the gap by smearing
/// blocks of older pictures (grey blocks, ghost text) until the next
/// keyframe.  Instead every frame from the loss on is dropped before the
/// decoder, so the previous complete picture stays on screen until a
/// keyframe arrives.  The stream starts frozen: frames before the first
/// keyframe cannot be decoded cleanly either.
#[derive(Debug)]
pub struct Freeze {
    next_seq: Option<u16>,
    frozen: bool,
}

impl Default for Freeze {
    fn default() -> Self {
        Self {
            next_seq: None,
            frozen: true,
        }
    }
}

impl Freeze {
    /// An RTP packet with sequence number `seq` arrived (in jitterbuffer
    /// order).  Returns true when packets before it were lost and the
    /// picture just froze.
    pub fn packet(&mut self, seq: u16) -> bool {
        let gap = self.next_seq.is_some_and(|n| n != seq);
        self.next_seq = Some(seq.wrapping_add(1));
        let froze = gap && !self.frozen;
        self.frozen |= gap;
        froze
    }

    /// A frame leaves the depayloader; returns whether to decode it.
    /// A keyframe ends the freeze.
    pub fn pass(&mut self, keyframe: bool) -> bool {
        if keyframe {
            self.frozen = false;
        }
        !self.frozen
    }

    pub fn frozen(&self) -> bool {
        self.frozen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_once_then_repeats_until_a_keyframe_arrives() {
        let t0 = Instant::now();
        let mut r = Requests::default();
        assert!(!r.due(t0), "nothing wanted yet");
        r.want();
        assert!(r.due(t0));
        assert!(!r.due(t0 + Duration::from_millis(100)), "not again so soon");
        assert!(r.due(t0 + RETRY), "unanswered: ask again");
        r.got();
        assert!(!r.due(t0 + RETRY * 3));
    }

    #[test]
    fn loss_right_after_a_request_waits_for_the_retry_time() {
        let t0 = Instant::now();
        let mut r = Requests::default();
        r.want();
        assert!(r.due(t0));
        r.got();
        r.want();
        assert!(!r.due(t0 + Duration::from_millis(10)));
        assert!(r.due(t0 + RETRY));
    }

    #[test]
    fn frozen_until_the_first_keyframe() {
        let mut f = Freeze::default();
        assert!(!f.packet(7));
        assert!(!f.pass(false), "a delta frame before any keyframe");
        assert!(f.pass(true));
        assert!(f.pass(false));
    }

    #[test]
    fn a_lost_packet_holds_the_picture_until_the_next_keyframe() {
        let mut f = Freeze::default();
        f.packet(1);
        assert!(f.pass(true));
        assert!(!f.packet(2));
        assert!(f.pass(false));
        assert!(f.packet(4), "3 was lost");
        assert!(!f.pass(false));
        assert!(!f.packet(6), "already frozen");
        f.packet(7);
        assert!(!f.pass(false));
        assert!(f.pass(true));
        f.packet(8);
        assert!(f.pass(false));
    }

    #[test]
    fn sequence_numbers_wrap_around() {
        let mut f = Freeze::default();
        f.packet(u16::MAX);
        f.pass(true);
        assert!(!f.packet(0));
        assert!(f.pass(false));
    }
}
