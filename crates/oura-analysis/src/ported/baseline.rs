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
            let bias = if delta != 0 && self.mean_x8 <= sample_x8 { 16 } else { -16 };
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
        let bias2 = if absd != self.dev_x8 && self.dev_x8 <= absd { mag } else { -mag };
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
}
