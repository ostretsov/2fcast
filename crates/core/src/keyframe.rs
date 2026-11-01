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
}
