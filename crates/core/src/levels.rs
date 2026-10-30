//! Audio level meters: dB values from GStreamer's `level` element mapped to
//! the on-screen scale.

/// Lowest level shown on the meter; anything below is drawn as silence.
pub const FLOOR_DB: f64 = -60.0;

/// Labelled marks on the scale.
pub const TICKS_DB: [f64; 7] = [-60.0, -40.0, -30.0, -20.0, -10.0, -6.0, 0.0];

/// One level reading in dBFS (0 = full scale), loudest channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Level {
    pub rms_db: f64,
    pub peak_db: f64,
}

impl Level {
    pub const SILENT: Level = Level {
        rms_db: f64::NEG_INFINITY,
        peak_db: f64::NEG_INFINITY,
    };

    /// From per-channel values as posted by `level`.
    pub fn from_channels(rms: &[f64], peak: &[f64]) -> Level {
        Level {
            rms_db: loudest(rms),
            peak_db: loudest(peak),
        }
    }
}

/// Loudest channel; silence (-inf) for no channels or NaN.
pub fn loudest(values: &[f64]) -> f64 {
    values
        .iter()
        .copied()
        .filter(|v| !v.is_nan())
        .fold(f64::NEG_INFINITY, f64::max)
}

/// Position on the meter, 0 (at or below the floor) to 1 (0 dBFS), linear in dB.
pub fn fraction(db: f64) -> f64 {
    if db.is_nan() {
        return 0.0;
    }
    ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    /// Comfortable speech level.
    Green,
    /// Loud: close to clipping.
    Yellow,
    /// Too loud: will distort.
    Red,
}

pub fn zone(db: f64) -> Zone {
    if db >= -3.0 {
        Zone::Red
    } else if db >= -12.0 {
        Zone::Yellow
    } else {
        Zone::Green
    }
}

/// Text next to the meter, e.g. "-12 dB", or "silent" below the floor.
pub fn label(db: f64) -> String {
    if db.is_nan() || db < FLOOR_DB {
        "silent".into()
    } else {
        format!("{:.0} dB", db.min(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loudest_channel_wins() {
        assert_eq!(loudest(&[-30.0, -12.5]), -12.5);
        assert_eq!(loudest(&[]), f64::NEG_INFINITY);
        assert_eq!(loudest(&[f64::NAN, -40.0]), -40.0);
        let l = Level::from_channels(&[-20.0, -25.0], &[-6.0, -9.0]);
        assert_eq!(
            l,
            Level {
                rms_db: -20.0,
                peak_db: -6.0
            }
        );
    }

    #[test]
    fn scale_mapping() {
        assert_eq!(fraction(0.0), 1.0);
        assert_eq!(fraction(-30.0), 0.5);
        assert_eq!(fraction(-60.0), 0.0);
        assert_eq!(fraction(f64::NEG_INFINITY), 0.0);
        assert_eq!(fraction(f64::NAN), 0.0);
        assert_eq!(fraction(3.0), 1.0);
        assert!(TICKS_DB.windows(2).all(|w| fraction(w[0]) < fraction(w[1])));
    }

    #[test]
    fn zones_and_labels() {
        assert_eq!(zone(-30.0), Zone::Green);
        assert_eq!(zone(-8.0), Zone::Yellow);
        assert_eq!(zone(-1.0), Zone::Red);
        assert_eq!(label(-12.4), "-12 dB");
        assert_eq!(label(f64::NEG_INFINITY), "silent");
        assert_eq!(label(-75.0), "silent");
        assert_eq!(label(1.0), "0 dB");
    }
}
