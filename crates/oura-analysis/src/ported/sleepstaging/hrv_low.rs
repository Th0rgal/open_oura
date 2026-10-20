//! Low-level HRV features, the first HRV stage. Cleaned beats go in (time relative to the bedtime midpoint in ms, and
//! `[ibi_ms, amplitude, validity]` per beat); out comes one row of 36 float32 values per 30 s
//! window: the window end time in seconds, 17 one-minute features and 18 five-minute features,
//! in the column order the high-level stage ([`super::features::hrv_low_to_high_level`]) expects.
//!
//! Stages, in order: [`trim_edge_timestamps`], [`repair_time`],
//! [`cumulative_time`], [`drop_invalid_ibi`], windows from [`window_grid`], then per window
//! [`window_hrv_features`] on the last minute (`nfft` 400) and the last five minutes (1333),
//! each only with at least 20 valid beats.

use super::ops::{iir_cascade, window_grid, index_search, peak_search};
use super::numeric_ops as t;

/// Columns of a row of the output.
pub const N_LOW: usize = 36;
const N_1MIN: usize = 17;
const N_5MIN: usize = 18;
const FREQ: f64 = 4.0;
const NFFT_1MIN: usize = 400;
const NFFT_5MIN: usize = 1333;
const MIN_BEATS: usize = 20;
/// Bands in mHz.
const HF_BAND: [f32; 2] = [150.0, 400.0];
const LF_BAND: [f32; 2] = [40.0, 150.0];
const VLF_BAND: [f32; 2] = [3.0, 40.0];
/// Output column j is column `HRV_LOW_ARGSORT[j]` of the window row.
pub const HRV_LOW_ARGSORT: [usize; 36] = [
    0, 12, 30, 11, 29, 6, 23, 10, 28, 8, 26, 7, 24, 9, 27, 25, 15, 33, 5, 22, 14, 32, 3, 20, 13,
    31, 2, 19, 17, 35, 1, 18, 16, 34, 4, 21,
];
/// The 12th-order Butterworth band-pass for breathing, six sections `[b0 b1 b2 a0 a1 a2]`.
pub const BR_SOS: [[f32; 6]; 6] = [
    [
        0.0002280496119055897,
        0.0004560992238111794,
        0.0002280496119055897,
        1.0,
        -1.2560985088348389,
        0.42303213477134705,
    ],
    [1.0, 2.0, 1.0, 1.0, -1.219077467918396, 0.5135181546211243],
    [1.0, 2.0, 1.0, 1.0, -1.6959424018859863, 0.7255245447158813],
    [1.0, -2.0, 1.0, 1.0, -1.3681423664093018, 0.7872657179832458],
    [1.0, -2.0, 1.0, 1.0, -1.8397016525268555, 0.8581900596618652],
    [1.0, -2.0, 1.0, 1.0, -1.9383554458618164, 0.9541428089141846],
];

/// Why the stage rejected its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HrvLowError {
    /// Fewer than four beats (the start and end searches use quarters of the series) or fewer than three
    /// (the validity filter looks at triples).
    TooShort,
    /// `time` and `values` differ in length.
    LengthMismatch,
}

impl std::fmt::Display for HrvLowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort => write!(f, "too few beats for the HRV stage"),
            Self::LengthMismatch => write!(f, "beat times and values differ in length"),
        }
    }
}

impl std::error::Error for HrvLowError {}

/// Keeps `[argmin(time[..n/4]), argmax(time[3n/4..]) + 3n/4]`, which drops a few leading and
/// trailing beats with stray timestamps. Returns the kept index range.
pub fn trim_edge_timestamps(time: &[i64]) -> Result<std::ops::Range<usize>, HrvLowError> {
    let n = time.len();
    if n < 4 {
        return Err(HrvLowError::TooShort);
    }
    let first_by = |s: &[i64], better: fn(i64, i64) -> bool| {
        let mut best = 0;
        for (i, &v) in s.iter().enumerate() {
            if better(v, s[best]) {
                best = i;
            }
        }
        best
    };
    let start = first_by(&time[..n / 4], |a, b| a < b);
    let off = n * 3 / 4;
    let end = first_by(&time[off..], |a, b| a > b) + off;
    Ok(start..end + 1)
}

/// Repairs timestamps that do not increase: each run of non-increasing samples is replaced by
/// the preceding good timestamp (an integer linspace from it to one millisecond later, truncated,
/// which is the preceding timestamp for every sample but the last), up to the next sample that
/// exceeds it.
pub fn repair_time(time: &mut [i64]) {
    let n = time.len();
    if n == 0 {
        return;
    }
    let idx: Vec<usize> = (1..n).filter(|&i| time[i] - time[i - 1] <= 0).collect();
    let mut i = 0;
    while i < idx.len() {
        let start = idx[i];
        let tstart = time[start - 1];
        let stop = (start..n).find(|&j| time[j] > tstart).unwrap_or(n);
        let len = stop - start;
        let tstop = tstart + 1;
        let tstop0 = if stop < n { time[stop] } else { tstop };
        let tstop1 = if tstop0 > tstart + 1 {
            tstart + 1
        } else {
            tstop0
        };
        let lin = t::linspace_i64(tstart, tstop1, len + 2);
        time[start..stop].copy_from_slice(&lin[1..len + 1]);
        let mut i1 = i;
        while i1 < idx.len() && idx[i1] < stop {
            i1 += 1;
        }
        i = i1;
    }
}

/// Cumulative time of each beat in ms: the beat interval (truncated to an integer), replaced by
/// the timestamp difference where the timestamps jump by more than `gap_thresh_s`.
pub fn cumulative_time(time_ms: &[i64], ibi: &[f32], gap_thresh_s: i64) -> Vec<i64> {
    let mut acc = 0i64;
    (0..time_ms.len())
        .map(|i| {
            let tdiff = if i == 0 {
                0
            } else {
                time_ms[i] - time_ms[i - 1]
            };
            acc += if tdiff > gap_thresh_s * 1000 {
                tdiff
            } else {
                ibi[i] as i64
            };
            acc
        })
        .collect()
}

/// Marks a beat invalid (NaN interval, `i64::MIN` time) unless its validity is 1 and so are
/// those of its two neighbors (the first and last beat copy their neighbor's flag).
pub fn drop_invalid_ibi(time: &[i64], ibi: &[f32], validity: &[f32]) -> (Vec<i64>, Vec<f32>) {
    let n = time.len();
    let valid: Vec<bool> = validity.iter().map(|&v| v == 1.0).collect();
    let tri: Vec<bool> = (0..n.saturating_sub(2))
        .map(|i| valid[i] && valid[i + 1] && valid[i + 2])
        .collect();
    let ok = |i: usize| tri[i.saturating_sub(1).min(tri.len() - 1)];
    let tf = (0..n)
        .map(|i| if ok(i) { time[i] } else { i64::MIN })
        .collect();
    let ibif = (0..n)
        .map(|i| if ok(i) { ibi[i] } else { f32::NAN })
        .collect();
    (tf, ibif)
}

/// Linear interpolation of `y` over `x` at `xnew` using [`index_search`], float32 arithmetic
/// (the fraction divides two integers converted to float32).
fn interp1d(x: &[i64], y: &[f32], xnew: &[i64]) -> Vec<f32> {
    let low = index_search(x, xnew);
    low.iter()
        .zip(xnew)
        .map(|(&l, &xn)| {
            let (lv, hv) = (y[l], y[l + 1]);
            let fraction = (xn - x[l]) as f32 / (x[l + 1] - x[l]) as f32;
            lv + (hv - lv) * fraction
        })
        .collect()
}

/// Periodogram: bins in mHz and `|rfft(x, nfft)|^2 / (win f)`, interior bins doubled. The square
/// is the complex product `conj(z) z`, whose real part is `fma(re, re, im * im)`.
fn simple_periodogram(x: &[f32], f: f64, nfft: usize) -> (Vec<f32>, Vec<f32>) {
    let d = 0.001 / f;
    let scale = (1.0 / (nfft as f64 * d)) as f32;
    let bins: Vec<f32> = (0..nfft / 2 + 1).map(|i| i as f32 * scale).collect();
    let win = if nfft > x.len() { x.len() } else { nfft };
    let s = (1.0 / (win as f64 * f)) as f32;
    let mut psd: Vec<f32> = t::rfft(x, nfft)
        .iter()
        .map(|&(re, im)| re.mul_add(re, im * im) * s)
        .collect();
    let end = if nfft % 2 == 1 {
        psd.len()
    } else {
        psd.len() - 1
    };
    for v in &mut psd[1..end] {
        *v *= 2.0;
    }
    (bins, psd)
}

/// Band power in mHz bins, divided by 1000 (`calc_bandpower`).
fn calc_bandpower(bins: &[f32], psd: &[f32], band: [f32; 2]) -> f32 {
    let res = bins[1] - bins[0];
    let sel: Vec<f32> = bins
        .iter()
        .zip(psd)
        .filter(|(&b, _)| b >= band[0] && b <= band[1])
        .map(|(_, &p)| p)
        .collect();
    t::trapz(&sel, res) / 1000.0
}

/// Index of the first maximum of `psd` over the bins in `[lo, hi)`.
fn peak_in(bins: &[f32], psd: &[f32], band: [f32; 2]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for i in 0..bins.len() {
        if bins[i] >= band[0] && bins[i] < band[1] && best.map_or(true, |b| psd[i] > psd[b]) {
            best = Some(i);
        }
    }
    best
}

/// Breathing rate (breaths per minute) and its variability from the 4 Hz interpolated beat
/// series (`breathing_rate`): band-pass filter, peak search, peaks above 0.3 of the 75th
/// percentile peak that are more than 0.035 min apart, cycle lengths between consecutive ones.
pub fn breathing_rate(x: &[f32], freq: f64) -> (f32, f32) {
    let nan = (f32::NAN, f32::NAN);
    let st = (1.0 / freq) as f32;
    let bf = iir_cascade(&BR_SOS, x);
    if bf.len() <= 40 {
        return nan;
    }
    let locs = peak_search(&bf);
    let pks: Vec<f32> = locs.iter().map(|&l| bf[l as usize]).collect();
    if pks.len() <= 1 {
        return nan;
    }
    let mut sorted = pks.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let ix = (0.75 * sorted.len() as f64 + 0.5).floor() as i64 - 1;
    if ix < 0 || ix >= sorted.len() as i64 {
        return nan;
    }
    let peak75 = sorted[ix as usize];
    let pks0: Vec<f32> = pks.iter().map(|&p| p / peak75).collect();
    let time_pks: Vec<f32> = locs.iter().map(|&l| (l - 1) as f32 * st / 60.0).collect();
    let (mut sel_a, mut sel_b) = (Vec::new(), Vec::new());
    for j in 0..pks0.len() - 2 {
        if pks0[1 + j] > 0.3 && pks0[2 + j] > 0.3 && time_pks[2 + j] - time_pks[1 + j] > 0.035 {
            sel_a.push(time_pks[1 + j]);
            sel_b.push(time_pks[2 + j]);
        }
    }
    let cycles = sel_a.len();
    let cl: Vec<f32> = sel_a.iter().zip(&sel_b).map(|(&a, &b)| b - a).collect();
    let sum = t::sum(&cl);
    if cycles == 0 || !(sum > 0.0) {
        return nan;
    }
    let br = (1.0 / sum) * cycles as f32;
    if !(br < 22.0) {
        return nan;
    }
    let brv = if cl.len() < 2 {
        0.0
    } else {
        let r: Vec<f32> = cl.iter().map(|&v| 1.0 / v).collect();
        t::std(&r, false)
    };
    (br, brv)
}

/// HRV features on the beats of one window (`time` in ms, `nni` in ms, both without
/// invalid beats): 17 values for `onemin`, 18 otherwise (VLF power after LF power).
pub fn window_hrv_features(
    quality: f64,
    time: &[i64],
    nni: &[f32],
    freq: f64,
    nfft: usize,
    onemin: bool,
) -> Vec<f32> {
    let n = nni.len();
    let diff: Vec<f32> = nni.windows(2).map(|w| w[1] - w[0]).collect();
    let sq: Vec<f32> = diff.iter().map(|&d| d * d).collect();
    let hr: Vec<f32> = nni.iter().map(|&v| 60000.0f32 / v).collect();
    let rmssd = t::mean(&sq).sqrt();
    let q = t::quantile(&hr, &[0.0, 0.25, 0.5, 0.75, 1.0]);
    let (median_hr, iqr_hr) = (q[2], q[3] - q[1]);
    let range_nn = 60000.0f32 / q[0] - 60000.0f32 / q[4];
    let cv_nn = t::std(nni, false) / t::mean(nni);

    let t_ms: Vec<i64> = time.iter().map(|&v| v - time[0]).collect();
    let upsampled = (((t_ms[n - 1] as f32) * freq as f32) / 1000.0f32).floor() as i64;
    let end = (upsampled * 1000) as f64 / freq;
    let t_interp = t::linspace_i64(0, end as i64, (upsampled + 1) as usize);
    let pp = interp1d(&t_ms, nni, &t_interp);
    let m = t::mean(&pp);
    let pp0: Vec<f32> = pp.iter().map(|&v| v - m).collect();

    let (bins, psd) = simple_periodogram(&pp0, freq, nfft);
    let hf = calc_bandpower(&bins, &psd, HF_BAND);
    let lf = calc_bandpower(&bins, &psd, LF_BAND);
    let vlf = calc_bandpower(&bins, &psd, VLF_BAND);
    let (hf_max_f, hf_amp) = match peak_in(&bins, &psd, HF_BAND) {
        Some(i) => (bins[i] / 1000.0, psd[i] / hf),
        None => (f32::NAN, f32::NAN),
    };
    let lf_max_f = peak_in(&bins, &psd, LF_BAND).map_or(f32::NAN, |i| bins[i] / 1000.0);
    let (br, brv) = breathing_rate(&pp0, freq);

    let (mut mean_zci, mut cv_zci) = (f32::NAN, f32::NAN);
    let neg: Vec<bool> = pp0.iter().map(|v| v.is_sign_negative()).collect();
    let cross: Vec<f32> = (0..neg.len().saturating_sub(1))
        .filter(|&i| neg[i] != neg[i + 1])
        .map(|i| i as f32)
        .collect();
    if !cross.is_empty() {
        let zi: Vec<f32> = cross.windows(2).map(|w| w[1] - w[0]).collect();
        if zi.len() > 2 {
            mean_zci = t::mean(&zi);
            cv_zci = t::std(&zi, false) / mean_zci;
        }
    }

    let (a, b) = (&nni[..n - 1], &nni[1..]);
    let sqrt2 = 1.4142135623730951f64 as f32;
    let x1: Vec<f32> = a.iter().zip(b).map(|(&p, &q)| (p - q) / sqrt2).collect();
    let xsum: Vec<f32> = a.iter().zip(b).map(|(&p, &q)| (p + q) / sqrt2).collect();
    let csi = t::std(&xsum, true) / t::std(&x1, true);
    let rrr = t::corrcoef2(a, b);

    let mut out = vec![rmssd, median_hr, iqr_hr, range_nn, cv_nn, hf, lf];
    if !onemin {
        out.push(vlf);
    }
    out.extend([
        hf_max_f,
        lf_max_f,
        hf_amp,
        br,
        brv,
        mean_zci,
        cv_zci,
        csi,
        rrr,
        quality as f32,
    ]);
    out
}

/// The 36-column feature row of each 30 s window of the night, trimmed of leading rows with
/// missing values and with the columns permuted by [`HRV_LOW_ARGSORT`].
pub fn hrv_window_features(
    timestamp_ms: &[i64],
    values: &[[f32; 3]],
) -> Result<Vec<[f32; N_LOW]>, HrvLowError> {
    if timestamp_ms.len() != values.len() {
        return Err(HrvLowError::LengthMismatch);
    }
    let range = trim_edge_timestamps(timestamp_ms)?;
    if range.len() < 3 {
        return Err(HrvLowError::TooShort);
    }
    let mut time: Vec<i64> = timestamp_ms[range.clone()].to_vec();
    let vals = &values[range];
    repair_time(&mut time);
    let ibi: Vec<f32> = vals.iter().map(|v| v[0]).collect();
    let validity: Vec<f32> = vals.iter().map(|v| v[2]).collect();
    let cum = cumulative_time(&time, &ibi, 5);
    let (time_f, ibi_f) = drop_invalid_ibi(&cum, &ibi, &validity);
    let (win_ts, windows) = window_grid(&time, 30.0);

    let slice_valid = |from: i64, to: i64| -> (Vec<i64>, Vec<f32>, usize) {
        let (a, b) = (from as usize, to as usize);
        let total = b.saturating_sub(a);
        let mut tt = Vec::new();
        let mut xx = Vec::new();
        for k in a..b.min(ibi_f.len()) {
            if !ibi_f[k].is_nan() {
                tt.push(time_f[k]);
                xx.push(ibi_f[k]);
            }
        }
        (tt, xx, total)
    };

    let mut rows: Vec<[f32; N_LOW]> = Vec::with_capacity(windows.len());
    for (k, w) in windows.iter().enumerate() {
        let (i5, i1, end) = (w[0], w[1], w[3]);
        let mut row = [f32::NAN; N_LOW];
        row[0] = (win_ts[k] as f32) / 1000.0;
        if i1 >= 0 {
            let (tt, xx, total) = slice_valid(i1, end);
            if xx.len() >= MIN_BEATS {
                let quality = xx.len() as f64 / total as f64 * 100.0;
                let f = window_hrv_features(quality, &tt, &xx, FREQ, NFFT_1MIN, true);
                debug_assert_eq!(f.len(), N_1MIN);
                row[1..1 + N_1MIN].copy_from_slice(&f);
            }
        }
        if i5 >= 0 {
            let (tt, xx, total) = slice_valid(i5, end);
            if xx.len() >= MIN_BEATS {
                let quality = xx.len() as f64 / total as f64 * 100.0;
                let f = window_hrv_features(quality, &tt, &xx, FREQ, NFFT_5MIN, false);
                debug_assert_eq!(f.len(), N_5MIN);
                row[1 + N_1MIN..].copy_from_slice(&f);
            }
        }
        rows.push(row);
    }
    let first = rows
        .iter()
        .position(|r| r.iter().all(|v| !v.is_nan()))
        .unwrap_or(0);
    Ok(rows[first..]
        .iter()
        .map(|r| {
            let mut o = [0.0f32; N_LOW];
            for (j, &c) in HRV_LOW_ARGSORT.iter().enumerate() {
                o[j] = r[c];
            }
            o
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_time_replaces_runs_that_do_not_increase() {
        let mut t = vec![10, 20, 30, 25, 28, 29, 40, 50, 50, 60];
        repair_time(&mut t);
        assert_eq!(
            t,
            vec![10, 20, 30, 30, 30, 30, 40, 50, 50, 60]
                .iter()
                .enumerate()
                .map(|(i, &v)| if i == 8 { 50 } else { v })
                .collect::<Vec<_>>()
        );
        // a duplicate at the end has no later sample to bound it
        let mut t = vec![5, 6, 6];
        repair_time(&mut t);
        assert_eq!(t, vec![5, 6, 6]);
    }

    #[test]
    fn drop_bad_ends_searches_the_outer_quarters() {
        let t: Vec<i64> = vec![9, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 20, 3, 21, 22];
        // min of the first four is index 1; max of the last four is index 15
        assert_eq!(trim_edge_timestamps(&t).unwrap(), 1..16);
        assert!(trim_edge_timestamps(&[1, 2, 3]).is_err());
    }

    #[test]
    fn cumsum_uses_gaps_and_filter_needs_three_valid_beats() {
        let time = [0, 800, 1600, 9000, 9800];
        let ibi = [800.0, 800.0, 799.0, 810.0, 790.0];
        // the third beat follows a 7.4 s gap: its interval is the gap
        assert_eq!(
            cumulative_time(&time, &ibi, 5),
            vec![800, 1600, 2399, 9799, 10589]
        );
        let (tf, ibif) = drop_invalid_ibi(
            &[1, 2, 3, 4, 5],
            &[1., 2., 3., 4., 5.],
            &[1., 1., 1., 0., 1.],
        );
        assert_eq!(tf, vec![1, 2, i64::MIN, i64::MIN, i64::MIN]);
        assert_eq!(ibif[..2], [1.0, 2.0]);
        assert!(ibif[2].is_nan());
    }

    #[test]
    fn steady_beats_give_expected_features() {
        // 5 minutes of a 75 bpm series with a 0.25 Hz respiratory sinusoid on the intervals
        let mut time = Vec::new();
        let mut nni = Vec::new();
        let mut tm = 0f64;
        while tm < 300_000.0 {
            let ibi = 800.0 + 40.0 * (2.0 * std::f64::consts::PI * 0.25 * tm / 1000.0).sin();
            time.push(tm as i64);
            nni.push(ibi as f32);
            tm += ibi;
        }
        let f = window_hrv_features(100.0, &time, &nni, 4.0, 1333, false);
        assert_eq!(f.len(), 18);
        assert!((f[1] - 75.0).abs() < 2.0, "median HR {}", f[1]);
        // the dominant frequency in the HF band is 0.25 Hz
        assert!((f[8] - 0.25).abs() < 0.02, "HF peak {}", f[8]);
        assert_eq!(f[17], 100.0);
        let g = window_hrv_features(100.0, &time, &nni, 4.0, 400, true);
        assert_eq!(g.len(), 17);
    }
}

