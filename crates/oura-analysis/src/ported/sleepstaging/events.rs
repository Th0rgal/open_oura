//! Inputs of the sleep stager for one bedtime window, assembled from decoded ring events: the
//! beat, motion and temperature streams placed on the Unix time axis, and the profile scalars.
//!
//! All timestamps are Unix milliseconds. Beats go through the inter-beat-interval correction
//! ([`crate::ported::ibi_correction`]); the stager keeps those it marks valid. The result maps
//! onto [`super::pipeline::Inputs`].
//!
//! Which ring clock places an event is the caller's concern: [`collect`] takes a fallback for
//! events captured before the first ring `time_sync` and does not decide whether a window can
//! be dated.

use crate::ported::ibi_correction::correct_ibis;
use serde_json::Value;

const EPOCH_MS: i64 = 30_000;
/// Events whose clock disagrees with the bedtime by more than this belong to another boot.
const MAX_SKEW_MS: i64 = 600_000;

pub struct StagerInputs {
    pub start_ms: i64,
    pub end_ms: i64,
    /// Corrected beats: (timestamp, ibi, amplitude, 1.0 when valid else 0.0).
    pub beats: Vec<(i64, f32, f32, f32)>,
    /// One row per 30 s epoch: [MAD iqr, max, mean, dANGLEZ iqr, max, mean].
    pub acm: Vec<(i64, [f32; 6])>,
    pub temp: Vec<(i64, f32)>,
    /// [age, BMI, sex]; sex 2 male, 1 female, -1 unknown.
    pub scalars: [f32; 3],
    /// Beats in the window before correction.
    pub raw_beats: usize,
}

/// Ring time to Unix time through `time_sync` events. An event is placed with the latest
/// ring-written `time_sync` captured at or before the event's own capture,
/// `unix = sync.unix + (ds - sync.ds) / 10`, where `ds` is ring time in tenths of a second.
pub struct AppClock {
    /// (captured_unix, ring ds, unix) in capture order.
    syncs: Vec<(i64, i64, i64)>,
    /// Every `time_sync`, ring-written or phone: (ring ds, unix, ring-written), by ring time.
    all: Vec<(i64, i64, bool)>,
}

/// Converts ring time inside one batch of events, see [`AppClock::window`].
#[derive(Debug, Clone)]
pub struct BatchClock {
    usable: Vec<(i64, i64)>,
}

impl BatchClock {
    /// Unix seconds of ring time `ds`: the latest usable sync at or before it, or the first one.
    pub fn unix_s(&self, ds: i64) -> f64 {
        let k = self.usable.partition_point(|s| s.0 <= ds).saturating_sub(1);
        let (d, u) = self.usable[k];
        u as f64 + (ds - d) as f64 / 10.0
    }
}

impl AppClock {
    pub fn from_events(events: &[(i64, u8, String, i64)]) -> Self {
        let mut syncs = Vec::new();
        let mut all = Vec::new();
        for (ds, tag, json, cu) in events {
            if *tag != 0x42 {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(json) {
                if let Some(unix) = v["unix_time"].as_i64() {
                    let ring = v["source"].as_str() != Some("phone");
                    all.push((*ds, unix, ring));
                    if ring {
                        syncs.push((*cu, *ds, unix));
                    }
                }
            }
        }
        // the input is already in (captured_unix, id) order; keep it stable
        syncs.sort_by_key(|s| s.0);
        all.sort_by_key(|s| s.0);
        AppClock { syncs, all }
    }

    /// Unix seconds of ring time `ds` through the latest ring-written sync whose own ring time is
    /// at or before `anchor_ds`, the ring time of the event that carries `ds` (a
    /// `bedtime_period` event for its start and end). This orders syncs by their position in the
    /// ring's event stream, not by capture time.
    pub fn ring_order(&self, ds: i64, anchor_ds: i64) -> Option<f64> {
        let (sd, su) = self.ring_sync_at(anchor_ds)?;
        Some(su as f64 + (ds - sd) as f64 / 10.0)
    }

    /// Inverse of [`AppClock::ring_order`] for a Unix time: the ring time through the latest
    /// ring-written sync that happened at or before it.
    pub fn ring_order_ds(&self, unix_s: f64) -> Option<i64> {
        let ring: Vec<&(i64, i64, bool)> = self.all.iter().filter(|s| s.2 && s.1 as f64 <= unix_s).collect();
        let (sd, su, _) = **ring.iter().max_by_key(|s| s.0)?;
        Some(sd + ((unix_s - su as f64) * 10.0).round() as i64)
    }

    fn ring_sync_at(&self, anchor_ds: i64) -> Option<(i64, i64)> {
        self.all.iter().filter(|s| s.2 && s.0 <= anchor_ds).max_by_key(|s| s.0).map(|s| (s.0, s.1))
    }

    /// The converter of a batch of events from `first_ds` to `last_ds`: the ring-written syncs
    /// from the latest ring-written sync at or before the first event to the earliest sync of
    /// either source at or after the last event. Phone-written syncs close the bracket but
    /// never open it and are never used for conversion: a phone anchor between a ring sync and
    /// the batch must not displace that ring sync. `None` when no ring-written sync lies in
    /// the bracket.
    pub fn window(&self, first_ds: i64, last_ds: i64) -> Option<BatchClock> {
        let i = self.all[..self.all.partition_point(|s| s.0 <= first_ds)]
            .iter()
            .rposition(|s| s.2)
            .unwrap_or(0);
        let j = self.all.partition_point(|s| s.0 < last_ds);
        let hi = j.min(self.all.len().checked_sub(1)?);
        let mut usable: Vec<(i64, i64)> = self.all[i..=hi.max(i)].iter().filter(|s| s.2).map(|s| (s.0, s.1)).collect();
        usable.sort();
        usable.dedup();
        if usable.is_empty() {
            None
        } else {
            Some(BatchClock { usable })
        }
    }

    pub fn unix_s(&self, ds: i64, captured_unix: i64) -> Option<f64> {
        let i = self.syncs.partition_point(|s| s.0 <= captured_unix);
        let (_, sync_ds, unix) = *self.syncs.get(i.checked_sub(1)?)?;
        Some(unix as f64 + (ds - sync_ds) as f64 / 10.0)
    }

    /// Inverse of [`AppClock::unix_s`]: the ring time of a Unix time, through the same sync.
    pub fn ds_at(&self, unix_s: f64, captured_unix: i64) -> Option<i64> {
        let i = self.syncs.partition_point(|s| s.0 <= captured_unix);
        let (_, sync_ds, unix) = *self.syncs.get(i.checked_sub(1)?)?;
        Some(sync_ds + ((unix_s - unix as f64) * 10.0).round() as i64)
    }
}

/// `[age, BMI, sex]`: BMI is -1 when height or weight is missing; sex is 2 for male, 1 for
/// female and -1 otherwise.
pub fn profile_scalars(age: f64, height_m: f64, weight_kg: f64, sex: char) -> [f32; 3] {
    let bmi = if height_m > 0.0 && weight_kg > 0.0 { weight_kg / (height_m * height_m) } else { -1.0 };
    let sex = match sex.to_ascii_uppercase() {
        'M' => 2.0,
        'F' => 1.0,
        _ => -1.0,
    };
    [age as f32, bmi as f32, sex]
}

/// Round to the nearest integer, ties to even.
fn round_even(x: f64) -> i64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 && (r as i64) % 2 != 0 {
        (r - x.signum()) as i64
    } else {
        r as i64
    }
}

/// Assembles one window. `events` holds `(ring ds, tag, decoded json, captured_unix)` for every
/// decoded event of the store, in capture order; `captured_unix` is the capture time of the window's `bedtime_period`
/// event. `fallback_unix_s` converts ring time to Unix seconds for events captured before any
/// ring-sourced `time_sync`; the caller also decides whether the window can be dated at all.
pub fn collect(
    events: &[(i64, u8, String, i64)],
    fallback_unix_s: &dyn Fn(i64, i64) -> f64,
    start_ds: i64,
    end_ds: i64,
    captured_unix: i64,
    scalars: [f32; 3],
) -> Result<StagerInputs, String> {
    let clock = AppClock::from_events(events);
    // capture order: the latest ring-written sync captured at or before the event's capture
    let capture_ms = |ds: i64, cu: i64| -> i64 {
        let s = clock.unix_s(ds, cu).unwrap_or_else(|| fallback_unix_s(ds, cu));
        round_even(s * 1000.0)
    };
    // The window's start and end come from a `bedtime_period` event and are dated with the sync
    // that precedes that event in ring time; the night's events are converted as one batch with
    // the syncs that bracket it (see `AppClock::ring_order` and `AppClock::window`).
    let bed_event_ds = events.iter().find_map(|(ds, tag, json, _)| {
        if *tag != 0x76 {
            return None;
        }
        let v: Value = serde_json::from_str(json).ok()?;
        (v["bedtime_start_ds"].as_i64()? == start_ds && v["bedtime_end_ds"].as_i64()? == end_ds).then_some(*ds)
    });
    let bed_ms = |ds: i64| -> i64 {
        match bed_event_ds.and_then(|b| clock.ring_order(ds, b)) {
            Some(s) => round_even(s * 1000.0),
            None => capture_ms(ds, captured_unix),
        }
    };
    let in_night = events
        .iter()
        .filter(|e| !matches!(e.1, 0x41 | 0x42) && start_ds <= e.0 && e.0 <= end_ds)
        .map(|e| e.0);
    let batch = in_night
        .clone()
        .min()
        .zip(in_night.max())
        .and_then(|(first, last)| clock.window(first, last));
    let unix_ms = |ds: i64, cu: i64| -> i64 {
        match &batch {
            Some(b) => round_even(b.unix_s(ds) * 1000.0),
            None => capture_ms(ds, cu),
        }
    };
    let start_ms = bed_ms(start_ds);
    let end_ms = bed_ms(end_ds);
    let mut beats: Vec<(u64, u16, u16)> = Vec::new();
    let mut acm: Vec<(i64, [f32; 6])> = Vec::new();
    let mut temp: Vec<(i64, f32)> = Vec::new();
    for (ds, tag, json, cu) in events {
        if !matches!(*tag, 0x60 | 0x72 | 0x75) || *ds < start_ds - 6000 || *ds > end_ds + 6000 {
            continue;
        }
        let t = unix_ms(*ds, *cu);
        if (t - (start_ms + (*ds - start_ds) * 100)).abs() > MAX_SKEW_MS {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(json) else { continue };
        match *tag {
            0x60 => {
                let ibis: Vec<i64> = v["ibi_ms"].as_array().map_or(vec![], |a| a.iter().map(|x| x.as_i64().unwrap_or(0)).collect());
                let amps: Vec<i64> = v["amplitude"].as_array().map_or(vec![], |a| a.iter().map(|x| x.as_i64().unwrap_or(0)).collect());
                for i in 0..ibis.len() {
                    let ts = t - ibis[i..].iter().sum::<i64>();
                    if start_ms <= ts && ts <= end_ms {
                        beats.push((ts as u64, ibis[i].clamp(0, 65535) as u16, amps.get(i).copied().unwrap_or(0).clamp(0, 65535) as u16));
                    }
                }
            }
            0x72 => {
                if let Some(m) = v["acm_mad"].as_array() {
                    if m.len() >= 6 && start_ms <= t && t <= end_ms {
                        let g = |i: usize| m[i].as_f64().unwrap_or(0.0) as f32;
                        acm.push((t, [g(5), g(4), g(3), g(2), g(1), g(0)]));
                    }
                }
            }
            _ => {
                if let Some(temps) = v["temps_c"].as_array().filter(|a| !a.is_empty()) {
                    let n = temps.len() as i64;
                    for (i, x) in temps.iter().enumerate() {
                        let ts = t - EPOCH_MS * (n - 1 - i as i64);
                        if start_ms <= ts && ts <= end_ms {
                            temp.push((ts, x.as_f64().unwrap_or(0.0) as f32));
                        }
                    }
                }
            }
        }
    }
    if beats.is_empty() || acm.is_empty() || temp.is_empty() {
        return Err("not enough IBI, motion or temperature events in this window".into());
    }
    let raw_beats = beats.len();
    acm.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)));
    temp.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)));
    // the corrector is stateful and drops beats that go back in time: keep stream order
    let corrected: Vec<(i64, f32, f32, f32)> = correct_ibis(&beats)
        .into_iter()
        .map(|e| (e.ts as i64, e.ibi as f32, e.amp as f32, if e.validity == 1 { 1.0 } else { 0.0 }))
        .collect();
    if !corrected.iter().any(|b| b.3 == 1.0) {
        return Err("no valid IBI in this window".into());
    }
    Ok(StagerInputs { start_ms, end_ms, beats: corrected, acm, temp, scalars, raw_beats })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_uses_the_latest_ring_sync_captured_before_the_event() {
        let ev = |ds, tag, json: &str, cu| (ds, tag, json.to_string(), cu);
        let events = vec![
            ev(100, 0x42, r#"{"unix_time":1000}"#, 10),
            ev(150, 0x42, r#"{"unix_time":9999,"source":"phone"}"#, 11),
            ev(500, 0x42, r#"{"unix_time":2000}"#, 20),
        ];
        let clock = AppClock::from_events(&events);
        assert_eq!(clock.unix_s(130, 15), Some(1003.0));
        assert_eq!(clock.unix_s(520, 25), Some(2002.0));
        assert_eq!(clock.unix_s(130, 5), None);
        assert_eq!(clock.ds_at(1003.0, 15), Some(130));
        assert_eq!(clock.ds_at(2002.0, 25), Some(520));
    }

    #[test]
    fn scalars_use_sentinels() {
        assert_eq!(profile_scalars(28.0, 1.86, 85.0, 'M')[2], 2.0);
        assert_eq!(profile_scalars(28.0, 0.0, 85.0, 'F'), [28.0, -1.0, 1.0]);
        assert_eq!(round_even(2.5), 2);
        assert_eq!(round_even(3.5), 4);
    }

    #[test]
    fn collects_a_window_from_events() {
        let ev = |ds, tag, json: String, cu| (ds, tag, json, cu);
        let ibis = vec![900; 80];
        let events = vec![
            ev(0, 0x42, r#"{"unix_time":1760000000}"#.into(), 10),
            ev(3000, 0x60, format!(r#"{{"ibi_ms":{ibis:?},"amplitude":{:?}}}"#, vec![100; 80]), 20),
            ev(3000, 0x72, r#"{"acm_mad":[1,2,3,4,5,6]}"#.into(), 20),
            ev(3000, 0x75, r#"{"temps_c":[33.0,33.1]}"#.into(), 20),
        ];
        let never = |_: i64, _: i64| -> f64 { panic!("fallback used") };
        let w = collect(&events, &never, 0, 6000, 30, [28.0, 24.0, 2.0]).unwrap();
        assert_eq!((w.start_ms, w.end_ms), (1_760_000_000_000, 1_760_000_600_000));
        assert_eq!(w.raw_beats, 80);
        assert_eq!(w.acm, vec![(1_760_000_300_000, [6.0, 5.0, 4.0, 3.0, 2.0, 1.0])]);
        assert_eq!(w.temp.len(), 2);
        assert_eq!(w.temp[1].0 - w.temp[0].0, 30_000);
        let empty = collect(&events[..1], &never, 0, 6000, 30, [28.0, 24.0, 2.0]);
        assert!(empty.is_err());
    }

    #[test]
    fn ring_order_and_batch_windows() {
        let ev = |ds, json: &str, cu| (ds, 0x42u8, json.to_string(), cu);
        let events = vec![
            ev(100, r#"{"unix_time":1000}"#, 10),
            ev(200, r#"{"unix_time":1500,"source":"phone"}"#, 11),
            ev(400, r#"{"unix_time":2000}"#, 12),
            ev(900, r#"{"unix_time":3000}"#, 13),
        ];
        let clock = AppClock::from_events(&events);
        // the sync at ds 400 is the latest ring-written one at or before anchor 450
        assert_eq!(clock.ring_order(430, 450), Some(2003.0));
        assert_eq!(clock.ring_order(50, 90), None);
        assert_eq!(clock.ring_order_ds(2003.0), Some(430));
        // a batch from 250 to 500: bracket is the ring sync at 100 .. the sync at 900; the phone
        // anchor at 200 does not displace the ring sync at 100 for ds 250
        let w = clock.window(250, 500).unwrap();
        assert_eq!(w.unix_s(250), 1015.0);
        assert_eq!(w.unix_s(450), 2005.0);
        assert!(clock.window(10, 20).is_some());
    }

    #[test]
    fn phone_anchors_do_not_open_the_bracket() {
        // Ring syncs at ds 1000 (unix 5000) and ds 90000 (unix 14000, 100 s off the first
        // sync's offset), a phone anchor at ds 40000 between them, and the night at ds
        // 50000..60000. The night is converted with the ring sync before the phone anchor.
        let sync = |ds, unix, src: &str| {
            let json = if src.is_empty() { format!(r#"{{"unix_time":{unix}}}"#) } else { format!(r#"{{"unix_time":{unix},"source":"{src}"}}"#) };
            (ds, 0x42u8, json, 100 + ds / 1000)
        };
        let ev = |ds, tag, json: String, cu| (ds, tag, json, cu);
        let mut events = vec![
            sync(1000, 5000, ""),
            sync(40000, 9000, "phone"),
            sync(90000, 14000, ""),
        ];
        events.push(ev(52000, 0x60, format!(r#"{{"ibi_ms":{:?},"amplitude":{:?}}}"#, vec![900; 80], vec![100; 80]), 150));
        events.push(ev(52000, 0x72, r#"{"acm_mad":[1,2,3,4,5,6]}"#.into(), 150));
        events.push(ev(52000, 0x75, r#"{"temps_c":[33.0,33.1]}"#.into(), 150));
        events.push(ev(58000, 0x72, r#"{"acm_mad":[1,2,3,4,5,6]}"#.into(), 150));
        events.push(ev(90500, 0x76, r#"{"bedtime_start_ds":50000,"bedtime_end_ds":60000}"#.into(), 190));
        let clock = AppClock::from_events(&events);
        // ring sync at ds 1000: unix = 5000 + (52000 - 1000) / 10 = 10100
        assert_eq!(clock.window(52000, 58000).unwrap().unix_s(52000), 10100.0);
        let never = |_: i64, _: i64| -> f64 { panic!("fallback used") };
        let w = collect(&events, &never, 50000, 60000, 190, [28.0, 24.0, 2.0]).unwrap();
        assert_eq!(w.acm[0].0, 10_100_000);
        assert_eq!(w.acm[1].0, 10_700_000);
    }
}
