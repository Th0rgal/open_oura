//! NaN-aware rolling statistics over windows of 30 s epochs and robust scaling by the 5th to
//! 95th percentile, in float32.

use super::numeric_ops as t;

/// `nansum` of a window: NaN counts as zero; summed with [`t::sum`].
fn nansum(win: &[f32]) -> f32 {
    let z: Vec<f32> = win.iter().map(|&v| if v.is_nan() { 0.0 } else { v }).collect();
    t::sum(&z)
}

fn valid_count(win: &[f32]) -> usize {
    win.iter().filter(|v| !v.is_nan()).count()
}

/// Quantile of one window at float32 quantile `q`: NaNs sorted last and ignored, rank
/// `q * (count - 1)` in float32, linear interpolation. All-NaN gives NaN.
pub fn nanquantile(x: &[f32], q: &[f32]) -> Vec<f32> {
    let mut s: Vec<f32> = x.iter().copied().filter(|v| !v.is_nan()).collect();
    if s.is_empty() {
        return vec![f32::NAN; q.len()];
    }
    s.sort_by(|a, b| a.total_cmp(b));
    let nm1 = (s.len() - 1) as f32;
    q.iter()
        .map(|&qi| {
            let r = (qi * nm1).max(0.0);
            let lo = r as usize;
            let hi = (r.ceil() as usize).min(s.len() - 1);
            t::lerp(s[lo], s[hi], r - lo as f32)
        })
        .collect()
}

/// The windows of `x`: NaN padding of `window / 2` on both sides when
/// `center`, otherwise `window - 1` on the left; one window per sample. `None` when the window
/// is longer than the series. A window is masked when it holds fewer than `min_periods` values
/// (`min_periods` 0 means all of them).
struct Rolled {
    padded: Vec<f32>,
    window: usize,
    min_periods: usize,
}

impl Rolled {
    fn new(x: &[f32], window: usize, center: bool, min_periods: usize) -> Option<Self> {
        if window > x.len() {
            return None;
        }
        let min_periods = if min_periods == 0 { window } else { min_periods };
        let (left, right) = if center { (window / 2, window / 2) } else { (window - 1, 0) };
        let mut padded = vec![f32::NAN; left];
        padded.extend_from_slice(x);
        padded.extend(std::iter::repeat(f32::NAN).take(right));
        Some(Self { padded, window, min_periods })
    }

    fn n(&self) -> usize {
        self.padded.len() + 1 - self.window
    }

    fn map(&self, f: impl Fn(&[f32]) -> f32) -> Vec<f32> {
        (0..self.n())
            .map(|i| {
                let w = &self.padded[i..i + self.window];
                if valid_count(w) < self.min_periods { f32::NAN } else { f(w) }
            })
            .collect()
    }
}

/// Mean over the valid values of each window.
pub fn rolling_mean(x: &[f32], window: usize, center: bool, min_periods: usize) -> Vec<f32> {
    match Rolled::new(x, window, center, min_periods) {
        None => vec![f32::NAN; x.len()],
        Some(r) => r.map(|w| nansum(w) / valid_count(w) as f32),
    }
}

/// Standard deviation of each window with `n - ddof` in the denominator.
pub fn rolling_std(x: &[f32], window: usize, center: bool, min_periods: usize, ddof: usize) -> Vec<f32> {
    match Rolled::new(x, window, center, min_periods) {
        None => vec![f32::NAN; x.len()],
        Some(r) => r.map(|w| {
            let n = valid_count(w) as f32;
            let mean = nansum(w) / n;
            let sq: Vec<f32> = w.iter().map(|&v| (v - mean) * (v - mean)).collect();
            (nansum(&sq) / (n - ddof as f32)).sqrt()
        }),
    }
}

/// Mean of `1 / (x + 1)` over the valid values of each window.
pub fn rolling_lids(x: &[f32], window: usize, center: bool, min_periods: usize) -> Vec<f32> {
    match Rolled::new(x, window, center, min_periods) {
        None => vec![f32::NAN; x.len()],
        Some(r) => r.map(|w| {
            let inv: Vec<f32> = w.iter().map(|&v| 1.0 / (v + 1.0)).collect();
            nansum(&inv) / valid_count(&inv) as f32
        }),
    }
}

fn rolling_quantile(x: &[f32], window: usize, center: bool, min_periods: usize, q: f32) -> Vec<f32> {
    match Rolled::new(x, window, center, min_periods) {
        None => vec![f32::NAN; x.len()],
        Some(r) => r.map(|w| nanquantile(w, &[q])[0]),
    }
}

pub fn rolling_median(x: &[f32], window: usize, center: bool, min_periods: usize) -> Vec<f32> {
    rolling_quantile(x, window, center, min_periods, 0.5)
}

pub fn rolling_max(x: &[f32], window: usize, center: bool, min_periods: usize) -> Vec<f32> {
    rolling_quantile(x, window, center, min_periods, 1.0)
}

pub fn rolling_min(x: &[f32], window: usize, center: bool, min_periods: usize) -> Vec<f32> {
    rolling_quantile(x, window, center, min_periods, 0.0)
}

/// Robust scaling: `(x - median) / (q95 - q05)` with NaN-ignoring
/// quantiles; `x` unchanged when the two quantiles coincide.
pub fn robust_scale(x: &[f32]) -> Vec<f32> {
    let qr = [5.0f32 / 100.0, 95.0f32 / 100.0, 50.0f32 / 100.0];
    let rv = nanquantile(x, &qr);
    let den = rv[1] - rv[0];
    if den == 0.0 {
        x.to_vec()
    } else {
        x.iter().map(|&v| (v - rv[2]) / den).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_mean_masks_short_windows() {
        let x = [1.0f32, 2.0, 3.0, 4.0];
        // trailing window of 2 needs both samples
        let m = rolling_mean(&x, 2, false, 0);
        assert!(m[0].is_nan());
        assert_eq!(&m[1..], &[1.5, 2.5, 3.5]);
        // centered window of 3 with min_periods 2: the ends have two samples
        assert_eq!(rolling_mean(&x, 3, true, 2), vec![1.5, 2.0, 3.0, 3.5]);
        // a window longer than the series gives NaN
        assert!(rolling_mean(&x, 9, true, 2).iter().all(|v| v.is_nan()));
    }

    #[test]
    fn rolling_std_ddof_and_nan() {
        let x = [1.0f32, 3.0, f32::NAN, 5.0];
        let s = rolling_std(&x, 3, true, 2, 1);
        assert!((s[0] - 1.4142135).abs() < 1e-6); // window [nan, 1, 3]
        assert!((s[2] - 1.4142135).abs() < 1e-6); // window [3, nan, 5]
        let p = rolling_std(&x, 3, true, 2, 0);
        assert!((p[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rolling_extremes_and_median() {
        let x = [4.0f32, 1.0, 3.0, 2.0];
        assert_eq!(rolling_max(&x, 3, true, 2), vec![4.0, 4.0, 3.0, 3.0]);
        assert_eq!(rolling_min(&x, 3, true, 2), vec![1.0, 1.0, 1.0, 2.0]);
        assert_eq!(rolling_median(&x, 3, true, 2), vec![2.5, 3.0, 2.0, 2.5]);
    }

    #[test]
    fn lids_averages_reciprocals() {
        let l = rolling_lids(&[0.0, 1.0, 3.0], 3, true, 3);
        assert!(l[0].is_nan());
        assert!((l[1] - (1.0 + 0.5 + 0.25) / 3.0).abs() < 1e-7);
    }

    #[test]
    fn robust_scale_centers_on_the_median() {
        let x: Vec<f32> = (0..101).map(|i| i as f32).collect();
        let s = robust_scale(&x);
        // q05 = 5, q95 = 95, median = 50
        assert_eq!(s[50], 0.0);
        assert!((s[95] - 0.5).abs() < 1e-6);
        // constant input is returned unchanged
        assert_eq!(robust_scale(&[2.0, 2.0, 2.0]), vec![2.0, 2.0, 2.0]);
        let nan = robust_scale(&[1.0, f32::NAN, 3.0]);
        assert!(nan[1].is_nan() && nan[0] < 0.0 && nan[2] > 0.0);
    }
}
