//! Four small signal operations used by the heart-rate-variability features: windowing of
//! timestamped samples, a sorted-index search, IIR cascade filtering, and peak finding.
//!
//! Each function is defined by the behavior stated in its documentation, including the handling
//! of edge cases (empty input, grids that do not yet cover a window, plateaus). The unit tests
//! are hand-built from these statements.

/// Window grid over ascending millisecond timestamps `t` (Unix or relative, possibly negative)
/// for windows of `win_s` seconds, with `w = trunc(win_s * 1000)` milliseconds.
///
/// The grid starts at `start`, the largest multiple of `w` that is at most `t[0]` (a floor, also
/// for negative times). It stops at `end = (t[n-1] - t[n-1] % w) + w`, where `%` truncates
/// toward zero. The number of windows is `(end - start) / w`; the result is empty when `t` is
/// empty, `w <= 0`, or `end <= start`.
///
/// Window `k` is `[e_k - w, e_k)` with `e_k = start + (k + 1) w`. The first returned vector holds
/// the end times `e_k`. The second holds one row per window,
/// `[i5, i1, i0, i_end]`: the first index whose time is at least `e_k - 300000`,
/// `e_k - 60000`, `e_k - w` and `e_k - 1` respectively (an index of `t.len()` when no
/// sample qualifies). Windows that the grid has not yet extended far enough to look back over
/// are marked with -1: `i5` is -1 when `e_k - 300000 < start`, and the other three
/// entries are -1 when `e_k - 60000 < start`.
pub fn window_grid(t: &[i64], win_s: f64) -> (Vec<i64>, Vec<[i64; 4]>) {
    let w = (win_s * 1000.0) as i64;
    let (Some(&first), Some(&last)) = (t.first(), t.last()) else {
        return (Vec::new(), Vec::new());
    };
    if w <= 0 {
        return (Vec::new(), Vec::new());
    }
    let start = first.div_euclid(w) * w;
    let end = (last - last % w) + w;
    if end <= start {
        return (Vec::new(), Vec::new());
    }
    let first_at_least = |v: i64| t.partition_point(|&x| x < v) as i64;
    (1..=(end - start) / w)
        .map(|k| {
            let e = start + k * w;
            let mut row = [
                first_at_least(e - 300_000),
                first_at_least(e - 60_000),
                first_at_least(e - w),
                first_at_least(e - 1),
            ];
            if e - 300_000 < start {
                row[0] = -1;
            }
            if e - 60_000 < start {
                row[1..].fill(-1);
            }
            (e, row)
        })
        .unzip()
}

/// For ascending query values `xnew`, the interval of the ascending grid `x` that holds each
/// one: result `j` satisfies `xnew <= x[j + 1]` and is the smallest such index that is not below
/// the previous result (the first result starts from 0). A value equal to a grid point belongs
/// to the lower interval. The index never exceeds the last interval, `x.len() - 2` (0 when `x`
/// has fewer than two points), so values beyond the grid use the last interval.
pub fn index_search(x: &[i64], xnew: &[i64]) -> Vec<usize> {
    let cap = x.len().saturating_sub(2);
    let mut j = 0usize;
    xnew.iter()
        .map(|&v| {
            while j < cap && v > x[j + 1] {
                j += 1;
            }
            j
        })
        .collect()
}

/// Filters `x` through a cascade of second-order sections in float32. Each section is a row
/// `[b0, b1, b2, a0, a1, a2]`; the coefficients are divided by `a0` first, the sections are
/// applied in order (each over the whole signal), and state starts at zero.
///
/// One section is evaluated per sample in transposed direct form II with fused multiply-adds:
/// `y = fma(x, b0, z1)`, `z1' = z2 + fma(x, b1, -(a1 y))`, `z2' = fma(x, b2, -(a2 y))`.
pub fn iir_cascade(sos: &[[f32; 6]], x: &[f32]) -> Vec<f32> {
    sos.iter().fold(x.to_vec(), |mut sig, s| {
        let [b0, b1, b2, a1, a2] = [s[0], s[1], s[2], s[4], s[5]].map(|c| c / s[3]);
        let (mut z1, mut z2) = (0.0f32, 0.0f32);
        for v in &mut sig {
            let xi = *v;
            let y = xi.mul_add(b0, z1);
            let next_z1 = z2 + xi.mul_add(b1, -(a1 * y));
            z2 = xi.mul_add(b2, -(a2 * y));
            z1 = next_z1;
            *v = y;
        }
        sig
    })
}

/// Peaks of `x`, including flat-topped ones. Index `i` (with `1 <= i <= n - 2`) starts a peak
/// when `x[i-1] < x[i]` and the first later sample that differs from `x[i]` is lower. If that
/// differing sample is higher, or all later samples equal `x[i]`, there is no peak. A peak is
/// reported at the middle of its plateau, `(i + j - 1) / 2` truncated, where `j` is the index
/// of that first differing sample. A rise that continues to the last sample is therefore not a
/// peak, but a plateau that ends at the second-to-last sample followed by a drop is.
pub fn peak_search(x: &[f32]) -> Vec<i64> {
    let n = x.len();
    let mut peaks = Vec::new();
    for i in 1..n.saturating_sub(1) {
        if !(x[i - 1] < x[i]) {
            continue;
        }
        if let Some(j) = (i + 1..n).find(|&j| x[j] != x[i]) {
            if x[j] < x[i] {
                peaks.push(((i + j - 1) / 2) as i64);
            }
        }
    }
    peaks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_single_plateau_and_edges() {
        // single peak at 1; plateau 3..4 reports 3; plateau 7..9 reports 8
        let x = [0., 1., 0., 2., 2., 1., 0., 3., 3., 3., 0.];
        assert_eq!(peak_search(&x), vec![1, 3, 8]);
        // a rise that reaches the last sample, or a plateau running to the end, is no peak
        assert!(peak_search(&[0., 1., 1.]).is_empty());
        assert!(peak_search(&[0., 1., 2.]).is_empty());
        assert!(peak_search(&[5., 1., 5.]).is_empty());
        assert!(peak_search(&[1., 2.]).is_empty());
        assert!(peak_search(&[]).is_empty());
        // plateau ending at the second-to-last sample then dropping is a peak
        assert_eq!(peak_search(&[0., 1., 1., 0.]), vec![1]);
        // plateau followed by a higher sample is not a peak, the higher sample is not either
        assert!(peak_search(&[0., 1., 1., 2., 3.]).is_empty());
    }

    #[test]
    fn find_indices_lower_interval_on_ties() {
        let x = [0, 10, 20, 35, 50];
        let xn = [0, 5, 10, 11, 20, 21, 50];
        assert_eq!(index_search(&x, &xn), vec![0, 0, 0, 1, 1, 2, 3]);
    }

    #[test]
    fn find_indices_clamps_to_the_last_interval() {
        assert_eq!(index_search(&[0, 10, 20], &[5, 25, 99]), vec![0, 1, 1]);
        assert_eq!(index_search(&[0], &[1]), vec![0]);
        assert_eq!(index_search(&[0, 10], &[50]), vec![0]);
        assert!(index_search(&[], &[]).is_empty());
    }

    #[test]
    fn find_indices_never_decreases() {
        // the second query is below the interval already reached
        assert_eq!(index_search(&[0, 10, 20, 30], &[25, 5]), vec![2, 2]);
    }

    #[test]
    fn biquad_first_sample_and_a0_scaling() {
        let sos = [
            [2.2805e-4f32, 4.561e-4, 2.2805e-4, 1.0, -1.2561, 0.42303],
            [1.0, 2.0, 1.0, 1.0, -1.2191, 0.51352],
        ];
        let y = iir_cascade(&sos, &[1.0, 0.0, 0.0, 0.0]);
        assert!((y[0] - sos[0][0] * sos[1][0]).abs() < 1e-12);
        // a0 = 2 halves the numerator: y0 = x0 b0 / a0
        assert_eq!(
            iir_cascade(&[[1.0, 0.0, 0.0, 2.0, 0.0, 0.0]], &[4.0, 0.0]),
            vec![2.0, 0.0]
        );
        assert!(iir_cascade(&[], &[1.0, 2.0]) == vec![1.0, 2.0]);
    }

    #[test]
    fn biquad_matches_a_float64_recursion() {
        // one-pole low-pass y[n] = 0.5 x[n] + 0.5 y[n-1] as the section [0.5 0 0 | 1 -0.5 0]
        let x: Vec<f32> = (0..50).map(|i| ((i * 7) % 5) as f32).collect();
        let y = iir_cascade(&[[0.5, 0.0, 0.0, 1.0, -0.5, 0.0]], &x);
        let mut prev = 0.0f64;
        for (i, &v) in x.iter().enumerate() {
            prev = 0.5 * v as f64 + 0.5 * prev;
            assert!((y[i] as f64 - prev).abs() < 1e-5, "sample {i}");
        }
    }

    #[test]
    fn biquad_two_pole_recursion() {
        // y[n] = x[n] + 0.5 y[n-1] - 0.25 y[n-2]; impulse response 1, 0.5, 0, -0.125
        let y = iir_cascade(&[[1.0, 0.0, 0.0, 1.0, -0.5, 0.25]], &[1.0, 0.0, 0.0, 0.0]);
        assert_eq!(y, vec![1.0, 0.5, 0.0, -0.125]);
    }

    #[test]
    fn windows_cover_the_grid() {
        // samples every second from 1000 s to 1399 s; 30 s windows
        let t: Vec<i64> = (0..400).map(|i| 1_000_000 + i * 1000).collect();
        let (ts, win) = window_grid(&t, 30.0);
        // start 990 s (a multiple of 30 s), end (1399 s - 1399 s % 30 s) + 30 s = 1410 s
        assert_eq!(ts.len(), 14);
        assert_eq!(ts[0], 1_020_000);
        assert_eq!(ts[13], 1_410_000);
        // the first windows are too short to look back one minute
        // the first window cannot look back one minute from the grid start
        assert_eq!(win[0], [-1; 4]);
    }

    #[test]
    fn windows_rows_by_hand() {
        let t: Vec<i64> = (0..400).map(|i| 1_000_000 + i * 1000).collect();
        let (ts, win) = window_grid(&t, 30.0);
        // window 2 ends at 1080 s: e - 60 s = 1020 s -> index 20, e - 30 s = 1050 s -> 50,
        // e - 1 ms = 1079.999 s -> 80; e - 300 s is before the grid start -> -1
        assert_eq!(ts[2], 1_080_000);
        assert_eq!(win[2], [-1, 20, 50, 80]);
        // window 10 ends at 1320 s: e - 300 s = 1020 s -> index 20
        assert_eq!(ts[10], 1_320_000);
        assert_eq!(win[10], [20, 260, 290, 320]);
        // an index past the data is t.len()
        assert_eq!(win[13][3], 400);
    }

    #[test]
    fn windows_floor_a_negative_start() {
        // -45 s .. +45 s: the grid starts at -60 s and ends at +60 s
        let t: Vec<i64> = (-45..=45).map(|s| s * 1000).collect();
        let (ts, _) = window_grid(&t, 30.0);
        assert_eq!(ts, vec![-30_000, 0, 30_000, 60_000]);
    }

    #[test]
    fn windows_degenerate_inputs() {
        assert_eq!(window_grid(&[], 30.0), (vec![], vec![]));
        assert_eq!(window_grid(&[5, 6], 0.0), (vec![], vec![]));
        assert_eq!(window_grid(&[5, 6], -3.0), (vec![], vec![]));
    }
}
