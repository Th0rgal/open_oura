//! Saved-record selection before the Android 7.26.0 long-term baseline update.
//!
//! Port of the `x7h` host selection: latest preceding same-version baseline,
//! latest preceding day's longest `long_sleep`, then activity on the calendar
//! day before that sleep. This chooses inputs; it does not update or save them.
//!
//! Dates are caller-projected, validated calendar days counted from 1970-01-01,
//! **not UTC seconds**. Keep the saved record's day rather than deriving it from
//! bedtime. Inputs contain active records only, in their original encounter
//! order. See `docs/algorithms/baseline-inputs.md` for ordering and integration.

/// A saved baseline's day and persisted algorithm version (`nssa`, `v1`, etc.).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviousBaseline<'a> {
    pub day: i32,
    pub sleep_algorithm_version: &'a str,
}

/// Fields used by the previous-long-sleep query and temperature admission gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviousSleep<'a> {
    pub day: i32,
    /// Persisted type (`long_sleep`), not a label or a duration-based guess.
    pub sleep_type: &'a str,
    pub total_sleep_duration_seconds: Option<i64>,
    pub highest_temperature_available: bool,
}

/// Fields used by the exact-day, descending-timestamp activity query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviousActivity {
    pub day: i32,
    pub timestamp: i64,
}

/// Native host decision; indices refer to the caller's unmodified input slices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// No preceding baseline of this version: create the current day's default.
    DefaultForCurrentDay,
    NoPreviousLongSleep,
    PreviousSleepHasNoHighestTemperature,
    PreviousBaselineAlreadyUsesSleep,
    NoActivityOnDayBeforeSleep,
    Update {
        baseline_index: usize,
        sleep_index: usize,
        activity_index: usize,
    },
}

/// Choose the records used by the original baseline-update host (`x7h`).
///
/// Missing current algorithm version defaults to `nssa`. Only the baseline is
/// filtered by algorithm version; the sleep query filters on `long_sleep`.
/// Equal-day baseline ties keep the last encounter, whereas equal-duration
/// sleep ties keep the first. Every nonnull duration, including `i64::MIN`,
/// sorts before null in the original descending duration query.
///
/// Activity uses the greatest saved timestamp on the required day, keeping the
/// first encounter on ties. Gaps are allowed; this never substitutes the previous
/// *observed* activity day for the calendar day before the selected sleep.
pub fn select_update(
    current_day: i32,
    current_sleep_algorithm_version: Option<&str>,
    baselines: &[PreviousBaseline<'_>],
    sleeps: &[PreviousSleep<'_>],
    activities: &[PreviousActivity],
) -> Admission {
    let version = current_sleep_algorithm_version.unwrap_or("nssa");
    let mut previous_baseline: Option<(usize, i32)> = None;
    for (index, row) in baselines.iter().enumerate() {
        if row.sleep_algorithm_version == version
            && row.day < current_day
            && previous_baseline.is_none_or(|(_, old_day)| row.day >= old_day)
        {
            previous_baseline = Some((index, row.day));
        }
    }
    let Some((baseline_index, baseline_day)) = previous_baseline else {
        return Admission::DefaultForCurrentDay;
    };
    let mut previous_sleep: Option<(usize, i32, Option<i64>)> = None;
    for (index, row) in sleeps.iter().enumerate() {
        if row.sleep_type != "long_sleep" || row.day >= current_day {
            continue;
        }
        let duration = row.total_sleep_duration_seconds;
        if previous_sleep.is_none_or(|(_, old_day, old_duration)| {
            row.day > old_day || (row.day == old_day && duration > old_duration)
        }) {
            previous_sleep = Some((index, row.day, duration));
        }
    }
    let Some((sleep_index, sleep_day, _)) = previous_sleep else {
        return Admission::NoPreviousLongSleep;
    };
    if !sleeps[sleep_index].highest_temperature_available {
        return Admission::PreviousSleepHasNoHighestTemperature;
    }
    if baseline_day > sleep_day {
        return Admission::PreviousBaselineAlreadyUsesSleep;
    }
    // Widen first so the public i32 day domain cannot underflow.
    let previous_activity_day = i64::from(sleep_day) - 1;
    let mut previous_activity: Option<(usize, i64)> = None;
    for (index, row) in activities.iter().enumerate() {
        if i64::from(row.day) == previous_activity_day
            && previous_activity.is_none_or(|(_, old_timestamp)| row.timestamp > old_timestamp)
        {
            previous_activity = Some((index, row.timestamp));
        }
    }
    match previous_activity {
        Some((activity_index, _)) => Admission::Update {
            baseline_index,
            sleep_index,
            activity_index,
        },
        None => Admission::NoActivityOnDayBeforeSleep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline(day: i32) -> PreviousBaseline<'static> {
        PreviousBaseline {
            day,
            sleep_algorithm_version: "nssa",
        }
    }

    fn sleep(day: i32, duration: Option<i64>) -> PreviousSleep<'static> {
        PreviousSleep {
            day,
            sleep_type: "long_sleep",
            total_sleep_duration_seconds: duration,
            highest_temperature_available: true,
        }
    }

    fn activity(day: i32, timestamp: i64) -> PreviousActivity {
        PreviousActivity { day, timestamp }
    }

    #[test]
    fn uses_latest_day_and_query_specific_tie_rules() {
        let baselines = [
            baseline(11),
            PreviousBaseline {
                sleep_algorithm_version: "v1",
                ..baseline(12)
            },
            baseline(11),
            baseline(13), // current day is not a previous baseline
        ];
        let sleeps = [
            sleep(11, Some(30_000)),
            PreviousSleep {
                sleep_type: "late_nap",
                ..sleep(12, Some(40_000))
            },
            sleep(12, None),
            sleep(12, Some(i64::MIN)),
            sleep(12, Some(i64::MIN)),
        ];
        assert_eq!(
            select_update(
                13,
                None,
                &baselines,
                &sleeps,
                &[
                    activity(11, 2),
                    activity(11, 3),
                    activity(11, 3),
                    activity(10, 99)
                ],
            ),
            Admission::Update {
                baseline_index: 2,
                sleep_index: 3,
                activity_index: 1,
            }
        );
        // The longest duration on the latest eligible day wins, not the
        // longest duration across all historical days.
        let mut sleeps = sleeps;
        sleeps[4].total_sleep_duration_seconds = Some(25_000);
        assert!(matches!(
            select_update(13, None, &baselines, &sleeps, &[activity(11, 0)]),
            Admission::Update { sleep_index: 4, .. }
        ));
    }

    #[test]
    fn preserves_admission_boundaries_and_does_not_fill_history_gaps() {
        let baselines = [baseline(10)];
        let sleeps = [sleep(12, Some(25_000))];
        assert_eq!(
            select_update(13, Some("v1"), &baselines, &sleeps, &[activity(11, 0)]),
            Admission::DefaultForCurrentDay
        );
        assert_eq!(
            select_update(13, None, &baselines, &[], &[activity(11, 0)]),
            Admission::NoPreviousLongSleep
        );
        let no_temp = [PreviousSleep {
            highest_temperature_available: false,
            ..sleeps[0]
        }];
        assert_eq!(
            select_update(13, None, &baselines, &no_temp, &[activity(11, 0)]),
            Admission::PreviousSleepHasNoHighestTemperature
        );
        assert_eq!(
            select_update(
                13,
                None,
                &[baseline(12)],
                &[sleep(11, Some(25_000))],
                &[activity(10, 0)]
            ),
            Admission::PreviousBaselineAlreadyUsesSleep
        );
        assert_eq!(
            select_update(13, None, &baselines, &sleeps, &[activity(10, 0)]),
            Admission::NoActivityOnDayBeforeSleep
        );
        // Equal baseline/sleep day is admitted; strictly newer is not.
        assert!(matches!(
            select_update(13, None, &[baseline(12)], &sleeps, &[activity(11, 0)]),
            Admission::Update { .. }
        ));
    }

    #[test]
    fn extreme_day_cannot_wrap_activity_selection() {
        assert_eq!(
            select_update(
                i32::MIN + 1,
                None,
                &[baseline(i32::MIN)],
                &[sleep(i32::MIN, Some(25_000))],
                &[activity(i32::MAX, 0)],
            ),
            Admission::NoActivityOnDayBeforeSleep
        );
    }
}
