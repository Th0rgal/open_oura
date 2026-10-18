//! Personal baseline — the asymmetric EMA of a rolling mean and abs-deviation
//! that ecore maintains per metric, with step size that anneals by baseline age.
//! Ported faithfully from `baseline_update_lt_mean_and_dev @ 0x1dad04`.
//!
//! Values are stored as **real × 8** fixed-point (i16 in ecore; we keep i32 for
//! headroom). The per-metric output clamp tables (`DAT_0017bc46/58/6a`) are not in
//! the decompile, so this reproduces the EMA update and sentinel only — see
//! `docs/algorithms/baselines.md`.

/// i16 "no data" sentinel used throughout ecore baselines.
pub const INVALID: i32 = 0x7eb6;

/// Saved-record selection before a long-term baseline update.
pub mod history;

/// Initial sleep-duration mean, in minutes multiplied by eight.
///
/// Android 7.26.0 (260930084), `baseline_get_sleep_time_lt_mean_times_8 @ 0xe2340`.
/// Feature 27 selects the newer age curve; the caller supplies the actual
/// feature setting. The native operation rounds minutes *before* scaling.
/// This does not initialize the other eight baseline items or a recovery state.
pub fn sleep_time_initial_mean_x8(age: u32, feature_27: bool) -> i32 {
    let minutes = if feature_27 {
        (age as f64).mul_add(-0.87, 468.0)
    } else {
        (age as f64).mul_add(-1.77, 486.0) + 20.0
    };
    (minutes.round() as i32).wrapping_shl(3)
}

/// Sleep-balance mean's lower limit, in minutes multiplied by eight.
///
/// Android 7.26.0, `baseline_get_sleep_time_acceptable_min_times_8 @ 0xe23b0`.
/// Unlike the initial mean, this truncates *after* scaling. The native setter
/// stores the result as an `i16`; narrow explicitly when constructing that
/// table. This is the sleep-balance item (index 1), not the previous-night item.
pub fn sleep_time_acceptable_min_x8(age: u32, feature_27: bool) -> i32 {
    let minutes = if feature_27 {
        (age as f64).mul_add(-0.87, 408.0)
    } else {
        (age as f64).mul_add(-1.77, 486.0)
    };
    (minutes * 8.0) as i32
}

/// A rolling personal baseline: `mean` and abs-`deviation`, both ×8 fixed-point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Baseline {
    pub mean_x8: i32,
    pub dev_x8: i32,
}

/// Arithmetic shift right with round-toward-zero bias, matching the decompiled
/// `(t < 0) ? (t + (2^s - 1)) >> s : t >> s`.
fn ashr_round(t: i32, shift: u32) -> i32 {
    let adj = if t < 0 { t + ((1 << shift) - 1) } else { t };
    adj >> shift
}

impl Baseline {
    pub fn new() -> Self {
        Baseline::default()
    }

    /// Update with a new (unscaled) sample given the baseline age in days. The
    /// step size anneals across three age bands (<4, 4–14, >14 days): the warm-up
    /// is fast (mean gain 1/2) and settles to ~1/32 once the baseline is mature.
    pub fn update(&mut self, sample: i32, age_days: u32) {
        let sample_x8 = sample << 3;
        let delta = sample_x8 - self.mean_x8;

        // --- mean step ---
        if age_days > 14 {
            let bias = if delta != 0 && self.mean_x8 <= sample_x8 {
                16
            } else {
                -16
            };
            self.mean_x8 += ashr_round(delta + bias, 5);
        } else if age_days >= 4 {
            let bias = if delta > 0 { 4 } else { -4 };
            self.mean_x8 += ashr_round(delta + bias, 3);
        } else {
            let t = if delta > 0 { delta + 1 } else { delta - 1 };
            self.mean_x8 += ashr_round(t, 1);
        }

        // --- deviation step (target = |sample - new mean|) ---
        let absd = (sample_x8 - self.mean_x8).abs();
        let (mag, shift) = if age_days > 14 {
            (32, 6)
        } else if age_days >= 4 {
            (8, 4)
        } else {
            (4, 3)
        };
        let bias2 = if absd != self.dev_x8 && self.dev_x8 <= absd {
            mag
        } else {
            -mag
        };
        self.dev_x8 += ashr_round((absd - self.dev_x8) + bias2, shift);
    }

    /// Mean in real units.
    pub fn mean(&self) -> f64 {
        self.mean_x8 as f64 / 8.0
    }
    /// Abs-deviation in real units.
    pub fn deviation(&self) -> f64 {
        self.dev_x8 as f64 / 8.0
    }
    /// Normalized deviation of `sample` from the mean (readiness contributors are
    /// built on this). `None` if the deviation hasn't accumulated yet.
    pub fn z(&self, sample: f64) -> Option<f64> {
        if self.dev_x8 == 0 {
            None
        } else {
            Some((sample - self.mean()) / self.deviation())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_up_then_settle() {
        let mut b = Baseline::new();
        b.update(100, 0); // delta 800 -> +400
        assert_eq!(b.mean_x8, 400);
        b.update(100, 0); // delta 400 -> +200
        assert_eq!(b.mean_x8, 600);
        for _ in 0..400 {
            b.update(100, 30); // mature: slow convergence toward 800 (=100*8)
        }
        // settles within the fixed-point deadband (~2 units) of the target
        assert!((b.mean() - 100.0).abs() < 2.5, "{}", b.mean());
    }

    #[test]
    fn current_android_age_curves_round_at_different_steps() {
        // Generated controls recorded from Android 7.26.0, both feature states.
        // Columns: age, feature 27, initial mean x8, acceptable minimum x8.
        for (age, feature, mean, minimum) in [
            (0, false, 4048, 3888),
            (18, false, 3792, 3633),
            (30, false, 3624, 3463),
            (65, false, 3128, 2967),
            (255, false, 440, 277),
            (0, true, 3744, 3264),
            (18, true, 3616, 3138),
            (30, true, 3536, 3055),
            (65, true, 3288, 2811),
            (255, true, 1968, 1489),
            (1_206_797_259, false, 91_624_048, i32::MIN),
            (1_499_916_368, true, -1_849_479_584, i32::MIN),
        ] {
            assert_eq!(sleep_time_initial_mean_x8(age, feature), mean);
            assert_eq!(sleep_time_acceptable_min_x8(age, feature), minimum);
        }
    }
}
