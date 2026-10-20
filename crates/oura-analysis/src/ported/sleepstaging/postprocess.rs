//! Classifier column selection and labels, smoothing of the probabilities, extra HRV columns, the
//! bedtime grid, aggregation from 30 s to 5 min epochs, and summary metrics.

use super::feature_columns::{COMBINED_COLUMNS, SUMMARY_LABELS, EXTRA_COLUMNS, KEEP_COLUMNS, LOW_COLUMNS};
use super::features::{epoch_grid, left_join_multiple, FeatureError};
use super::mat::Mat;
use super::rolling::nanquantile;
use super::numeric_ops as t;
use super::classifier::StageClassifier;
use crate::ported::sleep_classifier::{N_CLASSES, N_FEATURES};

/// Stage codes.
pub const DEEP: f32 = 1.0;
pub const LIGHT: f32 = 2.0;
pub const REM: f32 = 3.0;
pub const WAKE: f32 = 4.0;

/// Indices into the combined matrix of the columns the classifier reads:
/// the combined columns are walked once and those equal to the next kept column are selected.
pub fn keep_indices() -> Vec<usize> {
    let mut idx = Vec::new();
    let mut k = 0;
    for (a, c) in COMBINED_COLUMNS.iter().enumerate() {
        if k < KEEP_COLUMNS.len() && KEEP_COLUMNS[k] == *c {
            idx.push(a);
            k += 1;
        }
    }
    idx
}

/// Classifies every row: `[timestamp, label, p_deep, p_light, p_rem, p_wake]` per epoch.
/// The label is the first most probable class plus one.
pub fn classify(clf: &dyn StageClassifier, combined: &Mat) -> Mat {
    let idx = keep_indices();
    debug_assert_eq!(idx.len(), KEEP_COLUMNS.len());
    let mut out = Mat::new(combined.rows, 2 + N_CLASSES, 0.0);
    for i in 0..combined.rows {
        let row = combined.row(i);
        let mut x = [0f32; N_FEATURES];
        for (k, &c) in idx[1..].iter().enumerate() {
            x[k] = row[c];
        }
        let p = clf.predict_proba(&x);
        let mut best = 0;
        for c in 1..N_CLASSES {
            if p[c] > p[best] {
                best = c;
            }
        }
        out.data[i * 6] = row[0];
        out.data[i * 6 + 1] = best as f32 + 1.0;
        out.data[i * 6 + 2..i * 6 + 6].copy_from_slice(&p);
    }
    out
}

fn argmax(v: &[f32]) -> usize {
    let mut best = 0;
    for (i, &x) in v.iter().enumerate() {
        if x > v[best] {
            best = i;
        }
    }
    best
}

/// `smooth_predictions` with `halfwin` 1: where the gap between the two largest probabilities is
/// under `thresh`, the class of an epoch becomes the argmax of the neighborhood mean weighted
/// `weights[1], weights[0], weights[1]` (class 3, wake, is left alone when `skip_wake`). Returns
/// the class index (0 to 3) per epoch.
pub fn smooth_predictions(proba: &Mat, thresh: f32, weights: [f32; 2], skip_wake: bool) -> Vec<f32> {
    let n = proba.rows;
    let mut pred: Vec<f32> = (0..n).map(|i| argmax(proba.row(i)) as f32).collect();
    if n < 3 {
        return pred;
    }
    let w = [weights[1], weights[0], weights[1]];
    for i in 0..n {
        let mut s: Vec<f32> = proba.row(i).to_vec();
        s.sort_by(|a, b| a.total_cmp(b));
        let confidence = s[N_CLASSES - 1] - s[N_CLASSES - 2];
        if !(confidence < thresh) || i < 1 || i >= n - 1 {
            continue;
        }
        if skip_wake && argmax(proba.row(i)) == 3 {
            continue;
        }
        let mut mean = [0f32; N_CLASSES];
        for c in 0..N_CLASSES {
            let mut num = 0f32;
            let mut den = 0f32;
            for (k, wk) in w.iter().enumerate() {
                num += wk * proba.at(i - 1 + k, c);
                den += wk;
            }
            mean[c] = num / den;
        }
        pred[i] = argmax(&mean) as f32;
    }
    pred
}

/// The low-level HRV values appended for the summary metrics:
/// the combined matrix with four more columns joined on the timestamp.
pub fn concatenate_extra_hrv_low_features(combined: &Mat, low: &Mat) -> Result<Mat, FeatureError> {
    let mut cols = vec![low.col(0)];
    for name in EXTRA_COLUMNS {
        let base = name.strip_suffix("_raw").unwrap_or(name);
        let j = LOW_COLUMNS.iter().position(|&c| c == base).expect("low-level column");
        cols.push(low.col(j));
    }
    left_join_multiple(combined, &[&Mat::from_cols(&cols)])
}

/// Smooths the labels, adds the extra HRV columns and puts the
/// stages and the features on the bedtime's epoch grid (exclusive of the floored end epoch);
/// epochs without a stage get label 4 and NaN probabilities.
pub fn postprocess(stages: &Mat, combined: &Mat, low: &Mat, bedtime: [f32; 2]) -> Result<(Mat, Mat), FeatureError> {
    let mut stages = stages.clone();
    let proba = Mat::from_cols(&(2..6).map(|j| stages.col(j)).collect::<Vec<_>>());
    let pred = smooth_predictions(&proba, 0.2, [1.0, 0.5], true);
    for i in 0..stages.rows {
        stages.data[i * 6 + 1] = pred[i] + 1.0;
    }
    let combined = concatenate_extra_hrv_low_features(combined, low)?;
    let grid = Mat::from_cols(&[epoch_grid(bedtime[0], bedtime[1], false)]);
    let mut stages = left_join_multiple(&grid, &[&stages])?;
    for i in 0..stages.rows {
        let v = &mut stages.data[i * 6 + 1];
        if v.is_nan() {
            *v = 4.0;
        }
    }
    let combined = left_join_multiple(&grid, &[&combined])?;
    Ok((stages, combined))
}

/// The stage of each block of ten epochs. The mode in
/// the priority order wake, deep, REM, light (ties go to the earlier one), wake when at least
/// four epochs are wake; a trailing block of five to nine epochs is also kept. The trailing block is forced to deep, not wake, when it holds four deep epochs.
pub fn aggregate_30sec_to_5min(stages: &[f32]) -> Vec<f32> {
    const PRIORITY: [f32; 4] = [4.0, 1.0, 3.0, 2.0];
    const MIN_SAMPLES: usize = 5;
    const FORCE_WAKE: usize = 4;
    let mut order = [0usize; 4];
    for (i, p) in PRIORITY.iter().enumerate() {
        order[*p as usize - 1] = i;
    }
    let remapped: Vec<usize> = stages.iter().map(|&s| order[s as usize - 1]).collect();
    let mode = |w: &[usize]| -> usize {
        let mut counts = [0usize; 4];
        for &v in w {
            counts[v] += 1;
        }
        let mut best = 0;
        for c in 1..4 {
            if counts[c] > counts[best] {
                best = c;
            }
        }
        best
    };
    let n = remapped.len();
    let wake = order[3];
    let mut out: Vec<usize> = remapped
        .chunks_exact(10)
        .map(|w| if w.iter().filter(|&&v| v == wake).count() >= FORCE_WAKE { wake } else { mode(w) })
        .collect();
    if n % 10 >= MIN_SAMPLES {
        let tail = &remapped[n - n % 10..];
        out.push(if tail.iter().filter(|&&v| v == 1).count() >= FORCE_WAKE { 1 } else { mode(tail) });
    }
    out.into_iter().map(|r| PRIORITY[r]).collect()
}

fn col_index(name: &str) -> usize {
    COMBINED_COLUMNS
        .iter()
        .chain(EXTRA_COLUMNS.iter())
        .position(|&c| c == name)
        .unwrap_or_else(|| panic!("no column {name}"))
}

fn fraction(flags: impl Iterator<Item = bool>, n: usize) -> f32 {
    let ones: Vec<f32> = flags.map(|b| if b { 1.0 } else { 0.0 }).collect();
    t::sum(&ones) / n as f32
}

fn nanmean(x: &[f32]) -> f32 {
    let valid: Vec<f32> = x.iter().copied().filter(|v| !v.is_nan()).collect();
    let z: Vec<f32> = x.iter().map(|&v| if v.is_nan() { 0.0 } else { v }).collect();
    t::sum(&z) / valid.len() as f32
}

/// Fraction of sleep epochs (all but the last) followed by a wake epoch.
fn hyp_fragmentation(hypno: &[f32]) -> f32 {
    let sleep: Vec<usize> = (0..hypno.len()).filter(|&i| hypno[i] != 4.0).collect();
    if sleep.len() <= 1 {
        return 0.0;
    }
    let flags: Vec<f32> = sleep[..sleep.len() - 1].iter().map(|&i| if hypno[i + 1] == 4.0 { 1.0 } else { 0.0 }).collect();
    t::sum(&flags) / flags.len() as f32
}

/// 16 values named by [`SUMMARY_LABELS`] from the stages and the combined
/// matrix with the extra HRV columns.
pub fn summary_metrics(stages: &Mat, combined: &Mat, flag_rerun: f32) -> [f32; 16] {
    let n = stages.rows;
    let hypno = stages.col(1);
    let p0 = stages.col(2);
    let not_nan: Vec<usize> = (0..n).filter(|&i| !p0[i].is_nan()).collect();
    let (pad_start, pad_end) = if n > not_nan.len() {
        (not_nan[0] as f32, (n - (not_nan[not_nan.len() - 1] + 1)) as f32)
    } else {
        (0.0, 0.0)
    };
    let prop_wake = hypno.iter().filter(|&&h| h == 4.0).count() as f64 / n as f64;
    let pct_fragmentation = hyp_fragmentation(&hypno);
    let conf: Vec<f32> = (0..n)
        .map(|i| {
            let row = &stages.row(i)[2..6];
            if row.iter().any(|v| v.is_nan()) { f32::NAN } else { row.iter().copied().fold(f32::MIN, f32::max) }
        })
        .collect();
    let avg_conf = nanmean(&conf);
    let madmean = combined.col(col_index("acm_MAD_mean"));
    let prop_mad = fraction(madmean.iter().map(|&v| v > 0.01), n);
    let ibiq = combined.col(col_index("hrv_quality_1min"));
    let mean_ibi_quality = nanmean(&ibiq);
    let sleep_ibiq: Vec<f32> = (0..n).filter(|&i| hypno[i] != 4.0).map(|i| ibiq[i]).collect();
    let mean_ibi_quality_asleep = nanmean(&sleep_ibiq);
    let below85 = fraction(ibiq.iter().map(|&v| v < 85.0), n);
    let below95 = fraction(ibiq.iter().map(|&v| v < 95.0), n);
    let rmssd = combined.col(col_index("hrv_rMSSD_1min_raw"));
    let rmssd_p95 = nanquantile(&rmssd, &[0.95f64 as f32])[0];
    let ibi_missing = fraction(ibiq.iter().map(|v| v.is_nan()), n);
    let acm_missing = fraction(madmean.iter().map(|v| v.is_nan()), n);
    let temp = combined.col(col_index("temp"));
    let temp_missing = fraction(temp.iter().map(|v| v.is_nan()), n);
    let pct = |p: f32| (p as f64 * 100.0) as f32;
    let m = [
        pad_start,
        pad_end,
        (prop_wake * 100.0) as f32,
        pct_fragmentation * 100.0,
        (avg_conf as f64 * 100.0) as f32,
        pct(ibi_missing),
        pct(acm_missing),
        pct(temp_missing),
        pct(prop_mad),
        mean_ibi_quality,
        mean_ibi_quality_asleep,
        pct(below85),
        pct(below95),
        rmssd_p95,
        0.0,
        flag_rerun,
    ];
    debug_assert_eq!(m.len(), SUMMARY_LABELS.len());
    m
}

/// A session of 15 to 180 minutes asleep with more than 30% of
/// epochs above MAD 0.01, more than 25% scored wake and fragmentation above 5 is relabeled all
/// wake, and `flag_false_nap_cleared` is set.
pub fn clear_false_naps(stages: &mut Mat, debug: &mut [f32; 16]) {
    let asleep = (0..stages.rows).filter(|&i| stages.at(i, 1) != 4.0).count();
    let duration = asleep as f32 / 2.0;
    if duration >= 15.0 && duration < 180.0 {
        let idx = |name: &str| SUMMARY_LABELS.iter().position(|&l| l == name).unwrap();
        let exclude = debug[idx("pct_motion_above_threshold")] > 30.0 && debug[idx("pct_wake")] > 25.0 && debug[idx("pct_fragmentation")] > 5.0;
        if exclude {
            for i in 0..stages.rows {
                stages.data[i * 6 + 1] = 4.0;
            }
            debug[idx("flag_false_nap_cleared")] = 1.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_indices_follow_the_column_order() {
        let idx = keep_indices();
        assert_eq!(idx.len(), 183);
        assert_eq!(idx[0], 0);
        assert!(idx.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn smoothing_changes_only_ambiguous_epochs() {
        // epochs 0..4: light, light, (light 0.45 vs rem 0.40), light, light
        let p = |l: f32, r: f32| [0.0, l, r, 1.0 - l - r];
        let rows = [p(0.9, 0.05), p(0.9, 0.05), p(0.45, 0.40), p(0.9, 0.05), p(0.9, 0.05)];
        let m = Mat::from_rows(4, rows.iter().flatten().copied().collect());
        let s = smooth_predictions(&m, 0.2, [1.0, 0.5], true);
        // the middle epoch is pulled to light: (0.5*0.9 + 0.45 + 0.5*0.9) / 2 vs rem (0.5*.05*2 + .4) / 2
        assert_eq!(s, vec![1.0, 1.0, 1.0, 1.0, 1.0]);
        // an ambiguous wake epoch is skipped
        let rows = [p(0.9, 0.05), [0.0, 0.3, 0.1, 0.55], p(0.9, 0.05)];
        let m = Mat::from_rows(4, rows.iter().flatten().copied().collect());
        assert_eq!(smooth_predictions(&m, 0.4, [1.0, 0.5], true)[1], 3.0);
        assert_eq!(smooth_predictions(&m, 0.4, [1.0, 0.5], false)[1], 1.0);
    }

    #[test]
    fn aggregation_blocks_and_priorities() {
        // block 1: 6 light, 4 deep -> light mode; block 2: 4 wake -> forced wake;
        // block 3: 5 REM, 5 light -> tie resolved by priority (REM before light)
        let mut s = vec![2.0; 6];
        s.extend([1.0; 4]);
        s.extend([4.0; 4]);
        s.extend([2.0; 6]);
        s.extend([3.0; 5]);
        s.extend([2.0; 5]);
        assert_eq!(aggregate_30sec_to_5min(&s), vec![2.0, 4.0, 3.0]);
        // five trailing epochs form a block, four do not
        let mut s = vec![2.0; 10];
        s.extend([1.0; 5]);
        assert_eq!(aggregate_30sec_to_5min(&s), vec![2.0, 1.0]);
        s.pop();
        assert_eq!(aggregate_30sec_to_5min(&s), vec![2.0]);
    }

    #[test]
    fn fragmentation_counts_sleep_to_wake_transitions() {
        // light, light, wake, light, wake: the sleep epochs are 0, 1 and 3; the last sleep epoch
        // is not checked, epoch 0 is followed by light and epoch 1 by wake
        let h = [2.0, 2.0, 4.0, 2.0, 4.0];
        assert_eq!(hyp_fragmentation(&h), 0.5);
        assert_eq!(hyp_fragmentation(&[4.0, 2.0]), 0.0);
    }
}
