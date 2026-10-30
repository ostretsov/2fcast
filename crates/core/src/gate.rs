//! Noise gate: silences a voice while that person is not speaking, so room
//! noise is neither sent to the partner nor recorded.
//!
//! The gate opens as soon as a block of audio is louder than the threshold,
//! stays open for `hold` after the last loud block (pauses between words are
//! kept) and then fades out over `release` (no click).

use crate::levels;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GateSettings {
    /// Opens above this level (dBFS RMS of a block).
    pub threshold_db: f64,
    /// While open, blocks this much below the threshold still count as speech.
    pub hysteresis_db: f64,
    pub hold_ms: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    pub rate: u32,
}

pub const DEFAULT_THRESHOLD_DB: f64 = -45.0;

impl GateSettings {
    pub fn new(threshold_db: f64, rate: u32) -> Self {
        GateSettings {
            threshold_db,
            hysteresis_db: 6.0,
            hold_ms: 400.0,
            attack_ms: 5.0,
            release_ms: 150.0,
            rate,
        }
    }

    fn samples(&self, ms: f64) -> f64 {
        (ms * self.rate as f64 / 1000.0).max(1.0)
    }
}

#[derive(Clone, Debug)]
pub struct Gate {
    s: GateSettings,
    /// Current gain, 0 (closed) to 1 (open).
    gain: f32,
    /// Samples left before the gate starts closing.
    hold_left: f64,
}

impl Gate {
    /// Starts closed: a new session begins in silence, not with a noise burst.
    pub fn new(s: GateSettings) -> Gate {
        Gate {
            s,
            gain: 0.0,
            hold_left: 0.0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.hold_left > 0.0 || self.gain > 0.0
    }

    /// Processes one block of mono 16-bit samples in place.
    pub fn process(&mut self, samples: &mut [i16]) {
        if samples.is_empty() {
            return;
        }
        let db = block_db(samples);
        let threshold = if self.hold_left > 0.0 {
            self.s.threshold_db - self.s.hysteresis_db
        } else {
            self.s.threshold_db
        };
        if db >= threshold {
            self.hold_left = self.s.samples(self.s.hold_ms);
        } else {
            self.hold_left = (self.hold_left - samples.len() as f64).max(0.0);
        }
        let target = if self.hold_left > 0.0 { 1.0 } else { 0.0 };
        let up = (1.0 / self.s.samples(self.s.attack_ms)) as f32;
        let down = (1.0 / self.s.samples(self.s.release_ms)) as f32;
        for x in samples.iter_mut() {
            self.gain = if target > self.gain {
                (self.gain + up).min(1.0)
            } else {
                (self.gain - down).max(0.0)
            };
            if self.gain < 1.0 {
                *x = (*x as f32 * self.gain).round() as i16;
            }
        }
    }
}

/// RMS level of a block in dBFS.
pub fn block_db(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return f64::NEG_INFINITY;
    }
    let sum: f64 = samples.iter().map(|&x| (x as f64 / 32768.0).powi(2)).sum();
    let rms = (sum / samples.len() as f64).sqrt();
    if rms > 0.0 {
        20.0 * rms.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// Parses the `noise_gate_db` setting; `None` turns the gate off.
pub fn threshold_from_setting(enabled: bool, db: f64) -> Option<f64> {
    (enabled && db.is_finite()).then(|| db.clamp(levels::FLOOR_DB, -10.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    /// 10 ms blocks, as the microphone delivers them.
    const BLOCK: usize = 480;

    fn tone(db: f64) -> Vec<i16> {
        let amp = 10f64.powf(db / 20.0) * 2f64.sqrt() * 32767.0;
        (0..BLOCK)
            .map(|i| (amp * (i as f64 * 0.13).sin()) as i16)
            .collect()
    }

    fn run(g: &mut Gate, db: f64, blocks: usize) -> f64 {
        let mut last = f64::NEG_INFINITY;
        for _ in 0..blocks {
            let mut b = tone(db);
            g.process(&mut b);
            last = block_db(&b);
        }
        last
    }

    fn gate() -> Gate {
        Gate::new(GateSettings::new(-45.0, RATE))
    }

    #[test]
    fn measures_levels() {
        assert!((block_db(&tone(-20.0)) + 20.0).abs() < 0.5);
        assert_eq!(block_db(&[0; 10]), f64::NEG_INFINITY);
        assert_eq!(block_db(&[]), f64::NEG_INFINITY);
    }

    #[test]
    fn background_noise_is_silenced() {
        let mut g = gate();
        assert_eq!(run(&mut g, -60.0, 50), f64::NEG_INFINITY);
        assert!(!g.is_open());
    }

    #[test]
    fn speech_passes_unchanged() {
        let mut g = gate();
        let first = run(&mut g, -25.0, 1);
        assert!(
            first > -32.0,
            "opens within the first block (attack 5 ms): {first}"
        );
        let steady = run(&mut g, -25.0, 20);
        assert!((steady + 25.0).abs() < 0.5, "{steady}");
    }

    #[test]
    fn short_pauses_are_kept_then_it_closes() {
        let mut g = gate();
        run(&mut g, -25.0, 20);
        // A 300 ms pause of room noise: still open (hold 400 ms), noise passes.
        let during = run(&mut g, -60.0, 30);
        assert!((during + 60.0).abs() < 1.0, "{during}");
        // After hold + release the noise is gone.
        assert_eq!(run(&mut g, -60.0, 30), f64::NEG_INFINITY);
        assert!(!g.is_open());
    }

    #[test]
    fn hysteresis_keeps_quiet_word_endings() {
        let mut g = gate();
        run(&mut g, -25.0, 10);
        // Trailing off just under the threshold (but within 6 dB) stays open.
        let tail = run(&mut g, -48.0, 100);
        assert!((tail + 48.0).abs() < 1.0, "{tail}");
        // From closed, the same level does not open it.
        let mut g = gate();
        assert_eq!(run(&mut g, -48.0, 50), f64::NEG_INFINITY);
    }

    #[test]
    fn closing_fades_instead_of_clicking() {
        let mut g = gate();
        run(&mut g, -20.0, 20);
        run(&mut g, -60.0, 40); // the 400 ms hold has just run out
        let mut block = tone(-60.0);
        g.process(&mut block); // release starts inside this block
        assert!(g.is_open(), "still fading out");
        assert!(block.iter().any(|&x| x != 0), "fade, not an instant cut");
        assert!(block_db(&block) < -60.0, "but already quieter");
    }

    #[test]
    fn settings() {
        assert_eq!(threshold_from_setting(true, -45.0), Some(-45.0));
        assert_eq!(threshold_from_setting(false, -45.0), None);
        assert_eq!(threshold_from_setting(true, -100.0), Some(-60.0));
        assert_eq!(threshold_from_setting(true, 0.0), Some(-10.0));
        assert_eq!(threshold_from_setting(true, f64::NAN), None);
    }
}
