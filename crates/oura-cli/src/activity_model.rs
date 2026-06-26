//! In-process activity detection via Oura's own TorchScript model, run through
//! LibTorch (the `tch` crate) — the Rust-native alternative to shelling out to
//! `tools/run_activity_model.py`. Compiled only under `--features torch`.
//!
//! This loads `notes/models/automatic_activity_detection_3_1_11.pt` and feeds it
//! the exact same met/motion/temperature/heartrate tensors the Python runner
//! builds, so it returns bit-identical segments. See `docs/activity-model-runner.md`.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use oura_store::storage::Store;
use tch::{CModule, IValue, Kind, Tensor};

const MODEL_REL: &str = "notes/models/automatic_activity_detection_3_1_11.pt";
const MODEL_VERSION: &str = "3.1.11";

/// behavior-id -> name, from the model's behavior table (ActivityTypes.json).
fn behavior(id: i64) -> String {
    let name = match id {
        -1 => "nothing", 0 => "<empty>", 1 => "badminton", 2 => "boxing",
        3 => "crossCountrySkiing", 4 => "crossTraining", 5 => "cycling", 6 => "dance",
        7 => "elliptical", 8 => "strengthTraining", 9 => "hockey", 10 => "pilates",
        11 => "rowing", 12 => "running", 13 => "swimming", 14 => "walking", 15 => "yoga",
        16 => "golf", 17 => "tennis", 18 => "climbing", 19 => "downhillSkiing",
        20 => "snowboarding", 21 => "hiking", 22 => "horsebackRiding", 23 => "volleyball",
        24 => "basketball", 25 => "americanFootball", 26 => "soccer", 27 => "baseball",
        28 => "coreExercise", 29 => "cricket", 30 => "HIIT", 31 => "diving",
        32 => "fitnessClass", 33 => "floorball", 34 => "gymnastics", 35 => "handball",
        36 => "houseWork", 37 => "iceSkating", 38 => "jumpingRope", 39 => "martialArts",
        40 => "flexibility", 41 => "mountainBiking", 42 => "nordicWalking", 48 => "stairExercise",
        49 => "stretching", 50 => "surfing", 51 => "waterFitness", 52 => "yardwork", 53 => "padel",
        69 => "skateboarding", 65535 => "other", 65536 => "nap", 65537 => "sleep",
        65538 => "pause", 70937 => "meditation", 71201 => "eating", 71227 => "relax",
        71239 => "transport",
        _ => return id.to_string(),
    };
    name.to_string()
}

/// Days-since-epoch -> (year, month, day). Howard Hinnant's civil_from_days.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

/// 2-D float32 tensor from row-major `rows` of `cols` each (handles empty).
fn mat(rows: &[Vec<f32>], cols: i64) -> Tensor {
    let n = rows.len() as i64;
    let flat: Vec<f32> = rows.iter().flatten().copied().collect();
    Tensor::from_slice(&flat).reshape([n, cols])
}

/// 0-dim float32 scalar, matching Python's `torch.tensor(x)`.
fn scalar(x: f64) -> Tensor {
    Tensor::from(x).to_kind(Kind::Float)
}

struct Session {
    start: String, // "YYYY-MM-DD HH:MM"
    end: String,   // "HH:MM"
    duration_min: i64,
    is_workout: f64,
    top3: Vec<(String, f64)>,
}

pub fn run(db: &Path, root: &Path, tz: i64, threshold: f64, json: bool) -> Result<()> {
    let model = root.join(MODEL_REL);
    if !model.is_file() {
        bail!("model not found: {}", model.display());
    }

    let store = Store::open(db)?;
    let events = store.decoded_events()?;
    if events.is_empty() {
        bail!("no decoded events in {} (run `oura sync` first)", db.display());
    }

    // Anchor ring deciseconds to wall-clock via the latest event's capture time.
    let (max_ds, anchor_unix) = events
        .iter()
        .map(|(ds, _, _, cu)| (*ds, *cu))
        .max_by_key(|(ds, _)| *ds)
        .unwrap();
    let min_ds = events.iter().map(|(ds, _, _, _)| *ds).min().unwrap();
    let unix_min = |ds: i64| -> f64 { (anchor_unix as f64 - (max_ds - ds) as f64 / 10.0) / 60.0 };
    // Rebase by whole days: keeps time-of-day but stays float32-exact (unix-minutes
    // ~29.7M exceed 2^24 and silently break the model's exact time alignment).
    let offset = (unix_min(min_ds) / 1440.0).floor() as i64 * 1440;
    let tmin = |ds: i64| -> i64 { unix_min(ds).round() as i64 - offset };

    // Build per-series rows from decoded events (same tag map as the Python runner).
    let acm_scale: f32 = std::env::var("ACM_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let mut met_map: std::collections::HashMap<i64, f32> = Default::default();
    let mut met_order: Vec<i64> = Vec::new();
    let mut motion: Vec<Vec<f32>> = Vec::new();
    let mut temp: Vec<Vec<f32>> = Vec::new();
    let mut hr: Vec<Vec<f32>> = Vec::new();
    let getf = |v: &serde_json::Value, k: &str| -> f32 { v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32 };
    for (ds, tag, jsonstr, _) in &events {
        let v: serde_json::Value = match serde_json::from_str(jsonstr) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let t = tmin(*ds);
        match tag {
            0x50 => {
                if let Some(a) = v.get("met").and_then(|m| m.as_array()) {
                    for (i, m) in a.iter().enumerate() {
                        if let Some(m) = m.as_f64() {
                            let key = t + i as i64;
                            if met_map.insert(key, m as f32).is_none() {
                                met_order.push(key);
                            }
                        }
                    }
                }
            }
            0x47 => motion.push(vec![
                t as f32,
                getf(&v, "orientation"),
                getf(&v, "motion_seconds"),
                getf(&v, "avg_x") * acm_scale,
                getf(&v, "avg_y") * acm_scale,
                getf(&v, "avg_z") * acm_scale,
                f32::NAN, // regular_motion: no source
                getf(&v, "low_intensity"),
                getf(&v, "high_intensity"),
            ]),
            0x46 => {
                if let Some(c) = v.get("temps_c").and_then(|c| c.as_array()).and_then(|c| c.first()) {
                    if let Some(c) = c.as_f64() {
                        temp.push(vec![t as f32, c as f32]);
                    }
                }
            }
            0x80 => {
                if let Some(a) = v.get("hr_bpm").and_then(|h| h.as_array()) {
                    let vals: Vec<f64> = a.iter().filter_map(|x| x.as_f64()).collect();
                    if !vals.is_empty() {
                        hr.push(vec![t as f32, (vals.iter().sum::<f64>() / vals.len() as f64) as f32]);
                    }
                }
            }
            _ => {}
        }
    }

    // met: dedupe by minute (last wins), then sort by (t, value) like the Python.
    let mut met: Vec<Vec<f32>> = met_order.iter().map(|&t| vec![t as f32, met_map[&t]]).collect();
    met.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap().then(a[1].partial_cmp(&b[1]).unwrap()));
    if met.is_empty() {
        bail!("no MET (activity_information) events found — cannot run the model");
    }
    motion.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap());
    temp.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap());
    hr.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap());

    let met_t = mat(&met, 2);
    let motion_t = mat(&motion, 9);
    let temp_t = mat(&temp, 2);
    let hr_t = mat(&hr, 2);
    // stepmotion stub: NaN features spanning the FULL range, else its last
    // timestamp caps the model's last_valid_time and truncates every series.
    let step_t = Tensor::full([2, 12], f64::NAN, (Kind::Float, tch::Device::Cpu));
    let _ = step_t.get(0).get(0).fill_(met[0][0] as f64);
    let _ = step_t.get(1).get(0).fill_(met[met.len() - 1][0] as f64);

    // context [year, month, day, weekday(Mon=0)] from the anchor in local time.
    let t_local = anchor_unix + tz * 3600;
    let days = t_local.div_euclid(86400);
    let (y, mo, d) = civil(days);
    let weekday = (days + 3).rem_euclid(7); // 1970-01-01 was Thursday(=3, Mon=0)
    let context = Tensor::from_slice(&[y as f32, mo as f32, d as f32, weekday as f32]);
    let mut user_v = vec![30.0f32, 1.0, 1.78, 78.0];
    user_v.extend(std::iter::repeat(f32::NAN).take(10));
    let user = Tensor::from_slice(&user_v);

    let module = CModule::load(&model).with_context(|| format!("loading {}", model.display()))?;
    let inputs = vec![
        IValue::Tensor(context),
        IValue::Tensor(user),
        IValue::Tensor(met_t),
        IValue::Tensor(step_t),
        IValue::Tensor(motion_t),
        IValue::Tensor(temp_t),
        IValue::Tensor(hr_t),
        IValue::None,
        IValue::None,
        IValue::Tensor(scalar(threshold)),
        IValue::Tensor(scalar(5.0)), // minimum_duration_minutes
        IValue::Tensor(scalar(0.0)), // allow_non_wear
    ];
    let out = module.forward_is(&inputs).context("running the activity model")?;

    // forward returns (workouts[n,9], _, segments); we want workouts.
    let workouts = match out {
        IValue::Tuple(mut items) if !items.is_empty() => match items.swap_remove(0) {
            IValue::Tensor(t) => t,
            other => return Err(anyhow!("unexpected first output: {:?}", other)),
        },
        IValue::Tensor(t) => t,
        other => return Err(anyhow!("unexpected model output: {:?}", other)),
    };

    // to_local for a rebased minute value. Keep the minute as a float through the
    // *60 scaling (like the Python runner's `(minute + OFFSET) * 60`) so fractional
    // minute boundaries don't get truncated to a different wall-clock minute.
    let to_local = |minute: f64| -> (String, String) {
        let secs = ((minute + offset as f64) * 60.0 + (tz * 3600) as f64) as i64;
        let days = secs.div_euclid(86400);
        let sod = secs.rem_euclid(86400);
        let (y, mo, d) = civil(days);
        (
            format!("{:04}-{:02}-{:02} {:02}:{:02}", y, mo, d, sod / 3600, (sod % 3600) / 60),
            format!("{:02}:{:02}", sod / 3600, (sod % 3600) / 60),
        )
    };

    // workouts[n,9] = [start_min, end_min, is_workout, id1,p1, id2,p2, id3,p3]
    let n = if workouts.dim() == 2 { workouts.size()[0] } else { 0 };
    let mut sessions = Vec::new();
    for i in 0..n {
        let w: Vec<f64> = (0..9).map(|j| workouts.double_value(&[i, j])).collect();
        let (start_full, _) = to_local(w[0]);
        let (_, end_hm) = to_local(w[1]);
        let top3: Vec<(String, f64)> = (0..3)
            .map(|k| (behavior(w[3 + 2 * k] as i64), (w[4 + 2 * k] * 1000.0).round() / 1000.0))
            .collect();
        sessions.push(Session {
            start: start_full,
            end: end_hm,
            duration_min: (w[1] - w[0]).round() as i64,
            is_workout: (w[2] * 1000.0).round() / 1000.0,
            top3,
        });
    }

    if json {
        let arr: Vec<serde_json::Value> = sessions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "start": s.start,
                    "end": s.end,
                    "duration_min": s.duration_min,
                    "is_workout": s.is_workout,
                    "label": s.top3[0].0,
                    "label_confidence": s.top3[0].1,
                    "top3": s.top3.iter().map(|(n, p)| serde_json::json!([n, p])).collect::<Vec<_>>(),
                })
            })
            .collect();
        let doc = serde_json::json!({ "model": MODEL_VERSION, "sessions": arr });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }

    println!(
        "Activity sessions — Oura automatic_activity_detection v{MODEL_VERSION} (the ring's own model, in-process via LibTorch)\n"
    );
    if sessions.is_empty() {
        println!("  No activity segments detected.");
        return Ok(());
    }
    println!("  {:<10} {:<13} {:>4}  {:>7}  activity (model confidence)", "date", "time", "dur", "workout");
    for s in &sessions {
        let (date, hm) = s.start.split_once(' ').unwrap();
        let span = format!("{}-{}", hm, s.end);
        let mark = if s.is_workout >= threshold { "✓" } else { " " };
        let wk = format!("{:.2} {}", s.is_workout, mark);
        let alt = s.top3[1..].iter().map(|(n, p)| format!("{n} {p:.2}")).collect::<Vec<_>>().join("   ");
        println!(
            "  {date:<10} {span:<13} {:>3}m  {wk:>7}  {} {:.2}   ·   {alt}",
            s.duration_min, s.top3[0].0, s.top3[0].1,
        );
    }
    println!("\n  Labels are Oura's model, not a heuristic. ✓ = is_workout ≥ {threshold:.2}.");
    println!("  Type accuracy is limited: the gait ('stepmotion') input needs raw IMU we");
    println!("  can't sync, so it's stubbed — timing/detection is solid, the sport label is");
    println!("  the model's best guess from MET/motion/HR/temp.");
    Ok(())
}
