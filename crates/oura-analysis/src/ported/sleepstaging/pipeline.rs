//! The whole stager from cleaned ring data to the hypnogram.
//!
//! Inputs: motion rows (`[MAD iqr, max, mean, dANGLEZ iqr, max, mean]` per 30 s
//! epoch), cleaned beats (`[ibi_ms, amplitude, validity]`, validity 1 kept; the output of
//! `ibi_correction`), temperature samples, `[age, BMI, sex]` and the bedtime, all stamped in Unix
//! milliseconds. Times are measured from the minute-aligned midpoint of the bedtime and held as
//! float32 seconds.

use super::features::*;
use super::hrv_low::{hrv_window_features, HrvLowError, N_LOW};
use super::mat::Mat;
use super::postprocess::{
    aggregate_30sec_to_5min, classify, summary_metrics, clear_false_naps, postprocess,
};
use super::classifier::{StageClassifier, STAGER_CLASSES, STAGER_FEATURES};
use crate::ported::sleep_classifier::SleepClassifier;

/// Smallest and largest accepted timestamp (years 2010 and 2200) in ms.
const MIN_TS_MS: i64 = 1_262_304_000_000;
const MAX_TS_MS: i64 = 7_258_118_400_000;
const RERUN_WAKE_EPOCHS: usize = 30 * 2;
const PAD_WAKE_EPOCHS: usize = 10 * 2;

/// Raw inputs of the stager.
pub struct Inputs<'a> {
    pub acm_values: &'a [[f32; 6]],
    pub acm_timestamps: &'a [i64],
    pub ibi_values: &'a [[f32; 3]],
    pub ibi_timestamps: &'a [i64],
    pub temp_values: &'a [f32],
    pub temp_timestamps: &'a [i64],
    pub scalars: [f32; 3],
    pub bedtime: [i64; 2],
}

/// What the stager returns.
#[derive(Debug, Clone)]
pub struct Output {
    /// Epoch start times in Unix ms.
    pub timestamps_ms: Vec<i64>,
    /// Per epoch `[label, p_deep, p_light, p_rem, p_wake]`; labels are 1 deep, 2 light, 3 REM, 4 wake.
    pub stages: Mat,
    /// Feature matrix of the final run without the timestamp column.
    pub features: Mat,
    /// The 16 debug values (`SUMMARY_LABELS`).
    pub debug: [f32; 16],
}

/// Why the stager rejected its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagingError {
    Input(&'static str),
    Hrv(HrvLowError),
    Features(FeatureError),
    /// No classifier was supplied. [`stage_features`] and [`stage_night_or_features`] compute
    /// the features without one.
    NoClassifier,
    /// The classifier's feature or class count differs from the stager's
    /// ([`STAGER_FEATURES`], [`STAGER_CLASSES`]).
    ClassifierShape { features: usize, classes: usize },
}

impl std::fmt::Display for StagingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(s) => write!(f, "invalid stager input: {s}"),
            Self::Hrv(e) => write!(f, "{e}"),
            Self::Features(e) => write!(f, "{e}"),
            Self::NoClassifier => write!(f, "no sleep-stage classifier available"),
            Self::ClassifierShape { features, classes } => write!(
                f,
                "classifier has {features} features and {classes} classes; the stager needs {STAGER_FEATURES} and {STAGER_CLASSES}"
            ),
        }
    }
}

impl std::error::Error for StagingError {}

impl From<HrvLowError> for StagingError {
    fn from(e: HrvLowError) -> Self {
        Self::Hrv(e)
    }
}

impl From<FeatureError> for StagingError {
    fn from(e: FeatureError) -> Self {
        Self::Features(e)
    }
}

/// Input check: non-empty, matching lengths, timestamps between 2010 and 2200.
pub fn validate(i: &Inputs) -> Result<(), StagingError> {
    if i.acm_values.is_empty() || i.ibi_values.is_empty() || i.temp_values.is_empty() {
        return Err(StagingError::Input("a series is empty"));
    }
    if i.acm_values.len() != i.acm_timestamps.len()
        || i.ibi_values.len() != i.ibi_timestamps.len()
        || i.temp_values.len() != i.temp_timestamps.len()
    {
        return Err(StagingError::Input(
            "values and timestamps differ in length",
        ));
    }
    let ok = |ts: &[i64]| ts.iter().all(|&t| (MIN_TS_MS..=MAX_TS_MS).contains(&t));
    if !ok(i.acm_timestamps) || !ok(i.ibi_timestamps) || !ok(i.temp_timestamps) || !ok(&i.bedtime) {
        return Err(StagingError::Input(
            "timestamps should be in milliseconds from epoch",
        ));
    }
    Ok(())
}

/// The reference timestamp: the bedtime midpoint floored to the minute.
pub fn reference_timestamp_ms(bedtime: [i64; 2]) -> i64 {
    let mid = (bedtime[0].min(bedtime[1]) + bedtime[0].max(bedtime[1])).div_euclid(2);
    mid.div_euclid(60_000) * 60_000
}

/// Absolute Unix ms to float32 seconds from `reference`.
fn rel_s(ts: i64, reference: i64) -> f32 {
    (ts - reference) as f32 / 1000.0
}

/// Rows `[time, v...]` sorted by time.
fn sorted_table(
    ts: &[i64],
    reference: i64,
    values: impl Fn(usize) -> Vec<f32>,
    cols: usize,
) -> Mat {
    let mut rows: Vec<(f32, usize)> = ts
        .iter()
        .enumerate()
        .map(|(i, &t)| (rel_s(t, reference), i))
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut data = Vec::with_capacity(rows.len() * (cols + 1));
    for (t, i) in rows {
        data.push(t);
        data.extend(values(i));
    }
    Mat::from_rows(cols + 1, data)
}

fn crop_rows(m: &Mat, crop: [f32; 2]) -> Mat {
    m.filter_rows(|r| r[0] >= crop[0] && r[0] <= crop[1])
}

/// The feature matrix `[timestamp, features...]` for the (possibly cropped) night. The timestamps
/// of `acm` and `temp` are rounded in place when `crop` is `None`, which the rerun then sees.
fn run_high_level(
    acm: &mut Mat,
    low: &Mat,
    temp: &mut Mat,
    scalars: [f32; 3],
    crop: Option<[f32; 2]>,
) -> Result<Mat, StagingError> {
    let (acm_f, hrv_f, temp_f);
    match crop {
        Some(c) => {
            let (mut a, l, mut t) = (crop_rows(acm, c), crop_rows(low, c), crop_rows(temp, c));
            acm_f = extract_motion_features(&mut a)?;
            hrv_f = hrv_low_to_high_level(&l);
            temp_f = extract_temp_features(&mut t)?;
        }
        None => {
            acm_f = extract_motion_features(acm)?;
            hrv_f = hrv_low_to_high_level(low);
            temp_f = extract_temp_features(temp)?;
        }
    }
    let bedtime = crop.unwrap_or_else(|| [temp_f.at(0, 0), temp_f.at(temp_f.rows - 1, 0)]);
    let time_f = extract_time_features(bedtime);
    let demo_f = extract_demographic_features(&time_f.col(0), scalars[0], scalars[1], scalars[2]);
    Ok(combine_features(&acm_f, &hrv_f, &temp_f, &demo_f, &time_f, bedtime)?)
}

/// Features and classifier output for the (possibly cropped) night.
fn run_high_level_and_classify(
    clf: &dyn StageClassifier,
    acm: &mut Mat,
    low: &Mat,
    temp: &mut Mat,
    scalars: [f32; 3],
    crop: Option<[f32; 2]>,
) -> Result<(Mat, Mat), StagingError> {
    let combined = run_high_level(acm, low, temp, scalars, crop)?;
    let stages = classify(clf, &combined);
    Ok((stages, combined))
}

fn check_shape(clf: &dyn StageClassifier) -> Result<(), StagingError> {
    let (features, classes) = (clf.n_features(), clf.n_classes());
    if features != STAGER_FEATURES || classes != STAGER_CLASSES {
        return Err(StagingError::ClassifierShape { features, classes });
    }
    Ok(())
}

/// Inputs converted to relative-time tables.
struct Prepared {
    reference: i64,
    bedtime_rel: [f32; 2],
    acm: Mat,
    temp: Mat,
    low: Mat,
}

fn prepare(inp: &Inputs) -> Result<Prepared, StagingError> {
    validate(inp)?;
    let reference = reference_timestamp_ms(inp.bedtime);
    let bedtime_rel = [rel_s(inp.bedtime[0], reference), rel_s(inp.bedtime[1], reference)];
    let acm = sorted_table(inp.acm_timestamps, reference, |i| inp.acm_values[i].to_vec(), 6);
    let temp = sorted_table(inp.temp_timestamps, reference, |i| vec![inp.temp_values[i]], 1);
    let ibi_rel: Vec<i64> = inp.ibi_timestamps.iter().map(|&t| t - reference).collect();
    let low_rows: Vec<[f32; N_LOW]> = hrv_window_features(&ibi_rel, inp.ibi_values)?;
    let low = Mat::from_rows(N_LOW, low_rows.into_iter().flatten().collect());
    Ok(Prepared { reference, bedtime_rel, acm, temp, low })
}

/// When the first or last 30 minutes are all wake, the bedtime to rerun on,
/// keeping ten minutes of the wake before the first and after the last sleep epoch.
pub fn check_begin_end_wake(stages: &Mat) -> Option<[f32; 2]> {
    let asleep: Vec<bool> = (0..stages.rows).map(|i| stages.at(i, 1) < 4.0).collect();
    if !asleep.iter().any(|&a| a) {
        return None;
    }
    let n_start = asleep.iter().position(|&a| a).unwrap();
    let n_end = asleep.iter().rev().position(|&a| a).unwrap();
    if n_start < RERUN_WAKE_EPOCHS && n_end < RERUN_WAKE_EPOCHS {
        return None;
    }
    let first_row = n_start.saturating_sub(PAD_WAKE_EPOCHS);
    let e0 = n_end.saturating_sub(PAD_WAKE_EPOCHS);
    Some([stages.at(first_row, 0), stages.at(stages.rows - 1 - e0, 0)])
}

/// Stages a night with a [`SleepClassifier`]. Equivalent to [`stage_night_with`]; kept so
/// that callers holding a `&SleepClassifier` (or an `Arc` of one) compile unchanged.
pub fn stage_night(
    clf: &SleepClassifier,
    inp: &Inputs,
    rerun_after_long_wake: bool,
    false_nap_check: bool,
) -> Result<Output, StagingError> {
    stage_night_with(clf, inp, rerun_after_long_wake, false_nap_check)
}

/// Stages a night with the classifier bundled with the crate (`bundled::bundled_classifier`).
/// Flags as in [`stage_night_with`].
pub fn stage_night_default(
    inp: &Inputs,
    rerun_after_long_wake: bool,
    false_nap_check: bool,
) -> Result<Output, StagingError> {
    stage_night_with(super::bundled::bundled_classifier(), inp, rerun_after_long_wake, false_nap_check)
}

/// Stages a night with any [`StageClassifier`]. The first flag reruns the night trimmed to
/// the sleep period when the first or last 30 minutes are wake; the second clears
/// stages when the result looks like a false nap. Callers normally pass true for both.
pub fn stage_night_with(
    clf: &dyn StageClassifier,
    inp: &Inputs,
    rerun_after_long_wake: bool,
    false_nap_check: bool,
) -> Result<Output, StagingError> {
    check_shape(clf)?;
    let Prepared { reference, bedtime_rel, mut acm, mut temp, low } = prepare(inp)?;
    let (mut stages, mut combined) =
        run_high_level_and_classify(clf, &mut acm, &low, &mut temp, inp.scalars, None)?;
    let mut flag_rerun = 0.0;
    if rerun_after_long_wake {
        if let Some(crop) = check_begin_end_wake(&stages) {
            let (s, c) =
                run_high_level_and_classify(clf, &mut acm, &low, &mut temp, inp.scalars, Some(crop))?;
            stages = s;
            combined = c;
            flag_rerun = 1.0;
        }
    }
    let (mut stages, combined) = postprocess(&stages, &combined, &low, bedtime_rel)?;
    let mut debug = summary_metrics(&stages, &combined, flag_rerun);
    if false_nap_check {
        clear_false_naps(&mut stages, &mut debug);
    }
    let timestamps_ms = (0..stages.rows)
        .map(|i| (stages.at(i, 0) * 1000.0).round_ties_even() as i64 + reference)
        .collect();
    let stage_cols = Mat::from_cols(&(1..6).map(|j| stages.col(j)).collect::<Vec<_>>());
    let features = Mat::from_cols(
        &(1..combined.cols)
            .map(|j| combined.col(j))
            .collect::<Vec<_>>(),
    );
    Ok(Output {
        timestamps_ms,
        stages: stage_cols,
        features,
        debug,
    })
}

/// The feature rows of a night, before any classifier ran.
#[derive(Debug, Clone)]
pub struct NightFeatures {
    /// Epoch start times in Unix ms.
    pub timestamps_ms: Vec<i64>,
    /// The classifier's input: one row per epoch, [`STAGER_FEATURES`] columns in the order of
    /// `feature_columns::KEEP_COLUMNS` without the timestamp.
    pub classifier_input: Mat,
    /// All feature columns of the combined matrix (`feature_columns::COMBINED_COLUMNS` without the
    /// timestamp), a superset of `classifier_input`.
    pub features: Mat,
}

/// What [`stage_night_or_features`] returns.
#[derive(Debug, Clone)]
pub enum Staged {
    /// A classifier was supplied and the full pipeline ran.
    Stages(Output),
    /// No classifier was supplied; the features of the unrerun night are returned and the caller
    /// chooses how to stage them (for example with its own [`StageClassifier`], then
    /// [`stage_night_with`]).
    FeaturesOnly(NightFeatures),
}

/// Computes the features of the whole bedtime window without a classifier. Neither the
/// wake-trimming rerun nor the post-processing runs, because both depend on stage labels.
pub fn stage_features(inp: &Inputs) -> Result<NightFeatures, StagingError> {
    let Prepared { reference, mut acm, mut temp, low, .. } = prepare(inp)?;
    let combined = run_high_level(&mut acm, &low, &mut temp, inp.scalars, None)?;
    let idx = super::postprocess::keep_indices();
    let timestamps_ms = (0..combined.rows)
        .map(|i| (combined.at(i, 0) * 1000.0).round_ties_even() as i64 + reference)
        .collect();
    let classifier_input = Mat::from_cols(&idx[1..].iter().map(|&j| combined.col(j)).collect::<Vec<_>>());
    let features = Mat::from_cols(&(1..combined.cols).map(|j| combined.col(j)).collect::<Vec<_>>());
    Ok(NightFeatures { timestamps_ms, classifier_input, features })
}

/// Stages the night when a classifier is available and returns the features otherwise, so that
/// a caller without a classifier degrades instead of failing. Use
/// [`stage_night_required`] to get [`StagingError::NoClassifier`] instead.
pub fn stage_night_or_features(
    clf: Option<&dyn StageClassifier>,
    inp: &Inputs,
    rerun_after_long_wake: bool,
    false_nap_check: bool,
) -> Result<Staged, StagingError> {
    match clf {
        Some(c) => stage_night_with(c, inp, rerun_after_long_wake, false_nap_check).map(Staged::Stages),
        None => stage_features(inp).map(Staged::FeaturesOnly),
    }
}

/// Like [`stage_night_with`] for an optional classifier; `None` returns
/// [`StagingError::NoClassifier`].
pub fn stage_night_required(
    clf: Option<&dyn StageClassifier>,
    inp: &Inputs,
    rerun_after_long_wake: bool,
    false_nap_check: bool,
) -> Result<Output, StagingError> {
    stage_night_with(clf.ok_or(StagingError::NoClassifier)?, inp, rerun_after_long_wake, false_nap_check)
}

/// Runs the inter-beat-interval corrector over the beats of the bedtime window before staging. A beat is `(timestamp ms, ibi ms, amplitude)`, stamped at the start of its
/// interval and kept in stream order. Returns the corrected timestamps and `[ibi, amplitude,
/// validity]` rows for [`Inputs`]; validity is 1 for a valid beat and 0 otherwise.
pub fn correct_beats(beats: &[(u64, u16, u16)]) -> (Vec<i64>, Vec<[f32; 3]>) {
    let events = crate::ported::ibi_correction::correct_ibis(beats);
    let ts = events.iter().map(|e| e.ts as i64).collect();
    let values = events
        .iter()
        .map(|e| {
            [
                e.ibi as f32,
                e.amp as f32,
                if e.validity == 1 { 1.0 } else { 0.0 },
            ]
        })
        .collect();
    (ts, values)
}

/// The labels of an [`Output`] aggregated from 30 s to 5 min.
pub fn five_minute_stages(out: &Output) -> Vec<f32> {
    aggregate_30sec_to_5min(&out.stages.col(0))
}

#[cfg(test)]
mod tests {
    use super::super::bundled;
    use super::*;

    // ---- no model files: a synthetic night and a hand-built ensemble ---------------------

    /// Sum of stumps per class, NaN to the `le` side; implements the trait without a blob.
    struct Stumps {
        /// `(class, feature, threshold, value if x <= t, value if x > t)`
        stumps: Vec<(usize, usize, f32, f64, f64)>,
    }

    impl StageClassifier for Stumps {
        fn predict_proba(&self, x: &[f32; STAGER_FEATURES]) -> [f32; STAGER_CLASSES] {
            let mut m = [0f64; STAGER_CLASSES];
            for &(c, f, t, lo, hi) in &self.stumps {
                m[c] += if x[f].is_nan() || x[f] <= t { lo } else { hi };
            }
            crate::ported::sleep_classifier::softmax(&m)
        }
    }

    fn feature_index(name: &str) -> usize {
        super::super::feature_columns::KEEP_COLUMNS[1..].iter().position(|c| *c == name).expect(name)
    }

    /// Deep and light by the first half of the night, REM and wake by motion, all driven by two
    /// features.
    fn tiny_ensemble() -> Stumps {
        let (a, b) = (feature_index(super::super::feature_columns::KEEP_COLUMNS[1]), feature_index(super::super::feature_columns::KEEP_COLUMNS[2]));
        Stumps {
            stumps: vec![
                (0, a, 0.0, 1.0, -1.0),
                (1, a, 0.0, -0.5, 0.5),
                (2, b, 0.0, 0.3, -0.3),
                (3, b, 0.0, -1.0, 1.0),
            ],
        }
    }

    struct Night {
        acm: Vec<[f32; 6]>,
        acm_ts: Vec<i64>,
        ibi: Vec<[f32; 3]>,
        ibi_ts: Vec<i64>,
        temp: Vec<f32>,
        temp_ts: Vec<i64>,
        bedtime: [i64; 2],
    }

    impl Night {
        /// Eight hours from 2024-03-01 22:00 UTC with deterministic pseudo-random values.
        fn synthetic(hours: i64) -> Night {
            let start = 1_709_330_400_000i64;
            let end = start + hours * 3_600_000;
            let mut seed = 12345u64;
            let mut rnd = move || {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((seed >> 33) as f32) / (1u64 << 31) as f32
            };
            let (mut acm, mut acm_ts, mut temp, mut temp_ts) = (vec![], vec![], vec![], vec![]);
            let mut t = start;
            while t < end {
                let m = rnd();
                acm.push([m * 0.3, m, m * 0.5, rnd() * 0.2, rnd() * 0.5, rnd() * 0.1]);
                acm_ts.push(t);
                temp.push(34.0 + rnd() * 0.5);
                temp_ts.push(t);
                t += 30_000;
            }
            let (mut ibi, mut ibi_ts) = (vec![], vec![]);
            let mut t = start;
            while t < end {
                let d = 950.0 + 100.0 * rnd();
                ibi.push([d, 100.0 + 50.0 * rnd(), 1.0]);
                ibi_ts.push(t);
                t += d as i64;
            }
            Night { acm, acm_ts, ibi, ibi_ts, temp, temp_ts, bedtime: [start, end] }
        }

        fn inputs(&self) -> Inputs<'_> {
            Inputs {
                acm_values: &self.acm,
                acm_timestamps: &self.acm_ts,
                ibi_values: &self.ibi,
                ibi_timestamps: &self.ibi_ts,
                temp_values: &self.temp,
                temp_timestamps: &self.temp_ts,
                scalars: [30.0, 24.0, 1.0],
                bedtime: self.bedtime,
            }
        }
    }

    #[test]
    fn synthetic_night_runs_end_to_end_with_a_trait_object() {
        let night = Night::synthetic(8);
        let clf = tiny_ensemble();
        let out = stage_night_with(&clf, &night.inputs(), true, true).expect("staged");
        let n = out.timestamps_ms.len();
        assert!(n > 800, "about one epoch per 30 s, got {n}");
        assert_eq!((out.stages.rows, out.stages.cols), (n, 5));
        assert_eq!(out.features.rows, n);
        assert!(out.timestamps_ms.windows(2).all(|w| w[1] - w[0] == 30_000));
        for i in 0..n {
            let label = out.stages.at(i, 0);
            assert!((1.0..=4.0).contains(&label) || label == 0.0 || label.is_nan(), "label {label}");
            let p: f32 = (1..5).map(|c| out.stages.at(i, c)).sum();
            assert!(out.stages.at(i, 1).is_nan() || (p - 1.0).abs() < 1e-4, "row {i} sums to {p}");
        }
        // dyn dispatch and the generic reference impl agree
        let dynamic: &dyn StageClassifier = &clf;
        let again = stage_night_with(dynamic, &night.inputs(), true, true).unwrap();
        assert_eq!(again.stages, out.stages);
    }

    #[test]
    fn default_path_stages_a_synthetic_night_without_any_file() {
        let night = Night::synthetic(8);
        let out = stage_night_default(&night.inputs(), true, true).expect("staged");
        let n = out.timestamps_ms.len();
        assert!(n > 800, "about one epoch per 30 s, got {n}");
        assert_eq!((out.stages.rows, out.stages.cols), (n, 5));
        assert_eq!(out.features.rows, n);
        assert!(out.timestamps_ms.windows(2).all(|w| w[1] - w[0] == 30_000));
        for i in 0..n {
            let label = out.stages.at(i, 0);
            assert!([0.0, 1.0, 2.0, 3.0, 4.0].contains(&label), "label {label}");
            if label != 0.0 {
                let p: f32 = (1..5).map(|c| out.stages.at(i, c)).sum();
                assert!((p - 1.0).abs() < 1e-4, "row {i} sums to {p}");
            }
        }
        let again = stage_night_default(&night.inputs(), true, true).unwrap();
        assert_eq!(again.stages, out.stages);
        let via_with = stage_night_with(bundled::bundled_classifier(), &night.inputs(), true, true).unwrap();
        assert_eq!(via_with.stages, out.stages);
    }

    #[test]
    fn missing_classifier_degrades_to_features() {
        let night = Night::synthetic(8);
        let inp = night.inputs();
        assert_eq!(stage_night_required(None, &inp, true, true).unwrap_err(), StagingError::NoClassifier);
        let Staged::FeaturesOnly(f) = stage_night_or_features(None, &inp, true, true).unwrap() else {
            panic!("expected features");
        };
        assert_eq!(f.classifier_input.cols, STAGER_FEATURES);
        assert_eq!(f.classifier_input.rows, f.timestamps_ms.len());
        assert_eq!(f.features.rows, f.timestamps_ms.len());
        assert!(f.features.cols > STAGER_FEATURES);
        // the features are what the full pipeline classifies on its first pass
        let clf = tiny_ensemble();
        let Staged::Stages(out) = stage_night_or_features(Some(&clf), &inp, false, false).unwrap() else {
            panic!("expected stages");
        };
        assert!(out.timestamps_ms.len() <= f.timestamps_ms.len());
    }

    #[test]
    fn classifier_of_the_wrong_shape_is_rejected() {
        struct Wide;
        impl StageClassifier for Wide {
            fn n_features(&self) -> usize {
                10
            }
            fn predict_proba(&self, _: &[f32; STAGER_FEATURES]) -> [f32; STAGER_CLASSES] {
                [0.25; 4]
            }
        }
        let night = Night::synthetic(2);
        assert_eq!(
            stage_night_with(&Wide, &night.inputs(), true, true).unwrap_err(),
            StagingError::ClassifierShape { features: 10, classes: 4 }
        );
    }
}

