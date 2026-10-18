//! Readiness history conversion from the Android 7.26.0 `bxl.a` host mapper.
//!
//! The inputs are complete, already selected/materialized history lists in the
//! original caller's order. They are not arbitrary ring sessions or a sparse
//! collection of observed nights. This mapper reverses both lists and produces
//! the six arrays consumed by the daily readiness calculation.
//!
//! See `docs/algorithms/readiness-history-inputs.md` for selection boundaries,
//! timestamp units, missing values and the relationship to the metric ports.

/// Source sleep fields read by the original mapper. Temperatures are Celsius;
/// duration is seconds and HRV is milliseconds. `timestamp` is the original
/// saved Long, compared directly with `ring_setup_at` in the same source unit.
#[derive(Clone, Copy, Debug)]
pub struct SleepRow {
    pub readiness_temperature_deviation: Option<f64>,
    pub highest_temperature: Option<f64>,
    pub total_sleep_duration: Option<i64>,
    pub average_hrv: Option<i64>,
    pub timestamp: i64,
}

/// Source activity fields. Duration fields are seconds; distance is meters.
#[derive(Clone, Copy, Debug)]
pub struct ActivityRow {
    pub equivalent_walking_distance: i64,
    pub sedentary_seconds: i64,
    pub low_seconds: i64,
    pub medium_seconds: i64,
    pub high_seconds: i64,
    pub resting_seconds: i64,
}

/// Original signed Java array values, newest first. Reinterpret the low bits
/// explicitly when passing an array to a native unsigned field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct History {
    pub temperature_deviations: [i32; 3],
    pub highest_temperatures: [i32; 90],
    pub total_sleep: [i32; 14],
    pub walking_distance: [i32; 14],
    pub wear_percentage: [i8; 14],
    pub rmssd: [i32; 14],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    IncompleteHistory,
    NanTemperature,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::IncompleteHistory => "Complete 90-sleep and 15-activity history required",
            Self::NanTemperature => "Cannot round NaN value.",
        })
    }
}

impl std::error::Error for Error {}

/// Kotlin Double.roundToInt: ties toward positive infinity, saturation before
/// rounding, and NaN rejection. Adding 0.5 first would round nextDown(0.5) up.
fn centidegrees(celsius: f64) -> Result<i32, Error> {
    let value = celsius * 100.0;
    if value.is_nan() {
        return Err(Error::NanTemperature);
    }
    if value > f64::from(i32::MAX) {
        return Ok(i32::MAX);
    }
    if value < f64::from(i32::MIN) {
        return Ok(i32::MIN);
    }
    let lower = value.floor();
    Ok(lower as i32 + i32::from(value - lower >= 0.5))
}

/// Preserve the source Kotlin Duration saturation before Long-to-Int narrowing.
fn minutes(seconds: i64) -> i32 {
    const MAX_FINITE_SECONDS: i64 = 4_611_686_018_427_387;
    let value = if seconds > MAX_FINITE_SECONDS {
        i64::MAX
    } else if seconds < -MAX_FINITE_SECONDS {
        i64::MIN
    } else {
        seconds / 60
    };
    value as i32
}

/// Original readiness_calculate_wear_pct, followed by JNI/Java byte narrowing.
fn wear_percentage_minutes(minutes: [i32; 5]) -> i8 {
    if minutes[4] == 1440 {
        return 0;
    }
    let sum = minutes[..4]
        .iter()
        .fold(0u32, |value, &x| value.wrapping_add(x as u32));
    let denominator = 1440u32.wrapping_sub(minutes[4] as u32);
    let raw = ((sum as f32 * 100.0) / denominator as f32 + 0.5) as i32;
    raw as i8
}

/// Convert the original complete caller-order sleep/activity lists.
///
/// Reverse the lists, consume 90 sleep rows and skip the newest activity before
/// consuming 14 activity rows. The caller must materialize missing calendar days
/// using the source query's placeholders first; this function does not fill gaps.
/// Extra older rows are not read. Short lists fail instead of being zero-padded.
///
/// The ring-setup threshold gates temperature history only. It does not discard
/// old sleep duration or HRV. Null deviations become -32768; null highest
/// temperatures, durations and HRV become zero. The inputs remain unchanged.
pub fn prepare(
    sleeps: &[SleepRow],
    activities: &[ActivityRow],
    ring_setup_at: Option<i64>,
) -> Result<History, Error> {
    if sleeps.len() < 90 || activities.len() < 15 {
        return Err(Error::IncompleteHistory);
    }
    let mut history = History {
        temperature_deviations: [0; 3],
        highest_temperatures: [0; 90],
        total_sleep: [0; 14],
        walking_distance: [0; 14],
        wear_percentage: [0; 14],
        rmssd: [0; 14],
    };
    for (i, row) in sleeps.iter().rev().take(90).enumerate() {
        let valid_temperature = ring_setup_at.is_none_or(|setup| row.timestamp >= setup);
        if i < 3 {
            history.temperature_deviations[i] = if valid_temperature {
                row.readiness_temperature_deviation
                    .map(centidegrees)
                    .transpose()?
                    .unwrap_or(-32768)
            } else {
                -32768
            };
        }
        history.highest_temperatures[i] = if valid_temperature {
            row.highest_temperature
                .map(centidegrees)
                .transpose()?
                .unwrap_or(0)
        } else {
            0
        };
        if i < 14 {
            history.total_sleep[i] = row.total_sleep_duration.unwrap_or(0) as i32;
            history.rmssd[i] = row.average_hrv.unwrap_or(0) as i32;
        }
    }
    for (i, row) in activities.iter().rev().skip(1).take(14).enumerate() {
        history.walking_distance[i] = row.equivalent_walking_distance as i32;
        history.wear_percentage[i] = wear_percentage_minutes([
            minutes(row.sedentary_seconds),
            minutes(row.low_seconds),
            minutes(row.medium_seconds),
            minutes(row.high_seconds),
            minutes(row.resting_seconds),
        ]);
    }
    Ok(history)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> (Vec<SleepRow>, Vec<ActivityRow>) {
        let sleeps = (0..90)
            .map(|i| SleepRow {
                readiness_temperature_deviation: Some(i as f64 / 100.0),
                highest_temperature: Some(36.0 + i as f64 / 100.0),
                total_sleep_duration: Some(i),
                average_hrv: Some(i + 100),
                timestamp: i,
            })
            .collect();
        let activities = (0..15)
            .map(|i| ActivityRow {
                equivalent_walking_distance: i,
                sedentary_seconds: 3600,
                low_seconds: 0,
                medium_seconds: 0,
                high_seconds: 0,
                resting_seconds: 0,
            })
            .collect();
        (sleeps, activities)
    }

    #[test]
    fn reverses_lists_and_skips_newest_activity() {
        let (sleeps, activities) = rows();
        let h = prepare(&sleeps, &activities, None).unwrap();
        assert_eq!(h.temperature_deviations, [89, 88, 87]);
        assert_eq!(h.highest_temperatures[0], 3689);
        assert_eq!(h.total_sleep[0], 89);
        assert_eq!(h.rmssd[13], 176);
        assert_eq!(h.walking_distance[0], 13);
        assert_eq!(h.walking_distance[13], 0);
        assert_eq!(h.wear_percentage, [4; 14]);
    }

    #[test]
    fn ring_setup_only_gates_temperature_and_keeps_equal_timestamp() {
        let (mut sleeps, activities) = rows();
        sleeps[89].readiness_temperature_deviation = None;
        let h = prepare(&sleeps, &activities, Some(89)).unwrap();
        assert_eq!(h.temperature_deviations, [-32768; 3]);
        assert_eq!(h.highest_temperatures[0], 3689);
        assert_eq!(h.highest_temperatures[1], 0);
        assert_eq!(h.total_sleep[..2], [89, 88]);
        assert_eq!(h.rmssd[..2], [189, 188]);
        sleeps[0].highest_temperature = Some(f64::NAN);
        assert!(prepare(&sleeps, &activities, Some(89)).is_ok());
        assert_eq!(
            prepare(&sleeps, &activities, None),
            Err(Error::NanTemperature)
        );
    }

    #[test]
    fn short_lists_fail_and_native_narrowing_is_preserved() {
        let (mut sleeps, mut activities) = rows();
        assert_eq!(
            prepare(&sleeps[..89], &activities, None),
            Err(Error::IncompleteHistory)
        );
        assert_eq!(
            prepare(&sleeps, &activities[..14], None),
            Err(Error::IncompleteHistory)
        );
        sleeps[89].total_sleep_duration = Some(0x1_0000_0001);
        sleeps[89].average_hrv = None;
        activities[13].equivalent_walking_distance = i64::MAX;
        activities[13].sedentary_seconds = 120_000;
        let h = prepare(&sleeps, &activities, None).unwrap();
        assert_eq!(h.total_sleep[0], 1);
        assert_eq!(h.rmssd[0], 0);
        assert_eq!(h.walking_distance[0], -1);
        assert_eq!(h.wear_percentage[0], -117); // 139 as a Java byte
    }

    #[test]
    fn temperature_rounding_and_duration_limits_follow_the_host() {
        assert_eq!(centidegrees(-0.005), Ok(0));
        assert_eq!(centidegrees(0.005), Ok(1));
        assert_eq!(centidegrees(f64::INFINITY), Ok(i32::MAX));
        assert_eq!(centidegrees(f64::NEG_INFINITY), Ok(i32::MIN));
        assert_eq!(centidegrees(f64::NAN), Err(Error::NanTemperature));
        assert_eq!(minutes(i64::MAX), -1);
        assert_eq!(minutes(i64::MIN), 0);
        assert_eq!(minutes(-119), -1);
        assert_eq!(wear_percentage_minutes([0, 0, 0, 0, 1440]), 0);
    }
}
