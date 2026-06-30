//! The dashboard **summary computation** — the single source of truth both clients
//! render. Moved verbatim out of `oura-cli/src/dashboard.rs` so the native (iOS)
//! client computes the *same* JSON as the web client; the only thing the two share
//! is this crate, so they can't drift.
//!
//! The ML models (sleep hypnogram, cardiovascular age, activity sessions) are an
//! injected [`ModelRunner`]: `oura-cli` shells out to the Python torch runners, the
//! native client runs the `.ptl` models on-device (or supplies [`NoModelRunner`]).
//! Everything else here is pure Rust over the synced SQLite DB.
//!
//! A new field added to the JSON here surfaces in BOTH clients — but each must still
//! *render* it: web `dashboard/web/app.js`, iOS `apps/ios/OuraApp/OuraApp.swift`. See
//! `docs/clients-web-and-ios.md`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use oura_store::storage::Store;

/// User anthropometrics — only the CVA model needs them; everything else is
/// signal-derived. Stored in an editable `profile.json` next to the DB.
#[derive(Clone, Copy)]
pub struct Demographics {
    pub sex: char, // 'M' | 'F' | 'O'
    pub age: f64,
    pub height_m: f64,
    pub weight_kg: f64,
    pub ring_size: f64,
}

impl Demographics {
    pub fn to_json(self) -> Value {
        json!({ "sex": self.sex.to_string(), "age": self.age, "height_m": self.height_m,
                "weight_kg": self.weight_kg, "ring_size": self.ring_size })
    }
    pub fn from_json(v: &Value) -> Self {
        let d = Demographics::default();
        Demographics {
            sex: v["sex"]
                .as_str()
                .and_then(|s| s.chars().next())
                .unwrap_or(d.sex)
                .to_ascii_uppercase(),
            age: v["age"].as_f64().unwrap_or(d.age),
            height_m: v["height_m"].as_f64().unwrap_or(d.height_m),
            weight_kg: v["weight_kg"].as_f64().unwrap_or(d.weight_kg),
            ring_size: v["ring_size"].as_f64().unwrap_or(d.ring_size),
        }
    }
}
impl Default for Demographics {
    fn default() -> Self {
        Demographics { sex: 'M', age: 30.0, height_m: 1.78, weight_kg: 75.0, ring_size: 10.0 }
    }
}

// ── the model seam ────────────────────────────────────────────────────────────
/// What the ML models need: the DB, timezone, profile, and the night windows to
/// stage. The runner returns each model's raw `--json` output (or `None`).
pub struct ModelInputs<'a> {
    pub db: &'a Path,
    pub tz: i64,
    pub demo: &'a Demographics,
    pub sleep_ranges: &'a [[i64; 2]],
}

/// Raw model outputs, matching the Python runners' `--json` shape.
#[derive(Default)]
pub struct ModelOutputs {
    pub sleep_batch: Option<Value>, // run_sleep_model.py --batch
    pub cva: Option<Value>,         // run_cva_model.py
    pub activity: Option<Value>,    // run_activity_model.py
}

/// Runs the torch models. `oura-cli` shells out to Python; the native client runs
/// `.ptl` on-device. [`NoModelRunner`] degrades to the signal-derived panels only.
pub trait ModelRunner {
    fn run(&self, input: ModelInputs) -> ModelOutputs;
}

/// No models — vitals / cardio-trend / activity-profile / device / digest only.
pub struct NoModelRunner;
impl ModelRunner for NoModelRunner {
    fn run(&self, _: ModelInputs) -> ModelOutputs {
        ModelOutputs::default()
    }
}

// ── profile + feature-mode persistence (files next to the DB) ─────────────────
pub fn profile_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or(Path::new(".")).join("profile.json")
}
/// Read the user profile (defaults if absent or malformed).
pub fn read_profile(db: &Path) -> Demographics {
    std::fs::read_to_string(profile_path(db))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| Demographics::from_json(&v))
        .unwrap_or_default()
}
pub fn write_profile(db: &Path, v: &Value) -> Result<Demographics> {
    let demo = Demographics::from_json(v);
    std::fs::write(profile_path(db), serde_json::to_vec_pretty(&demo.to_json())?)
        .context("writing profile.json")?;
    Ok(demo)
}

pub fn feature_modes_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or(Path::new(".")).join("feature_modes.json")
}
/// Real on-ring feature modes snapshotted at the last sync, as `{ feature: mode }`.
pub fn read_feature_modes(db: &Path) -> Value {
    std::fs::read_to_string(feature_modes_path(db))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null)
}
/// Record a feature's new mode (0 = off, 1 = automatic) right after a toggle.
pub fn write_feature_mode(db: &Path, feature: &str, mode_int: i64) {
    let mut modes = match read_feature_modes(db) {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    modes.insert(feature.to_string(), json!(mode_int));
    let at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    modes.insert("_at".into(), json!(at));
    let _ = std::fs::write(
        feature_modes_path(db),
        serde_json::to_vec_pretty(&Value::Object(modes)).unwrap_or_default(),
    );
}

// ── small date helpers (no chrono dep) ────────────────────────────────────────
/// Howard Hinnant's civil_from_days: days since 1970-01-01 → (year, month, day).
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (y + i64::from(m <= 2), m, d)
}
/// Inverse of `civil`: (year, month, day) → days since 1970-01-01.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

const WD: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

fn date_label(unix_s: f64, tz: i64) -> String {
    let days = (unix_s as i64 + tz * 3600).div_euclid(86400);
    let (_, m, d) = civil(days);
    let wd = WD[(days + 3).rem_euclid(7) as usize];
    format!("{wd} {m:02}-{d:02}")
}
fn hm(unix_s: f64, tz: i64) -> String {
    let sod = (unix_s as i64 + tz * 3600).rem_euclid(86400);
    format!("{:02}:{:02}", sod / 3600, (sod % 3600) / 60)
}
/// Oura "SpO2 Simple" calibration (gen4/oreo coefficients), clamped 85–100.
fn spo2_pct(r: f64) -> f64 {
    (-13.4 * r * r - 5.1 * r + 105.2).clamp(85.0, 100.0)
}

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

// ── per-night signal accumulation ────────────────────────────────────────────
#[derive(Default)]
struct Night {
    start_ds: i64,
    end_ds: i64,
    rmssd: Vec<f64>,
    hr: Vec<f64>,
    temp: Vec<f64>,
    spo2: Vec<f64>,
}

/// `(sparkline series, latest, baseline)` for a per-night vital.
type VitalStat = (Vec<f64>, Option<f64>, Option<f64>);
fn vital_stat(per_night: &[Option<f64>]) -> VitalStat {
    let series: Vec<f64> = per_night.iter().filter_map(|x| *x).collect();
    let latest = per_night.last().copied().flatten();
    let priors: Vec<f64> = per_night[..per_night.len().saturating_sub(1)]
        .iter()
        .filter_map(|x| *x)
        .collect();
    (series, latest, mean(&priors))
}
/// Percent change of `latest` vs `baseline`, guarding a zero/absent baseline.
fn vital_delta_pct(stat: &VitalStat) -> Option<f64> {
    match (stat.1, stat.2) {
        (Some(l), Some(b)) if b != 0.0 => Some(((l - b) / b * 100.0).round()),
        _ => None,
    }
}

fn feat(name: &str, on: bool, feature: &str) -> Value {
    json!({ "name": name, "on": on, "feature": feature })
}

fn downsample(arr: &[Value], n: usize) -> Vec<i64> {
    let vals: Vec<i64> = arr.iter().filter_map(|x| x.as_i64()).collect();
    if vals.len() <= n {
        return vals;
    }
    let step = vals.len() as f64 / n as f64;
    (0..n)
        .map(|i| {
            let a = (i as f64 * step) as usize;
            let b = ((i as f64 + 1.0) * step) as usize;
            let slice = &vals[a..b.min(vals.len()).max(a + 1)];
            let mut counts = [0u32; 5];
            for &s in slice {
                if (1..=4).contains(&s) {
                    counts[s as usize] += 1;
                }
            }
            (1..=4).max_by_key(|&k| counts[k as usize]).unwrap_or(2) as i64
        })
        .collect()
}

fn make_digest(hrv: &VitalStat, rhr: &VitalStat, nights: &[Night], cva: &Option<Value>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let hrv_pct = vital_delta_pct(hrv);
    let rhr_delta = match (rhr.1, rhr.2) {
        (Some(l), Some(b)) => Some(l - b),
        _ => None,
    };
    if let Some(p) = hrv_pct {
        parts.push(format!("HRV {}{:.0}%", if p >= 0.0 { "+" } else { "" }, p));
    }
    if let Some(d) = rhr_delta {
        parts.push(format!("resting HR {}{:.0} bpm", if d >= 0.0 { "+" } else { "" }, d));
    }
    let recovering = match (hrv_pct, rhr_delta) {
        (Some(p), _) => p >= 0.0,
        (None, Some(d)) => d <= 0.0,
        (None, None) => false,
    };
    let mut s = parts.join(", ");
    if !s.is_empty() {
        s.push_str(if recovering {
            ". Recovering well."
        } else {
            ". Recovery dipping, take it easy."
        });
    }
    let _ = (nights, cva);
    if s.is_empty() {
        s = "Synced. Not enough history yet for trends.".into();
    }
    s
}

/// Assemble the full dashboard summary as a JSON value. The torch models are
/// supplied by `runner` (Python subprocess on desktop, `.ptl` on-device).
pub fn build_summary(db: &Path, tz: i64, runner: &dyn ModelRunner) -> Result<Value> {
    let db_abs = std::fs::canonicalize(db).unwrap_or_else(|_| {
        std::env::current_dir().map(|d| d.join(db)).unwrap_or_else(|_| db.to_path_buf())
    });
    let db = db_abs.as_path();
    let demo = read_profile(db);
    let store = Store::open(db).context("opening DB")?;
    let events = store.decoded_events().context("reading events")?;
    if events.is_empty() {
        return Err(anyhow!("no decoded events in {} — run `oura sync` first", db.display()));
    }
    let (max_ds, anchor_unix) = events
        .iter()
        .map(|(ds, _, _, cu)| (*ds, *cu))
        .max_by_key(|(ds, _)| *ds)
        .unwrap();
    let min_ds = events.iter().map(|(ds, _, _, _)| *ds).min().unwrap();
    let unix_s = |ds: i64| -> f64 { anchor_unix as f64 - (max_ds - ds) as f64 / 10.0 };

    let mut beds: Vec<(i64, i64)> = Vec::new();
    let mut present_recent = std::collections::HashSet::new();
    let recent_cut = max_ds - 10 * 86_400;
    let name_of = |tag: u8| oura_protocol::events::event_name(tag);
    for (ds, tag, jstr, _) in &events {
        let n = name_of(*tag);
        if *ds >= recent_cut {
            present_recent.insert(n);
        }
        if n == "bedtime_period" {
            if let Ok(v) = serde_json::from_str::<Value>(jstr) {
                if let (Some(s), Some(e)) =
                    (v["bedtime_start_ds"].as_i64(), v["bedtime_end_ds"].as_i64())
                {
                    match beds.iter_mut().find(|(bs, _)| *bs == s) {
                        Some(b) => b.1 = b.1.max(e),
                        None => beds.push((s, e)),
                    }
                }
            }
        }
    }
    beds.sort();

    let mut nights: Vec<Night> = beds
        .iter()
        .map(|&(s, e)| Night { start_ds: s, end_ds: e, ..Default::default() })
        .collect();
    let find_night = |ds: i64, nights: &[Night]| {
        nights.iter().position(|nt| nt.start_ds - 600 <= ds && ds <= nt.end_ds + 600)
    };
    for (ds, tag, jstr, _) in &events {
        let Some(idx) = find_night(*ds, &nights) else {
            continue;
        };
        let n = name_of(*tag);
        let v: Value = match serde_json::from_str(jstr) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match n {
            "hrv_event" => {
                if let Some(a) = v["rmssd_ms"].as_array() {
                    nights[idx]
                        .rmssd
                        .extend(a.iter().filter_map(|x| x.as_f64()).filter(|&x| x > 0.0));
                }
                if let Some(a) = v["hr_bpm"].as_array() {
                    nights[idx]
                        .hr
                        .extend(a.iter().filter_map(|x| x.as_f64()).filter(|&x| x > 0.0));
                }
            }
            "temp_event" | "sleep_temp_event" => {
                if let Some(c) =
                    v["temps_c"].as_array().and_then(|a| a.first()).and_then(|x| x.as_f64())
                {
                    nights[idx].temp.push(c);
                }
            }
            "spo2_r_pi_event" => {
                if let Some(a) = v["r"].as_array() {
                    nights[idx].spo2.extend(
                        a.iter().filter_map(|x| x.as_f64()).filter(|&x| x > 0.0).map(spo2_pct),
                    );
                }
            }
            _ => {}
        }
    }

    // the model seam — sleep / cva / activity (Python subprocess or on-device .ptl)
    let sleep_ranges: Vec<[i64; 2]> = nights.iter().map(|nt| [nt.start_ds, nt.end_ds]).collect();
    let ModelOutputs { sleep_batch, cva, activity: activity_raw } =
        runner.run(ModelInputs { db, tz, demo: &demo, sleep_ranges: &sleep_ranges });

    let hyps: std::collections::HashMap<i64, Value> = sleep_batch
        .as_ref()
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter().filter_map(|h| Some((h["start_ds"].as_i64()?, h.clone()))).collect()
        })
        .unwrap_or_default();

    let mut nights_json = Vec::new();
    for nt in &nights {
        let hyp = hyps.get(&nt.start_ds);
        let stage_cells = hyp.and_then(|h| h["stages"].as_array()).map(|s| downsample(s, 120));
        nights_json.push(json!({
            "date": date_label(unix_s(nt.start_ds), tz),
            "start": hm(unix_s(nt.start_ds), tz),
            "end": hm(unix_s(nt.end_ds), tz),
            "in_bed_h": ((nt.end_ds - nt.start_ds) as f64 / 10.0 / 3600.0 * 10.0).round() / 10.0,
            "hrv_ms": mean(&nt.rmssd).map(|x| x.round()),
            "rhr": nt.hr.iter().cloned().fold(f64::INFINITY, f64::min).is_finite().then(|| nt.hr.iter().cloned().fold(f64::INFINITY, f64::min).round()),
            "skin_temp": mean(&nt.temp).map(|x| (x * 10.0).round() / 10.0),
            "spo2_mean": mean(&nt.spo2).map(|x| x.round()),
            "deep_pct": hyp.map(|h| h["deep_pct"].clone()),
            "light_pct": hyp.map(|h| h["light_pct"].clone()),
            "rem_pct": hyp.map(|h| h["rem_pct"].clone()),
            "wake_pct": hyp.map(|h| h["wake_pct"].clone()),
            "efficiency": hyp.map(|h| h["efficiency_pct"].clone()),
            "stages": stage_cells,
        }));
    }
    nights_json.reverse();

    let mut activity = activity_raw
        .as_ref()
        .and_then(|v| v["sessions"].as_array().cloned())
        .unwrap_or_default();

    let mut prof_sum: std::collections::BTreeMap<String, [f64; 96]> = Default::default();
    let mut prof_cnt: std::collections::BTreeMap<String, [u32; 96]> = Default::default();
    let mut daily: std::collections::BTreeMap<String, (f64, f64)> = Default::default();
    let mut met_min: std::collections::BTreeMap<i64, f64> = Default::default();
    let weight = demo.weight_kg;
    for (ds, tag, jstr, _) in &events {
        if name_of(*tag) != "activity_information" || !jstr.contains("\"met\"") {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(jstr) {
            if let Some(met) = v["met"].as_array() {
                for (i, m) in met.iter().enumerate() {
                    let mv = m.as_f64().unwrap_or(1.0);
                    let unix = anchor_unix as f64 - (max_ds - ds) as f64 / 10.0 + i as f64 * 60.0;
                    let local = unix + tz as f64 * 3600.0;
                    let day_idx = (local / 86400.0).floor() as i64;
                    let (y, mo, dd) = civil(day_idx);
                    let key = format!("{y:04}-{mo:02}-{dd:02}");
                    let bucket = (((local - day_idx as f64 * 86400.0) / 86400.0) * 96.0)
                        .floor()
                        .clamp(0.0, 95.0) as usize;
                    prof_sum.entry(key.clone()).or_insert([0.0; 96])[bucket] += (mv - 1.0).max(0.0);
                    prof_cnt.entry(key.clone()).or_insert([0; 96])[bucket] += 1;
                    let step_rate = if mv >= 7.0 {
                        150.0
                    } else if mv >= 2.5 {
                        105.0
                    } else {
                        0.0
                    };
                    let e = daily.entry(key).or_insert((0.0, 0.0));
                    e.0 += (mv - 1.0).max(0.0) * weight / 60.0;
                    e.1 += step_rate;
                    *met_min.entry((local / 60.0).floor() as i64).or_insert(0.0) +=
                        (mv - 1.0).max(0.0);
                }
            }
        }
    }
    for sess in activity.iter_mut() {
        let (Some(start), Some(dur)) = (sess["start"].as_str(), sess["duration_min"].as_f64())
        else {
            continue;
        };
        let parse = || -> Option<i64> {
            let (date, time) = start.split_once(' ')?;
            let mut dp = date.split('-');
            let y: i64 = dp.next()?.parse().ok()?;
            let mo: u32 = dp.next()?.parse().ok()?;
            let dd: u32 = dp.next()?.parse().ok()?;
            let mut tp = time.split(':');
            let hh: i64 = tp.next()?.parse().ok()?;
            let mm: i64 = tp.next()?.parse().ok()?;
            Some(days_from_civil(y, mo, dd) * 1440 + hh * 60 + mm)
        };
        if let Some(m0) = parse() {
            let kcal: f64 = (m0..m0 + dur as i64)
                .filter_map(|m| met_min.get(&m))
                .map(|met| met * weight / 60.0)
                .sum();
            sess["active_kcal"] = json!(kcal.round());
        }
    }

    let activity_daily: Value = daily
        .iter()
        .map(|(k, (act, steps))| {
            (
                k.clone(),
                json!({
                    "active_kcal": act.round(),
                    "total_kcal": (weight * 24.0 + act).round(),
                    "steps": (steps / 100.0).round() * 100.0,
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    let activity_profile: Value = prof_sum
        .iter()
        .map(|(k, sums)| {
            let cnt = &prof_cnt[k];
            let arr: Vec<f64> = sums
                .iter()
                .zip(cnt.iter())
                .map(|(s, c)| if *c > 0 { (s / *c as f64 * 100.0).round() / 100.0 } else { 0.0 })
                .collect();
            (k.clone(), json!(arr))
        })
        .collect::<serde_json::Map<_, _>>()
        .into();

    let hrv_by_night: Vec<Option<f64>> = nights.iter().map(|n| mean(&n.rmssd)).collect();
    let rhr_by_night: Vec<Option<f64>> =
        nights.iter().map(|n| n.hr.iter().cloned().reduce(f64::min)).collect();
    let hrv_stat = vital_stat(&hrv_by_night);
    let rhr_stat = vital_stat(&rhr_by_night);
    let trend = |stat: &VitalStat| -> Value {
        let (series, latest, base) = stat;
        json!({
            "series": series.iter().map(|x| x.round()).collect::<Vec<_>>(),
            "latest": latest.map(|x| x.round()),
            "baseline": base.map(|b| (b * 10.0).round() / 10.0),
            "delta_pct": vital_delta_pct(stat),
        })
    };

    let has = |evname: &str| present_recent.contains(evname);
    let modes = read_feature_modes(db);
    let cap_on = |feature: &str, present: bool| -> bool {
        modes.get(feature).and_then(Value::as_i64).map(|m| m != 0).unwrap_or(present)
    };
    let measuring = json!([
        feat("Daytime HR", cap_on("daytime_hr", has("ibi_and_amplitude_event") || has("green_ibi_quality_event")), "daytime_hr"),
        feat("SpO2", cap_on("spo2", has("spo2_r_pi_event")), "spo2"),
        feat("Exercise HR", cap_on("exercise_hr", has("ehr_trace_event")), "exercise_hr"),
        feat("Real steps", cap_on("real_steps", has("real_step_event_feature_1") || has("real_step_event_feature_2")), "real_steps"),
        feat("Cardio PPG (CVA)", cap_on("cva_ppg", has("cva_raw_ppg_data")), "cva_ppg"),
    ]);

    let mut sc: std::collections::BTreeMap<&str, i64> = Default::default();
    for (_ds, tag, _j, _) in &events {
        let cat = match name_of(*tag) {
            "spo2_r_pi_event" => Some("Blood oxygen"),
            "ibi_and_amplitude_event" | "green_ibi_quality_event" => Some("Heart beats"),
            "ehr_trace_event" | "ehr_acm_intensity_event" => Some("Exercise HR"),
            "motion_event" | "sleep_acm_period" => Some("Motion"),
            "temp_event" | "sleep_temp_event" => Some("Skin temp"),
            "real_step_event_feature_1" | "real_step_event_feature_2" => Some("Steps"),
            "cva_raw_ppg_data" => Some("Cardio PPG"),
            _ => None,
        };
        if let Some(c) = cat {
            *sc.entry(c).or_insert(0) += 1;
        }
    }
    let mut sv: Vec<(&str, i64)> = sc.into_iter().collect();
    sv.sort_by(|a, b| b.1.cmp(&a.1));
    let streams = json!(sv.iter().map(|(n, c)| json!({ "name": n, "count": c })).collect::<Vec<_>>());

    let mut allc: std::collections::BTreeMap<&str, i64> = Default::default();
    for (_ds, tag, _j, _) in &events {
        *allc.entry(name_of(*tag)).or_insert(0) += 1;
    }
    let mut allv: Vec<(&str, i64)> = allc.into_iter().collect();
    allv.sort_by(|a, b| b.1.cmp(&a.1));
    let event_counts =
        json!(allv.iter().map(|(n, c)| json!({ "name": n, "count": c })).collect::<Vec<_>>());
    let insight = |name: &str, live: bool, why: &str| json!({"name": name, "status": if live {"live"} else {"gated"}, "why": why});
    let insights = json!([
        insight("Sleep stages", true, ""),
        insight("Apnea / breathing", has("ibi_and_amplitude_event"), "needs overnight IBI"),
        insight("Cardiovascular age", has("cva_raw_ppg_data"), "enable cva_ppg"),
        insight("SpO2", has("spo2_r_pi_event"), "enable spo2"),
        insight("Activity sessions", true, ""),
        insight("HRV / resting HR", true, ""),
        insight("Steps", has("real_step_event_feature_1") || has("real_step_event_feature_2"), "enable real_steps"),
        insight("Stress / resilience", false, "needs cloud scores"),
    ]);
    let mut battery: Option<(i64, i64)> = None;
    for (_ds, tag, jstr, _) in &events {
        if name_of(*tag) == "debug_data" && jstr.contains("battery_pct") {
            if let Ok(v) = serde_json::from_str::<Value>(jstr) {
                if let Some(p) = v["battery_pct"].as_i64() {
                    battery = Some((p, v["voltage_mv"].as_i64().unwrap_or(0)));
                }
            }
        }
    }

    let dev = store.device_info().ok().flatten();
    let last_sync = dev.as_ref().map(|d| d.6).filter(|&t| t > 0);
    let synced_unix = last_sync.map(|t| t as f64);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as f64)
        .unwrap_or(anchor_unix as f64);
    let device = json!({
        "serial": dev.as_ref().map(|d| d.0.clone()),
        "hardware_id": dev.as_ref().map(|d| d.1.clone()).filter(|s| !s.is_empty()),
        "firmware": dev.as_ref().map(|d| d.2.clone()).filter(|s| !s.is_empty()),
        "api_version": dev.as_ref().map(|d| d.3.clone()).filter(|s| !s.is_empty()),
        "mac": dev.as_ref().map(|d| d.4.clone()).filter(|s| !s.is_empty()),
        "synced": synced_unix.map(|s| date_label(s, tz)),
        "synced_hm": synced_unix.map(|s| hm(s, tz)),
        "fresh_hours": synced_unix.map(|s| ((now - s) / 3600.0 * 10.0).round() / 10.0),
        "days_of_data": ((max_ds - min_ds) as f64 / 10.0 / 86400.0 * 10.0).round() / 10.0,
        "total_events": events.len(),
        "nights": nights.len(),
        "battery_pct": battery.map(|b| b.0),
        "battery_v": battery.map(|b| (b.1 as f64 / 1000.0 * 100.0).round() / 100.0),
        "measuring": measuring,
        "streams": streams,
        "event_counts": event_counts,
        "next_cursor": dev.as_ref().map(|d| d.7),
        "insights": insights,
    });

    let digest = make_digest(&hrv_stat, &rhr_stat, &nights, &cva);

    Ok(json!({
        "generated_at": now,
        "tz": tz,
        "digest": digest,
        "device": device,
        "profile": demo.to_json(),
        "nights": nights_json,
        "cardio": cva,
        "activity": activity,
        "activity_profile": activity_profile,
        "activity_daily": activity_daily,
        "vitals": { "hrv": trend(&hrv_stat), "rhr": trend(&rhr_stat) },
    }))
}
