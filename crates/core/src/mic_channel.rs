//! Turning a (usually two-channel) microphone input into one mono voice.
//!
//! Audio interfaces such as the Focusrite Scarlett deliver input 1 on the left
//! and input 2 on the right channel, so a single mic is only on one side;
//! ordinary USB and laptop mics carry the same sound on both. In `Auto` mode
//! the channel levels are watched while someone speaks: if one channel is much
//! louder, only that one is used; otherwise both are averaged.

/// Which input channel(s) make up the voice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Both,
    Left,
    Right,
}

impl Pick {
    /// Weights of (left, right) in the mono voice.
    pub fn gains(self) -> [f32; 2] {
        match self {
            Pick::Both => [0.5, 0.5],
            Pick::Left => [1.0, 0.0],
            Pick::Right => [0.0, 1.0],
        }
    }
}

/// The `microphone_channel` setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Fixed(Pick),
}

impl Mode {
    /// "auto" (default), "both", "left" or "right"; anything else is "auto".
    pub fn parse(s: &str) -> Mode {
        match s.trim().to_ascii_lowercase().as_str() {
            "both" => Mode::Fixed(Pick::Both),
            "left" => Mode::Fixed(Pick::Left),
            "right" => Mode::Fixed(Pick::Right),
            _ => Mode::Auto,
        }
    }

    pub fn initial(self) -> Pick {
        match self {
            Mode::Auto => Pick::Both,
            Mode::Fixed(p) => p,
        }
    }
}

/// A channel counts as "the voice" if it is this much louder than the other.
pub const DOMINANCE_DB: f64 = 20.0;
/// Readings quieter than this (both channels) are not speech: no evidence.
pub const SPEECH_DB: f64 = -50.0;
/// Consecutive speech readings agreeing on a new pick before switching
/// (with 50 ms readings: one second of speech).
pub const VOTES_TO_SWITCH: u32 = 20;

/// Decides the pick from per-channel RMS readings (dBFS) in `Auto` mode.
#[derive(Clone, Debug)]
pub struct Picker {
    mode: Mode,
    current: Pick,
    candidate: Pick,
    votes: u32,
}

impl Picker {
    pub fn new(mode: Mode) -> Picker {
        let current = mode.initial();
        Picker {
            mode,
            current,
            candidate: current,
            votes: 0,
        }
    }

    pub fn current(&self) -> Pick {
        self.current
    }

    /// Feeds one reading of the *input* (before mixing to mono). Returns the
    /// new pick when it changes.
    pub fn feed(&mut self, rms_db: &[f64]) -> Option<Pick> {
        if self.mode != Mode::Auto || rms_db.len() < 2 {
            return None;
        }
        let (l, r) = (finite(rms_db[0]), finite(rms_db[1]));
        if l.max(r) < SPEECH_DB {
            return None; // Silence or noise tells us nothing.
        }
        let vote = if l - r >= DOMINANCE_DB {
            Pick::Left
        } else if r - l >= DOMINANCE_DB {
            Pick::Right
        } else {
            Pick::Both
        };
        if vote == self.current {
            self.votes = 0;
            return None;
        }
        if vote != self.candidate {
            self.candidate = vote;
            self.votes = 0;
        }
        self.votes += 1;
        if self.votes >= VOTES_TO_SWITCH {
            self.current = vote;
            self.votes = 0;
            return Some(vote);
        }
        None
    }
}

fn finite(db: f64) -> f64 {
    if db.is_finite() { db } else { -200.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_n(p: &mut Picker, n: u32, l: f64, r: f64) -> Option<Pick> {
        let mut last = None;
        for _ in 0..n {
            if let Some(x) = p.feed(&[l, r]) {
                last = Some(x);
            }
        }
        last
    }

    #[test]
    fn settings() {
        assert_eq!(Mode::parse("auto"), Mode::Auto);
        assert_eq!(Mode::parse(""), Mode::Auto);
        assert_eq!(Mode::parse(" Left "), Mode::Fixed(Pick::Left));
        assert_eq!(Mode::parse("right"), Mode::Fixed(Pick::Right));
        assert_eq!(Mode::parse("both"), Mode::Fixed(Pick::Both));
        assert_eq!(Mode::Auto.initial(), Pick::Both);
        assert_eq!(Pick::Left.gains(), [1.0, 0.0]);
        assert_eq!(Pick::Both.gains(), [0.5, 0.5]);
    }

    #[test]
    fn scarlett_input_one_picks_left() {
        let mut p = Picker::new(Mode::Auto);
        // Mic on input 1, nothing on input 2 (-inf on a digital-silent input).
        assert_eq!(
            feed_n(&mut p, VOTES_TO_SWITCH - 1, -30.0, f64::NEG_INFINITY),
            None
        );
        assert_eq!(p.feed(&[-30.0, f64::NEG_INFINITY]), Some(Pick::Left));
        assert_eq!(p.current(), Pick::Left);
    }

    #[test]
    fn mic_on_input_two_picks_right() {
        let mut p = Picker::new(Mode::Auto);
        assert_eq!(
            feed_n(&mut p, VOTES_TO_SWITCH, -85.0, -25.0),
            Some(Pick::Right)
        );
    }

    #[test]
    fn usb_mic_with_both_channels_stays_both() {
        let mut p = Picker::new(Mode::Auto);
        assert_eq!(feed_n(&mut p, 200, -28.0, -29.5), None);
        assert_eq!(p.current(), Pick::Both);
    }

    #[test]
    fn silence_is_no_evidence() {
        let mut p = Picker::new(Mode::Auto);
        // A pause in speech must not reset or count: 19 votes, silence, 1 vote.
        feed_n(&mut p, VOTES_TO_SWITCH - 1, -30.0, -90.0);
        assert_eq!(feed_n(&mut p, 100, -70.0, -95.0), None);
        assert_eq!(p.feed(&[-30.0, -90.0]), Some(Pick::Left));
    }

    #[test]
    fn short_glitches_do_not_switch() {
        let mut p = Picker::new(Mode::Auto);
        for _ in 0..50 {
            feed_n(&mut p, 5, -30.0, -90.0); // Left-only bursts...
            feed_n(&mut p, 1, -30.0, -31.0); // ...interrupted by "both".
        }
        assert_eq!(p.current(), Pick::Both);
    }

    #[test]
    fn switches_back_when_the_setup_changes() {
        let mut p = Picker::new(Mode::Auto);
        feed_n(&mut p, VOTES_TO_SWITCH, -30.0, -90.0);
        assert_eq!(p.current(), Pick::Left);
        // Mic replugged into a device with the voice on both channels.
        assert_eq!(
            feed_n(&mut p, VOTES_TO_SWITCH, -30.0, -32.0),
            Some(Pick::Both)
        );
    }

    #[test]
    fn fixed_mode_never_changes() {
        let mut p = Picker::new(Mode::Fixed(Pick::Right));
        assert_eq!(feed_n(&mut p, 100, -30.0, -90.0), None);
        assert_eq!(p.current(), Pick::Right);
        assert_eq!(p.feed(&[-30.0]), None, "mono input: nothing to decide");
    }
}
