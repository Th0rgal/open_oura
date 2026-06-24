//! Activity-session detection — **open_oura ORIGINAL heuristic, NOT an Oura
//! algorithm.** Oura classifies workouts in its cloud (`/api/activity-tagging`),
//! which we cannot port; this is our own simple, transparent detector built on the
//! signals we *do* decode from the ring (MET, motion intensity, skin temperature,
//! heart rate). It will not match Oura's labels and makes no claim to.
//!
//! It validates against the real swim+sauna we captured: a swim shows a sustained
//! temperature *floor* (the ring equilibrating to pool water) under high MET, while
//! a sauna shows a sharp temperature *spike* with little motion.

/// One minute of aggregated ring signals. All fields optional except `minute`
/// (a monotonic minute index or unix-minute); missing channels are `None`.
#[derive(Clone, Copy, Debug, Default)]
pub struct MinuteSample {
    pub minute: i64,
    pub met: Option<f64>,
    pub motion: u32, // high-intensity motion units in the minute
    pub temp_c: Option<f64>,
    pub hr: Option<u32>,
}

/// What kind of session we think this was. These are *our* labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum SessionKind {
    /// Elevated activity at normal skin temperature.
    Workout,
    /// Elevated activity with a sustained low temperature floor (in-water).
    Swim,
    /// Sharp high-temperature excursion (heat exposure), typically low motion.
    Sauna,
    /// Sustained low temperature without the activity of a swim.
    ColdExposure,
}

/// Tunable thresholds. **These are open_oura's choices, not Oura's.**
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub met_active: f64,      // MET considered "active"
    pub motion_active: u32,   // high-intensity motion/min considered "active"
    pub hot_c: f64,           // temp at/above this = heat exposure
    pub water_c: f64,         // temp at/below this (while active) = in-water
    pub cold_c: f64,          // temp at/below this = cold exposure
    pub min_minutes: usize,   // shortest reported session
    pub merge_gap: usize,     // bridge gaps up to this many minutes
}

impl Default for Config {
    fn default() -> Self {
        Self {
            met_active: 3.0,
            motion_active: 15,
            hot_c: 38.5,
            water_c: 32.0,
            cold_c: 29.0,
            min_minutes: 3,
            merge_gap: 2,
        }
    }
}

/// A detected session over `[start_minute, end_minute]` (inclusive).
#[derive(Clone, Debug, serde::Serialize)]
pub struct Session {
    pub start_minute: i64,
    pub end_minute: i64,
    pub minutes: usize,
    pub kind: SessionKind,
    pub peak_met: f64,
    pub mean_hr: Option<u32>,
    pub temp_min: Option<f64>,
    pub temp_max: Option<f64>,
}

/// Classify a single minute into a session kind, or `None` if it's "quiet". The
/// order encodes priority: a heat spike (Sauna) wins over activity, so an adjacent
/// swim and sauna land in different kinds and therefore different sessions.
fn minute_kind(s: &MinuteSample, c: &Config) -> Option<SessionKind> {
    let active = s.met.is_some_and(|m| m >= c.met_active) || s.motion >= c.motion_active;
    if s.temp_c.is_some_and(|t| t >= c.hot_c) {
        Some(SessionKind::Sauna)
    } else if active && s.temp_c.is_some_and(|t| t <= c.water_c) {
        Some(SessionKind::Swim)
    } else if s.temp_c.is_some_and(|t| t <= c.cold_c) {
        Some(SessionKind::ColdExposure)
    } else if active {
        Some(SessionKind::Workout)
    } else {
        None
    }
}

fn stats(run: &[MinuteSample], kind: SessionKind) -> Session {
    let peak_met = run.iter().filter_map(|s| s.met).fold(0.0, f64::max);
    let temps: Vec<f64> = run.iter().filter_map(|s| s.temp_c).collect();
    let hrs: Vec<u32> = run.iter().filter_map(|s| s.hr).collect();
    let mean_hr = (!hrs.is_empty())
        .then(|| (hrs.iter().sum::<u32>() as f64 / hrs.len() as f64).round() as u32);
    Session {
        start_minute: run[0].minute,
        end_minute: run[run.len() - 1].minute,
        minutes: run.len(),
        kind,
        peak_met,
        mean_hr,
        temp_min: temps.iter().copied().reduce(f64::min),
        temp_max: temps.iter().copied().reduce(f64::max),
    }
}

/// Detect activity/exposure sessions from a minute-resolution signal series
/// (assumed sorted by `minute`). Each minute is classified into a kind; runs of
/// the **same** kind (bridging quiet gaps up to `merge_gap`) of at least
/// `min_minutes` become a [`Session`]. Distinct kinds never merge, so an adjacent
/// swim and sauna stay separate.
pub fn detect(samples: &[MinuteSample], c: &Config) -> Vec<Session> {
    let mut out = Vec::new();
    let mut run: Vec<MinuteSample> = Vec::new();
    let mut cur: Option<SessionKind> = None;
    let mut gap = 0usize;
    let mut flush = |run: &mut Vec<MinuteSample>, cur: &mut Option<SessionKind>| {
        if let Some(k) = *cur {
            if run.len() >= c.min_minutes {
                out.push(stats(run, k));
            }
        }
        run.clear();
        *cur = None;
    };
    for &s in samples {
        match minute_kind(&s, c) {
            Some(k) if cur == Some(k) => {
                run.push(s);
                gap = 0;
            }
            Some(k) => {
                flush(&mut run, &mut cur);
                run.push(s);
                cur = Some(k);
                gap = 0;
            }
            None if cur.is_some() => {
                gap += 1;
                if gap > c.merge_gap {
                    flush(&mut run, &mut cur);
                    gap = 0;
                } else {
                    run.push(s); // bridge a short quiet gap within the same kind
                }
            }
            None => {}
        }
    }
    flush(&mut run, &mut cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(minute: i64, met: f64, motion: u32, temp: f64) -> MinuteSample {
        MinuteSample { minute, met: Some(met), motion, temp_c: Some(temp), hr: Some(80) }
    }

    #[test]
    fn detects_swim_then_sauna() {
        let mut s = Vec::new();
        // swim: 20 min, high MET, temp floor ~30.7
        for i in 0..20 {
            s.push(m(i, 8.0, 40, 30.7));
        }
        // 3 min rest (not interesting)
        for i in 20..23 {
            s.push(m(i, 1.0, 2, 34.0));
        }
        // sauna: 6 min, low motion, temp spike to 42
        for i in 23..29 {
            s.push(m(i, 1.0, 2, 41.0));
        }
        let sessions = detect(&s, &Config::default());
        let kinds: Vec<_> = sessions.iter().map(|x| x.kind).collect();
        assert!(kinds.contains(&SessionKind::Swim), "{kinds:?}");
        assert!(kinds.contains(&SessionKind::Sauna), "{kinds:?}");
        let swim = sessions.iter().find(|x| x.kind == SessionKind::Swim).unwrap();
        assert!(swim.peak_met >= 8.0 && swim.temp_min.unwrap() <= 32.0);
    }

    #[test]
    fn ignores_quiet_periods() {
        let s: Vec<_> = (0..30).map(|i| m(i, 1.2, 3, 34.5)).collect();
        assert!(detect(&s, &Config::default()).is_empty());
    }
}
