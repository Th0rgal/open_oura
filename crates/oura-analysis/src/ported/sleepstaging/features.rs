//! Feature stages between the low-level HRV rows and the classifier: high-level HRV, motion,
//! temperature, time and demographic features, and the combiner. Every table is a [`Mat`] whose first
//! column is the epoch timestamp (float32 seconds from the reference minute).

use super::feature_columns::{COMBINED_COLUMNS, LOW_COLUMNS};
use super::mat::Mat;
use super::rolling as r;

/// Why a stage rejected its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeatureError {
    /// A joined table does not line up with the epoch grid.
    Join(&'static str),
    /// The input has no rows or the wrong number of columns.
    Shape(&'static str),
}

impl std::fmt::Display for FeatureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Join(s) => write!(f, "feature join failed: {s}"),
            Self::Shape(s) => write!(f, "bad feature input: {s}"),
        }
    }
}

impl std::error::Error for FeatureError {}

/// Range of the combined matrix's columns by source (column 0 is the timestamp).
pub const ACM_COLS: std::ops::Range<usize> = 1..27;
pub const DEMO_COLS: std::ops::Range<usize> = 27..30;
pub const HRV_COLS: std::ops::Range<usize> = 30..177;
pub const TEMP_COLS: std::ops::Range<usize> = 177..188;
pub const TIME_COLS: std::ops::Range<usize> = 188..192;

// ---- High-level HRV features ----------------------------------------------------------

/// One column of the high-level HRV table, named as in [`COMBINED_COLUMNS`]. The base columns and
/// the rolling variants are robust-scaled; `_raw` copies and the two quality columns are not.
fn hrv_high_column(name: &str, low: &Mat) -> Vec<f32> {
    let col = |base: &str| -> Vec<f32> {
        let j = LOW_COLUMNS
            .iter()
            .position(|&c| c == base)
            .unwrap_or_else(|| panic!("no low-level column {base}"));
        low.col(j)
    };
    if let Some(base) = name.strip_suffix("_raw") {
        return col(base);
    }
    if let Some(base) = name.strip_suffix("_rollmean_cen15min") {
        return r::robust_scale(&r::rolling_mean(&col(base), 31, true, 15));
    }
    if let Some(base) = name.strip_suffix("_rollstd_cen15min") {
        return r::robust_scale(&r::rolling_std(&col(base), 31, true, 15, 0));
    }
    if let Some(base) = name.strip_suffix("_rollmean_past10min") {
        return r::robust_scale(&r::rolling_mean(&col(base), 20, false, 10));
    }
    if name == "hrv_quality_1min" || name == "hrv_quality_5min" {
        return col(name);
    }
    r::robust_scale(&col(name))
}

/// High-level HRV features: the timestamp column and the 147 HRV columns (rolling means and
/// standard deviations over 15 minutes either side, a trailing 10-minute mean, robust scaling).
pub fn hrv_low_to_high_level(low: &Mat) -> Mat {
    let mut cols = vec![low.col(0)];
    for name in &COMBINED_COLUMNS[HRV_COLS] {
        cols.push(hrv_high_column(name, low));
    }
    Mat::from_cols(&cols)
}

// ---- 30 s epoch rounding --------------------------------------------------------------

/// Timestamps to the nearest multiple of 30 (float32 arithmetic) and
/// only the first row of each run of equal timestamps kept.
fn round_timestamps_30s(data: &mut Mat) -> Mat {
    for i in 0..data.rows {
        let t = data.data[i * data.cols];
        data.data[i * data.cols] = ((t / 30.0 + 0.5).floor()) * 30.0;
    }
    let mut keep = vec![true; data.rows];
    for i in 1..data.rows {
        keep[i] = data.at(i, 0) != data.at(i - 1, 0);
    }
    let mut out = Vec::new();
    for i in 0..data.rows {
        if keep[i] {
            out.extend_from_slice(data.row(i));
        }
    }
    Mat {
        rows: out.len() / data.cols,
        cols: data.cols,
        data: out,
    }
}

// ---- Motion features ------------------------------------------------------------------

/// Motion features. `data` has one row per accelerometer epoch: relative time in
/// seconds, then MAD iqr, max, mean and dANGLEZ iqr, max, mean. Returns the timestamp and the 26
/// motion columns of the combined matrix. The timestamps of `data` are rounded in place.
pub fn extract_motion_features(data: &mut Mat) -> Result<Mat, FeatureError> {
    if data.cols != 7 || data.rows == 0 {
        return Err(FeatureError::Shape("motion input needs 7 columns"));
    }
    let mut d = round_timestamps_30s(data);
    for i in 0..d.rows {
        for j in 1..4 {
            let v = d.data[i * 7 + j];
            d.data[i * 7 + j] = (v * 4095.0 + 0.5).floor() / 4095.0;
        }
        for j in 4..7 {
            let v = d.data[i * 7 + j];
            d.data[i * 7 + j] = (v * 255.0 + 0.5).floor() / 255.0;
        }
    }
    let ts = d.col(0);
    let mut cols = vec![ts];
    // per statistic (MAD, dANGLEZ), in the order iqr, max, mean
    for group in 0..2 {
        let base = 1 + 3 * group;
        let (iqr, max, mean) = (d.col(base), d.col(base + 1), d.col(base + 2));
        let norm = |x: Vec<f32>| r::robust_scale(&x);
        for x in [&iqr, &max] {
            cols.push(x.clone());
            cols.push(norm(r::rolling_mean(x, 2, false, 0)));
            cols.push(norm(r::rolling_mean(x, 10, false, 5)));
        }
        cols.push(mean.clone());
        cols.push(norm(r::rolling_mean(&mean, 2, false, 0)));
        cols.push(norm(r::rolling_mean(&mean, 10, false, 5)));
        cols.push(norm(r::rolling_lids(&mean, 31, true, 15)));
        cols.push(norm(r::rolling_max(&mean, 31, true, 15)));
        cols.push(norm(r::rolling_median(&mean, 61, true, 30)));
        cols.push(norm(r::rolling_std(&mean, 31, true, 15, 1)));
    }
    Ok(Mat::from_cols(&cols))
}

// ---- Temperature features -------------------------------------------------------------

/// Temperature features. `data` has one row per temperature sample: relative
/// time in seconds and degrees Celsius. Returns the timestamp, the temperature (below 10 is
/// missing) and ten scaled columns. The timestamps of `data` are rounded in place.
pub fn extract_temp_features(data: &mut Mat) -> Result<Mat, FeatureError> {
    if data.cols != 2 || data.rows == 0 {
        return Err(FeatureError::Shape("temperature input needs 2 columns"));
    }
    let d = round_timestamps_30s(data);
    let ts = d.col(0);
    let temp: Vec<f32> = d
        .col(1)
        .into_iter()
        .map(|v| if v < 10.0 { f32::NAN } else { v })
        .collect();
    let norm = |x: Vec<f32>| r::robust_scale(&x);
    let cols = vec![
        ts,
        temp.clone(),
        norm(temp.clone()),
        norm(r::rolling_max(&temp, 31, true, 15)),
        norm(r::rolling_max(&temp, 20, false, 10)),
        norm(r::rolling_mean(&temp, 31, true, 15)),
        norm(r::rolling_mean(&temp, 20, false, 10)),
        norm(r::rolling_mean(&temp, 2, false, 0)),
        norm(r::rolling_min(&temp, 31, true, 15)),
        norm(r::rolling_min(&temp, 20, false, 10)),
        norm(r::rolling_std(&temp, 31, true, 15, 0)),
        norm(r::rolling_std(&temp, 20, false, 10, 0)),
    ];
    Ok(Mat::from_cols(&cols))
}

// ---- time and demographics ------------------------------------------------------------

/// Remainder `a - b floor(a / b)` for float32 with `b > 0`.
fn remainder(a: f32, b: f32) -> f32 {
    let m = a % b;
    if m != 0.0 && m < 0.0 {
        m + b
    } else {
        m
    }
}

/// Float32 arithmetic progression from `start` below `end`: the length from the float64 quotient, each
/// value `start + i * step` in float64 rounded to float32.
pub fn arange_f32(start: f32, end: f32, step: f32) -> Vec<f32> {
    let n = ((end as f64 - start as f64) / step as f64).ceil().max(0.0) as usize;
    (0..n)
        .map(|i| (start as f64 + i as f64 * step as f64) as f32)
        .collect()
}

/// The 30 s epoch grid of a bedtime `[start, end]` in float32 seconds: both ends floored to a
/// multiple of 30. `inclusive_end` appends the epoch that starts at the floored end.
pub fn epoch_grid(start: f32, end: f32, inclusive_end: bool) -> Vec<f32> {
    let s = start - remainder(start, 30.0);
    let e = end - remainder(end, 30.0);
    arange_f32(s, if inclusive_end { e + 1.0 } else { e }, 30.0)
}

/// Time features: timestamp, `sin(pi (i + 0.5) / n)`, `exp(-0.003 i)`, `i / 120`
/// and `i / (n - 1)` over the epoch grid of the bedtime. The sine and exponential are the float32
/// functions of [`super::numeric_ops`].
pub fn extract_time_features(bedtime: [f32; 2]) -> Mat {
    let ts = epoch_grid(bedtime[0], bedtime[1], true);
    let n = ts.len();
    let step = (std::f64::consts::PI / n as f64) as f32;
    let idx: Vec<f32> = (0..n).map(|i| i as f32).collect();
    let cosine: Vec<f32> = idx
        .iter()
        .map(|&i| super::numeric_ops::sin_f32((i + 0.5) * step))
        .collect();
    let decay: Vec<f32> = idx
        .iter()
        .map(|&i| super::numeric_ops::exp_f32(i * (-0.003f64 as f32)))
        .collect();
    let hours: Vec<f32> = idx.iter().map(|&i| i / 120.0).collect();
    let linear: Vec<f32> = idx.iter().map(|&i| i / (n as f32 - 1.0)).collect();
    Mat::from_cols(&[ts, cosine, decay, hours, linear])
}

/// Demographic features: age and BMI rounded to a multiple of 5 (missing when
/// outside 0 to 150 and 0 to 100) and sex (-1 becomes 2, 0 is missing), repeated per epoch.
pub fn extract_demographic_features(timestamps: &[f32], age: f32, bmi: f32, sex: f32) -> Mat {
    let age = if age < 0.0 || age > 150.0 {
        f32::NAN
    } else {
        age
    };
    let bmi = if bmi < 0.0 || bmi > 100.0 {
        f32::NAN
    } else {
        bmi
    };
    let sex = if sex == -1.0 {
        2.0
    } else if sex == 0.0 {
        f32::NAN
    } else {
        sex
    };
    let age = (age / 5.0 + 0.5).floor() * 5.0;
    let bmi = (bmi / 5.0 + 0.5).floor() * 5.0;
    let n = timestamps.len();
    Mat::from_cols(&[
        timestamps.to_vec(),
        vec![age; n],
        vec![bmi; n],
        vec![sex; n],
    ])
}

// ---- left join of the feature blocks ------------------------------------------------

/// Left join of several tables: a matrix with the left timestamp column and the data columns of each
/// right table placed at the rows whose timestamp it shares with the left grid (NaN elsewhere).
///
/// Each right table is cropped to the left's time range, then the rows are located through the
/// positions of repeated values in the sorted union of both timestamp lists, which equals the
/// position in the left grid when every right timestamp lies on it.
pub fn left_join_multiple(left: &Mat, right: &[&Mat]) -> Result<Mat, FeatureError> {
    let rows = left.rows;
    let cols0 = left.cols + right.iter().map(|m| m.cols - 1).sum::<usize>();
    let mut joined = Mat::new(rows, cols0, f32::NAN);
    for i in 0..rows {
        joined.data[i * cols0..i * cols0 + left.cols].copy_from_slice(left.row(i));
    }
    let left_ts = left.col(0);
    let mut offset = left.cols;
    for rr in right {
        if rr.rows > 0 {
            let cropped = crop_right_to_left(&left_ts, rr);
            let indices = find_row_indices(&left_ts, &cropped.col(0));
            if !indices.is_empty() {
                if indices.len() != cropped.rows || indices.iter().any(|&i| i >= rows) {
                    return Err(FeatureError::Join(
                        "right timestamps are not on the left grid",
                    ));
                }
                for (k, &i) in indices.iter().enumerate() {
                    for c in 1..cropped.cols {
                        joined.data[i * cols0 + offset + c - 1] = cropped.at(k, c);
                    }
                }
            }
        }
        offset += rr.cols - 1;
    }
    Ok(joined)
}

fn crop_right_to_left(left: &[f32], right: &Mat) -> Mat {
    let first = left[0];
    let last = left[left.len() - 1];
    let col = right.col(0);
    // argmax of an int mask is the first true entry (0 when none), argmin the first false entry
    let start = col.iter().position(|&v| v >= first).unwrap_or(0);
    let mut end = col.iter().position(|&v| !(v <= last)).unwrap_or(0);
    if end == 0 {
        end = start + left.len();
    }
    let end = end.min(right.rows);
    let start = start.min(end);
    Mat {
        rows: end - start,
        cols: right.cols,
        data: right.data[start * right.cols..end * right.cols].to_vec(),
    }
}

/// Positions, in the sorted union of both timestamp lists, of the values that occur more than once.
fn find_row_indices(left: &[f32], right: &[f32]) -> Vec<usize> {
    let mut all: Vec<f32> = left.iter().chain(right).copied().collect();
    all.sort_by(|a, b| a.total_cmp(b));
    let mut out = Vec::new();
    let (mut pos, mut i) = (0usize, 0usize);
    while i < all.len() {
        let mut j = i;
        while j < all.len() && all[j] == all[i] {
            j += 1;
        }
        if j - i > 1 {
            out.push(pos);
        }
        pos += 1;
        i = j;
    }
    out
}

/// Feature combiner: acm, demographics, high-level HRV, temperature and time tables on
/// the epoch grid of the bedtime (including the epoch at the floored end).
pub fn combine_features(
    acm: &Mat,
    hrv: &Mat,
    temp: &Mat,
    demo: &Mat,
    time: &Mat,
    bedtime: [f32; 2],
) -> Result<Mat, FeatureError> {
    let grid = epoch_grid(bedtime[0], bedtime[1], true);
    left_join_multiple(&Mat::from_cols(&[grid]), &[acm, demo, hrv, temp, time])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arange_and_grid() {
        assert_eq!(arange_f32(0.0, 91.0, 30.0), vec![0.0, 30.0, 60.0, 90.0]);
        assert_eq!(
            epoch_grid(-44.0, 61.0, true),
            vec![-60.0, -30.0, 0.0, 30.0, 60.0]
        );
        assert_eq!(
            epoch_grid(-44.0, 61.0, false),
            vec![-60.0, -30.0, 0.0, 30.0]
        );
    }

    #[test]
    fn join_places_rows_on_the_grid() {
        let right = Mat::from_rows(2, vec![30.0, 1.0, 60.0, 2.0, 120.0, 3.0]);
        let j = left_join_multiple(
            &Mat::from_cols(&[vec![0.0, 30.0, 60.0, 90.0, 120.0]]),
            &[&right],
        )
        .unwrap();
        assert_eq!(j.cols, 2);
        let v: Vec<f32> = j.col(1);
        assert!(v[0].is_nan() && v[3].is_nan());
        assert_eq!((v[1], v[2], v[4]), (1.0, 2.0, 3.0));
        // rows outside the left range are cropped
        let wide = Mat::from_rows(2, vec![-30.0, 9.0, 0.0, 1.0, 30.0, 2.0, 300.0, 9.0]);
        let j = left_join_multiple(&Mat::from_cols(&[vec![0.0, 30.0]]), &[&wide]).unwrap();
        assert_eq!(j.col(1), vec![1.0, 2.0]);
    }

    #[test]
    fn rounding_keeps_the_first_of_a_run() {
        let mut m = Mat::from_rows(2, vec![1.0, 10.0, 14.0, 11.0, 31.0, 12.0, 70.0, 13.0]);
        let r = round_timestamps_30s(&mut m);
        // 1 and 14 round to 0, 31 to 30, 70 to 60
        assert_eq!(r.col(0), vec![0.0, 30.0, 60.0]);
        assert_eq!(r.col(1), vec![10.0, 12.0, 13.0]);
    }

    #[test]
    fn demographics_round_to_five() {
        let d = extract_demographic_features(&[0.0, 30.0], 28.0, 24.5, 2.0);
        assert_eq!(d.row(0), &[0.0, 30.0, 25.0, 2.0]);
        let d = extract_demographic_features(&[0.0], -1.0, 150.0, -1.0);
        assert!(d.at(0, 1).is_nan() && d.at(0, 2).is_nan());
        assert_eq!(d.at(0, 3), 2.0);
    }

    #[test]
    fn time_features_over_the_grid() {
        let t = extract_time_features([0.0, 90.0]);
        assert_eq!(t.rows, 4);
        assert_eq!(t.col(0), vec![0.0, 30.0, 60.0, 90.0]);
        assert_eq!(t.at(3, 4), 1.0);
        assert_eq!(t.at(0, 2), 1.0);
        assert!((t.at(1, 1) - (std::f32::consts::PI * 1.5 / 4.0).sin()).abs() < 1e-7);
    }
}
