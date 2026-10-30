//! Voice clean-up applied to the microphone before it is sent and recorded:
//!
//! 1. high-pass filter: removes rumble, desk bumps, breath pops and hum;
//! 2. noise gate ([`crate::gate`]): silence while not speaking;
//! 3. compressor: evens out loud and quiet speech, with a little make-up gain;
//! 4. limiter: keeps laughs and shouts from clipping.
//!
//! The gate runs before the compressor so its threshold is measured on the
//! plain mic level (and the make-up gain does not raise the room noise).
//!
//! [`Ducker`] works on the other end: while I speak, the partner's voice on my
//! speakers gets a little quieter, so talking over each other stays
//! intelligible.  Only the speakers: recordings keep both levels unchanged.

use crate::gate::{Gate, GateSettings};

pub const DEFAULT_HIGH_PASS_HZ: f64 = 80.0;
pub const DEFAULT_DUCK_DB: f64 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceSettings {
    /// High-pass cutoff; `None` = off.
    pub high_pass_hz: Option<f64>,
    /// Noise gate threshold (dBFS); `None` = off.
    pub gate_db: Option<f64>,
    pub compressor: bool,
    pub rate: u32,
}

/// Parses the `high_pass_hz` setting: 0 (or nonsense) turns the filter off.
pub fn high_pass_from_setting(hz: f64) -> Option<f64> {
    (hz.is_finite() && hz > 0.0).then(|| hz.clamp(20.0, 300.0))
}

pub struct Voice {
    high_pass: Option<HighPass>,
    /// The noise gate; without one it only detects speech (on a copy).
    gate: Gate,
    gating: bool,
    scratch: Vec<i16>,
    compressor: Option<Compressor>,
    limiter: Limiter,
}

impl Voice {
    pub fn new(s: VoiceSettings) -> Voice {
        Voice {
            high_pass: s.high_pass_hz.map(|hz| HighPass::new(hz, s.rate)),
            gate: Gate::new(GateSettings::new(
                s.gate_db.unwrap_or(crate::gate::DEFAULT_THRESHOLD_DB),
                s.rate,
            )),
            gating: s.gate_db.is_some(),
            scratch: vec![],
            compressor: s.compressor.then(|| Compressor::new(s.rate)),
            limiter: Limiter::new(s.rate),
        }
    }

    /// Processes one block of mono 16-bit samples in place.
    pub fn process(&mut self, samples: &mut [i16]) {
        if let Some(f) = &mut self.high_pass {
            map(samples, |x| f.next(x));
        }
        if self.gating {
            self.gate.process(samples);
        } else {
            self.scratch.clear();
            self.scratch.extend_from_slice(samples);
            self.gate.process(&mut self.scratch);
        }
        if let Some(c) = &mut self.compressor {
            map(samples, |x| c.next(x));
        }
        map(samples, |x| self.limiter.next(x));
    }
}

impl Voice {
    /// Whether I am speaking (the gate is open, or would be).
    pub fn is_speaking(&self) -> bool {
        self.gate.is_open()
    }
}

/// Lowers the partner's voice on my speakers while I speak: down over
/// 100 ms, back up over 400 ms.
pub struct Ducker {
    gain: f64,
    low: f64,
    down: f64,
    up: f64,
}

impl Ducker {
    pub fn new(db: f64, rate: u32) -> Ducker {
        let low = db_to_gain(-db.abs());
        let per_ms = rate as f64 / 1000.0;
        Ducker {
            gain: 1.0,
            low,
            down: (1.0 - low) / (100.0 * per_ms),
            up: (1.0 - low) / (400.0 * per_ms),
        }
    }

    pub fn process(&mut self, samples: &mut [i16], i_speak: bool) {
        if !i_speak && self.gain >= 1.0 {
            return; // The usual case: untouched.
        }
        map(samples, |x| {
            self.gain = if i_speak {
                (self.gain - self.down).max(self.low)
            } else {
                (self.gain + self.up).min(1.0)
            };
            x * self.gain
        });
    }
}

/// Parses the `duck_partner_db` setting: 0 (or nonsense) turns ducking off.
pub fn duck_from_setting(db: f64) -> Option<f64> {
    (db.is_finite() && db.abs() > 0.0).then(|| db.abs().min(20.0))
}

fn map(samples: &mut [i16], mut f: impl FnMut(f64) -> f64) {
    for s in samples.iter_mut() {
        *s = f(*s as f64).round().clamp(-32768.0, 32767.0) as i16;
    }
}

fn db_to_gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Smoothing coefficient for a one-pole follower with time constant `ms`.
fn coef(ms: f64, rate: u32) -> f64 {
    (-1.0 / (ms / 1000.0 * rate as f64)).exp()
}

/// Second-order Butterworth high-pass (RBJ cookbook biquad).
struct HighPass {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl HighPass {
    fn new(hz: f64, rate: u32) -> HighPass {
        let w0 = 2.0 * std::f64::consts::PI * hz / rate as f64;
        let alpha = w0.sin() / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let cos = w0.cos();
        let a0 = 1.0 + alpha;
        let b0 = (1.0 + cos) / 2.0 / a0;
        HighPass {
            b: [b0, -2.0 * b0, b0],
            a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    fn next(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// Gentle speech compressor: above `THRESHOLD_DB` (RMS) the level rises only
/// 1/`RATIO` as fast; `MAKEUP_DB` lifts the result back up.
struct Compressor {
    /// Signal power averaged over ~20 ms (full scale = 1).
    power: f64,
    smooth: f64,
    /// Current gain reduction (dB); it follows the target with attack/release.
    reduction: f64,
    attack: f64,
    release: f64,
}

const THRESHOLD_DB: f64 = -28.0;
const RATIO: f64 = 3.0;
const MAKEUP_DB: f64 = 6.0;

impl Compressor {
    fn new(rate: u32) -> Compressor {
        Compressor {
            power: 0.0,
            smooth: coef(20.0, rate),
            reduction: 0.0,
            attack: coef(5.0, rate),
            release: coef(150.0, rate),
        }
    }

    fn next(&mut self, x: f64) -> f64 {
        let p = (x / 32768.0).powi(2);
        self.power = self.smooth * self.power + (1.0 - self.smooth) * p;
        let level_db = 10.0 * self.power.max(1e-12).log10();
        let target = (level_db - THRESHOLD_DB).max(0.0) * (1.0 - 1.0 / RATIO);
        let k = if target > self.reduction {
            self.attack
        } else {
            self.release
        };
        self.reduction = k * self.reduction + (1.0 - k) * target;
        x * db_to_gain(MAKEUP_DB - self.reduction)
    }
}

/// Peak limiter: no sample goes above -1 dBFS; the gain comes back over ~50 ms.
struct Limiter {
    gain: f64,
    release: f64,
}

const CEILING: f64 = 32767.0 * 0.891; // -1 dBFS

impl Limiter {
    fn new(rate: u32) -> Limiter {
        Limiter {
            gain: 1.0,
            release: coef(50.0, rate),
        }
    }

    fn next(&mut self, x: f64) -> f64 {
        self.gain = 1.0 - self.release * (1.0 - self.gain);
        if x.abs() * self.gain > CEILING {
            self.gain = CEILING / x.abs();
        }
        x * self.gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::block_db;

    const RATE: u32 = 48_000;

    /// `blocks` 10 ms blocks of a sine at `hz` with RMS level `db`.
    fn tone(hz: f64, db: f64, blocks: usize) -> Vec<Vec<i16>> {
        let amp = db_to_gain(db) * 2f64.sqrt() * 32767.0;
        let w = 2.0 * std::f64::consts::PI * hz / RATE as f64;
        (0..blocks)
            .map(|b| {
                (0..480)
                    .map(|i| (amp * (w * (b * 480 + i) as f64).sin()).round() as i16)
                    .collect()
            })
            .collect()
    }

    /// Level of the last block after processing `blocks` blocks.
    fn out_db(v: &mut Voice, hz: f64, db: f64, blocks: usize) -> f64 {
        let mut last = f64::NEG_INFINITY;
        for mut b in tone(hz, db, blocks) {
            v.process(&mut b);
            last = block_db(&b);
        }
        last
    }

    fn only(high_pass: bool, compressor: bool) -> Voice {
        Voice::new(VoiceSettings {
            high_pass_hz: high_pass.then_some(DEFAULT_HIGH_PASS_HZ),
            gate_db: None,
            compressor,
            rate: RATE,
        })
    }

    #[test]
    fn high_pass_removes_rumble_keeps_voice() {
        let rumble = out_db(&mut only(true, false), 30.0, -20.0, 50);
        assert!(rumble < -35.0, "30 Hz attenuated: {rumble}");
        let voice = out_db(&mut only(true, false), 300.0, -20.0, 50);
        assert!((voice + 20.0).abs() < 0.5, "300 Hz passes: {voice}");
        // DC offset (cheap sound cards) is removed too.
        let mut v = only(true, false);
        let mut dc = vec![3000i16; 4800];
        v.process(&mut dc);
        assert!(dc[4799].abs() < 30, "{}", dc[4799]);
    }

    #[test]
    fn compressor_evens_out_loud_and_quiet() {
        let quiet = out_db(&mut only(false, true), 300.0, -40.0, 100);
        assert!(
            (quiet + 34.0).abs() < 0.5,
            "below threshold: +6 dB: {quiet}"
        );
        let loud = out_db(&mut only(false, true), 300.0, -10.0, 100);
        // -28 + 18/3 + 6 = -16
        assert!((loud + 16.0).abs() < 0.7, "{loud}");
        assert!(
            loud - quiet < 20.0,
            "30 dB apart before, {} after",
            loud - quiet
        );
    }

    #[test]
    fn limiter_keeps_peaks_below_full_scale() {
        let mut v = only(false, false);
        let mut peaks = tone(300.0, 0.0, 20).concat(); // clips at +3 dB peaks
        let mut square = vec![32767i16; 480];
        v.process(&mut peaks);
        v.process(&mut square);
        let max = peaks.iter().chain(&square).map(|x| x.unsigned_abs()).max();
        assert!(max <= Some(CEILING as u16 + 1), "{max:?}");
        // A normal voice is not touched.
        let mut v = only(false, false);
        let mut b = tone(300.0, -20.0, 1).remove(0);
        let orig = b.clone();
        v.process(&mut b);
        assert_eq!(b, orig);
    }

    #[test]
    fn gate_still_silences_noise_before_make_up_gain() {
        let mut v = Voice::new(VoiceSettings {
            high_pass_hz: Some(DEFAULT_HIGH_PASS_HZ),
            gate_db: Some(-45.0),
            compressor: true,
            rate: RATE,
        });
        assert_eq!(out_db(&mut v, 300.0, -60.0, 50), f64::NEG_INFINITY);
        let speech = out_db(&mut v, 300.0, -25.0, 100);
        assert!((speech + 21.0).abs() < 1.0, "-28 + 3/3 + 6: {speech}");
    }

    #[test]
    fn detects_speech_with_and_without_gate() {
        for gate_db in [Some(-45.0), None] {
            let mut v = Voice::new(VoiceSettings {
                high_pass_hz: None,
                gate_db,
                compressor: false,
                rate: RATE,
            });
            out_db(&mut v, 300.0, -60.0, 20);
            assert!(!v.is_speaking(), "{gate_db:?}");
            let level = out_db(&mut v, 300.0, -25.0, 5);
            assert!(v.is_speaking(), "{gate_db:?}");
            assert!((level + 25.0).abs() < 0.5, "unchanged level: {level}");
        }
        // Without a gate the noise passes.
        let mut v = only(false, false);
        assert!((out_db(&mut v, 300.0, -60.0, 20) + 60.0).abs() < 0.5);
    }

    #[test]
    fn ducks_partner_while_i_speak() {
        let mut d = Ducker::new(6.0, RATE);
        let block = |d: &mut Ducker, speak: bool| {
            let mut b = tone(300.0, -20.0, 1).remove(0);
            d.process(&mut b, speak);
            block_db(&b)
        };
        assert!((block(&mut d, false) + 20.0).abs() < 0.1, "untouched");
        for _ in 0..10 {
            block(&mut d, true);
        }
        let ducked = block(&mut d, true);
        assert!(
            (ducked + 26.0).abs() < 0.3,
            "6 dB down after 100 ms: {ducked}"
        );
        let first_back = block(&mut d, false);
        assert!(first_back < -24.0, "comes back slowly: {first_back}");
        for _ in 0..40 {
            block(&mut d, false);
        }
        assert!((block(&mut d, false) + 20.0).abs() < 0.1, "back to normal");
    }

    #[test]
    fn settings() {
        assert_eq!(duck_from_setting(6.0), Some(6.0));
        assert_eq!(duck_from_setting(-6.0), Some(6.0));
        assert_eq!(duck_from_setting(0.0), None);
        assert_eq!(high_pass_from_setting(80.0), Some(80.0));
        assert_eq!(high_pass_from_setting(0.0), None);
        assert_eq!(high_pass_from_setting(5.0), Some(20.0));
        assert_eq!(high_pass_from_setting(f64::NAN), None);
    }
}
