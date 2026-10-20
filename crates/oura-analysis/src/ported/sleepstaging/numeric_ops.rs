//! Float32 reductions and transforms used by the feature pipeline: `sum`, `mean`, `std`,
//! `quantile`, `trapz`, `corrcoef2` and a real FFT.
//!
//! Elementwise results are rounded to float32 in a fixed operation order. Sums are
//! accumulated in float64 and rounded once, the covariance products of
//! [`corrcoef2`] are a fused multiply-add chain, and the transform is the float64 mixed-radix
//! [`rfft_f64`].

/// Sum of a float32 slice: float64 accumulation rounded once to float32.
pub fn sum(x: &[f32]) -> f32 {
    x.iter().map(|&v| v as f64).sum::<f64>() as f32
}


/// `sin(x)` in float32 (`std`).
#[inline]
pub fn sin_f32(x: f32) -> f32 {
        x.sin()
}

/// `exp(x)` in float32 (`std`).
#[inline]
pub fn exp_f32(x: f32) -> f32 {
        x.exp()
}

/// Mean of a float32 vector: [`sum`] divided by `n` in float32.
pub fn mean(x: &[f32]) -> f32 {
    sum(x) / x.len() as f32
}

/// Standard deviation; `unbiased` divides by `n - 1` (the sample standard deviation), otherwise by `n`.
pub fn std(x: &[f32], unbiased: bool) -> f32 {
    // the float32 mean, then a float64 sum of squared deviations
    let n = x.len() as f64;
    let m = mean(x) as f64;
    let ss = x
        .iter()
        .map(|&v| (v as f64 - m) * (v as f64 - m))
        .sum::<f64>();
    (ss / if unbiased { n - 1.0 } else { n }).sqrt() as f32
}

/// Linear interpolation `lerp(a, b, w)` in float32 with a fused multiply-add:
/// `a + w (b - a)` for `|w| < 0.5`, otherwise `b - (b - a)(1 - w)`.
pub fn lerp(a: f32, b: f32, w: f32) -> f32 {
    if w.abs() < 0.5 {
        w.mul_add(b - a, a)
    } else {
        (-(b - a)).mul_add(1.0 - w, b)
    }
}

/// Quantiles `q` of `x` with linear interpolation, in float32: the rank is
/// `q * (n - 1)` rounded to float32, and the result is `lerp(sorted[floor], sorted[ceil],
/// rank - floor)`. Any NaN in `x` gives NaN.
pub fn quantile(x: &[f32], q: &[f32]) -> Vec<f32> {
    if x.iter().any(|v| v.is_nan()) {
        return vec![f32::NAN; q.len()];
    }
    let mut s = x.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let nm1 = (s.len() - 1) as f32;
    q.iter()
        .map(|&qi| {
            let r = qi * nm1;
            let lo = r as usize;
            let hi = (r.ceil() as usize).min(s.len() - 1);
            lerp(s[lo], s[hi], r - lo as f32)
        })
        .collect()
}

/// Trapezoid integral of a float32 vector with spacing `dx`: `(sum(y) - (y[0] + y[-1]) * 0.5) * dx`.
pub fn trapz(y: &[f32], dx: f32) -> f32 {
    if y.is_empty() {
        return 0.0;
    }
    let s = sum(y);
    (s - (y[0] + y[y.len() - 1]) * 0.5) * dx
}

/// Integer linspace from `start` to `end` in `steps` points: the step is computed in float64 and the
/// first half counts up from `start`, the second half down from `end`, both truncated.
pub fn linspace_i64(start: i64, end: i64, steps: usize) -> Vec<i64> {
    if steps == 0 {
        return Vec::new();
    }
    if steps == 1 {
        return vec![start];
    }
    let step = (end - start) as f64 / (steps - 1) as f64;
    let half = steps / 2;
    (0..steps)
        .map(|i| {
            if i < half {
                (start as f64 + step * i as f64) as i64
            } else {
                (end as f64 - step * (steps - i - 1) as f64) as i64
            }
        })
        .collect()
}

/// Inner product of two centered rows, as an element of the covariance product:
/// a fused multiply-add chain in index order.
fn cov_dot(u: &[f32], v: &[f32]) -> f32 {
    u.iter().zip(v).fold(0.0f32, |acc, (&p, &q)| p.mul_add(q, acc))
}


/// Correlation coefficient of two equally long rows (`corrcoef(vstack([a, b]))[0, 1]`):
/// covariance from mean-centered rows (float32 centering), divided by `n - 1`, then by both
/// standard deviations, clipped to [-1, 1].
pub fn corrcoef2(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len();
    let (ma, mb) = (mean(a), mean(b));
    let ca: Vec<f32> = a.iter().map(|&v| v - ma).collect();
    let cb: Vec<f32> = b.iter().map(|&v| v - mb).collect();
    let norm = (n - 1) as f32;
    let (caa, cbb, cab) = (
        cov_dot(&ca, &ca) / norm,
        cov_dot(&cb, &cb) / norm,
        cov_dot(&ca, &cb) / norm,
    );
    let (sa, sb) = (caa.sqrt(), cbb.sqrt());
    (cab / sa / sb).clamp(-1.0, 1.0)
}

/// Real FFT of `x` zero-padded or truncated to `nfft` samples: the `nfft / 2 + 1` coefficients
/// as `(re, im)` float32 pairs, computed by [`rfft_f64`].
pub fn rfft(x: &[f32], nfft: usize) -> Vec<(f32, f32)> {
    rfft_f64(x, nfft)
}

/// Real FFT in float64 rounded to float32: a mixed-radix decimation-in-time transform for any
/// length (cost is `nfft` times the sum of its prime factors).
pub fn rfft_f64(x: &[f32], nfft: usize) -> Vec<(f32, f32)> {
    let mut data: Vec<(f64, f64)> = (0..nfft)
        .map(|i| (x.get(i).copied().unwrap_or(0.0) as f64, 0.0))
        .collect();
    let tw: Vec<(f64, f64)> = (0..nfft)
        .map(|j| {
            let a = -2.0 * std::f64::consts::PI * j as f64 / nfft as f64;
            (a.cos(), a.sin())
        })
        .collect();
    let mut out = vec![(0.0, 0.0); nfft];
    fft_rec(&data, 1, nfft, &mut out, &tw, nfft);
    data.clear();
    out.iter()
        .take(nfft / 2 + 1)
        .map(|&(r, i)| (r as f32, i as f32))
        .collect()
}

/// Decimation-in-time transform of `n` samples read from `input` at `stride`, written to `out`.
fn fft_rec(
    input: &[(f64, f64)],
    stride: usize,
    n: usize,
    out: &mut [(f64, f64)],
    tw: &[(f64, f64)],
    nfft: usize,
) {
    if n == 1 {
        out[0] = input[0];
        return;
    }
    let p = (2..=n).find(|d| n % d == 0).unwrap();
    let m = n / p;
    for r in 0..p {
        fft_rec(
            &input[r * stride..],
            stride * p,
            m,
            &mut out[r * m..(r + 1) * m],
            tw,
            nfft,
        );
    }
    let sub: Vec<(f64, f64)> = out[..n].to_vec();
    let step = nfft / n;
    for k in 0..m {
        for q in 0..p {
            let kk = k + q * m;
            let (mut sr, mut si) = (0.0, 0.0);
            for r in 0..p {
                let (xr, xi) = sub[r * m + k];
                let (wr, wi) = tw[(r * kk * step) % nfft];
                sr += xr * wr - xi * wi;
                si += xr * wi + xi * wr;
            }
            out[kk] = (sr, si);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantile_interpolates_linearly() {
        let x = [4.0f32, 1.0, 3.0, 2.0];
        // sorted 1 2 3 4; rank 0.75 * 3 = 2.25 -> 3 + 0.25
        assert_eq!(
            quantile(&x, &[0.0, 0.25, 0.5, 0.75, 1.0]),
            vec![1.0, 1.75, 2.5, 3.25, 4.0]
        );
        assert!(quantile(&[1.0, f32::NAN], &[0.5])[0].is_nan());
    }

    #[test]
    fn linspace_truncates() {
        assert_eq!(linspace_i64(5, 6, 7), vec![5, 5, 5, 5, 5, 5, 6]);
        assert_eq!(linspace_i64(0, 1000, 5), vec![0, 250, 500, 750, 1000]);
        assert_eq!(linspace_i64(7, 9, 1), vec![7]);
    }

    #[test]
    fn trapz_of_a_ramp() {
        // y = 0 1 2 3 with dx 2: area 2 * (0.5 + 1 + 1 + 1.5 - ...) = 2 * 4.5
        assert_eq!(trapz(&[0.0, 1.0, 2.0, 3.0], 2.0), 9.0);
    }

    #[test]
    fn corrcoef_of_a_line_and_its_negative() {
        let a = [1.0f32, 2.0, 3.0, 4.0];
        assert!((corrcoef2(&a, &[2.0, 4.0, 6.0, 8.0]) - 1.0).abs() < 1e-6);
        assert!((corrcoef2(&a, &[8.0, 6.0, 4.0, 2.0]) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn rfft_matches_a_direct_dft() {
        for &(len, nfft) in &[(7usize, 12usize), (10, 15), (9, 13), (16, 16)] {
            let x: Vec<f32> = (0..len).map(|i| ((i * 5 + 3) % 7) as f32 - 2.5).collect();
            let got = rfft(&x, nfft);
            assert_eq!(got.len(), nfft / 2 + 1);
            for (k, &(re, im)) in got.iter().enumerate() {
                let (mut sr, mut si) = (0.0f64, 0.0f64);
                for (j, &v) in x.iter().enumerate() {
                    let a = -2.0 * std::f64::consts::PI * (j * k) as f64 / nfft as f64;
                    sr += v as f64 * a.cos();
                    si += v as f64 * a.sin();
                }
                assert!(
                    (re as f64 - sr).abs() < 1e-5 && (im as f64 - si).abs() < 1e-5,
                    "n {nfft} k {k}"
                );
            }
        }
    }

    #[test]
    fn mean_and_std() {
        let x = [1.0f32, 2.0, 3.0, 4.0];
        assert_eq!(mean(&x), 2.5);
        assert!((std(&x, true) - 1.2909944).abs() < 1e-6);
        assert!((std(&x, false) - 1.1180340).abs() < 1e-6);
    }
}

