//! Personal baseline — the asymmetric EMA of a rolling mean and abs-deviation
//! that ecore maintains per metric, with step size that anneals by baseline age.
//! Original EMA port: `baseline_update_lt_mean_and_dev @ 0x1dad04`.
//! Current Android 7.26.0 native update: `update_ecore_with_config @ 0xe1aa8`.
//!
//! Values are stored as **real × 8** fixed-point (i16 in ecore; we keep i32 for
//! headroom). The per-metric output clamp tables (`DAT_0017bc46/58/6a`) are not in
//! the original decompile. `update` preserves that unclamped EMA;
//! `update_ecore_with_config` adds the measured metric limits and outlier policy.
//! The mutable lower limits and feature setting must come from the caller. See
//! `docs/algorithms/baselines.md`.

/// i16 "no data" sentinel used throughout ecore baselines.
pub const INVALID: i32 = 0x7eb6;

/// Current-engine settings that affect the update. The first two lower limits
/// are mutable in ecore (previous-night and age-dependent sleep-balance limits),
/// so callers must persist the actual configured values, not assume defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcoreBaselineConfig {
    pub mean_min_x8: [i16; 9],
    pub feature_23: bool,
}

impl Default for EcoreBaselineConfig {
    fn default() -> Self {
        Self {
            mean_min_x8: [680, 3250, 320, 2400, 4000, 80, 2400, 26400, 3040],
            feature_23: false,
        }
    }
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

    /// The update with explicitly supplied native configuration. The nine
    /// lower limits are signed i16 fixed-point values exactly as ecore stores.
    pub fn update_ecore_with_config(
        &mut self,
        sample: i16,
        age_days: u32,
        metric_index: usize,
        config: &EcoreBaselineConfig,
    ) -> Result<(), InvalidBaselineMetric> {
        const MEAN_MAX: [i32; 9] = [760, 4320, 720, 3360, 30000, 320, 4320, 29920, 6000];
        const DEV_MIN: [i32; 9] = [40, 200, 24, 400, 2000, 16, 720, 240, 240];
        const DEV_MAX: [i32; 9] = [80, 360, 40, 560, 10000, 240, 960, 400, 480];
        if metric_index >= config.mean_min_x8.len() {
            return Err(InvalidBaselineMetric(metric_index));
        }
        let normalize = |value: i32| {
            let value = value as i16 as i32;
            if value < 0 {
                INVALID
            } else {
                value.min(INVALID)
            }
        };
        let mut mean = normalize(self.mean_x8);
        let mut dev = normalize(self.dev_x8);
        let wide = config.feature_23 && matches!(metric_index, 4 | 5);
        let mut target = (sample as i32) * 8;
        if wide {
            target = target.clamp(0, INVALID);
        } else if age_days > 3 {
            let delta = target - mean;
            let bias = if dev == 0 { -1 } else { 1 };
            let limit = if delta.abs() > (5 * dev + bias) / 2 {
                dev
            } else {
                (3 * dev + bias) / 2
            };
            if delta.abs() > limit {
                target = mean + if delta > 0 { limit } else { -limit };
            }
        }
        let delta = target - mean;
        let mean_step = if age_days > 14 {
            let bias = if delta != 0 && mean <= target {
                16
            } else {
                -16
            };
            (delta + bias) / 32
        } else if age_days > 3 {
            (delta + if delta > 0 { 4 } else { -4 }) / 8
        } else {
            (delta + if delta > 0 { 1 } else { -1 }) / 2
        };
        mean += mean_step as i16 as i32;
        let absolute_deviation = (target - mean).abs();
        let (bias, divisor) = if age_days > 14 {
            (32, 64)
        } else if age_days > 3 {
            (8, 16)
        } else {
            (4, 8)
        };
        let bias = if absolute_deviation != dev && dev <= absolute_deviation {
            bias
        } else {
            -bias
        };
        dev += ((absolute_deviation - dev + bias) / divisor) as i16 as i32;
        if age_days > 3 {
            dev = if wide {
                dev.clamp(DEV_MIN[metric_index], INVALID)
            } else {
                dev.clamp(DEV_MIN[metric_index], DEV_MAX[metric_index])
            };
        }
        mean = if wide {
            mean.clamp(0, INVALID)
        } else {
            // C++ comparisons retain the supplied minimum even if it exceeds
            // the maximum; clamp() would panic on that original edge case.
            if mean < config.mean_min_x8[metric_index] as i32 {
                config.mean_min_x8[metric_index] as i32
            } else {
                mean.min(MEAN_MAX[metric_index])
            }
        };
        self.mean_x8 = mean.clamp(0, INVALID);
        self.dev_x8 = dev.clamp(0, INVALID);
        Ok(())
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

/// The current native clamp tables define precisely nine metric indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidBaselineMetric(pub usize);

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
}
