//! `oura dashboard` — a local web health dashboard following
//! `notes/dashboard-v2-brainstorm.md`.
//!
//! Rust owns the DB + every non-model calculation (per-night HRV/RHR/skin-temp,
//! SpO2 % via Oura's calibration, device/data-health, baselines + deltas, the
//! digest) and the HTTP server. The torch models (sleep hypnogram, activity, CVA)
//! run through the Python runners, shelled out exactly like `oura sessions`. All
//! data stays on this machine.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use oura_store::storage::Store;

const INDEX_HTML: &str = include_str!("../../../dashboard/web/index.html");
const STYLES_CSS: &str = include_str!("../../../dashboard/web/styles.css");
const APP_JS: &str = include_str!("../../../dashboard/web/app.js");

/// User anthropometrics — only the CVA model needs them; everything else is
/// signal-derived. Stored in an editable, gitignored `profile.json` next to the DB
/// (the ring can't report these), used by the CVA model and the runners.
#[derive(Clone, Copy)]
pub struct Demographics {
    pub sex: char, // 'M' | 'F' | 'O'
    pub age: f64,
    pub height_m: f64,
    pub weight_kg: f64,
    pub ring_size: f64,
}

impl Demographics {
    fn to_json(self) -> Value {
        json!({ "sex": self.sex.to_string(), "age": self.age, "height_m": self.height_m,
                "weight_kg": self.weight_kg, "ring_size": self.ring_size })
    }
    fn from_json(v: &Value) -> Self {
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
        Demographics {
            sex: 'M',
            age: 30.0,
            height_m: 1.78,
            weight_kg: 75.0,
            ring_size: 10.0,
        }
    }
}

fn profile_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or(Path::new(".")).join("profile.json")
}
/// Read the user profile (defaults if absent or malformed).
fn read_profile(db: &Path) -> Demographics {
    std::fs::read_to_string(profile_path(db))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| Demographics::from_json(&v))
        .unwrap_or_default()
}
fn write_profile(db: &Path, v: &Value) -> Result<Demographics> {
    let demo = Demographics::from_json(v);
    std::fs::write(
        profile_path(db),
        serde_json::to_vec_pretty(&demo.to_json())?,
    )
    .context("writing profile.json")?;
    Ok(demo)
}

fn read_ring_key(path: Option<&Path>) -> Result<String> {
    let path = path.ok_or_else(|| anyhow!("dashboard was started without --key-file"))?;
    let key = std::fs::read_to_string(path)
        .with_context(|| format!("reading key file {}", path.display()))?;
    validate_ring_key(&key)
}

fn write_ring_key(path: Option<&Path>, key: &str) -> Result<String> {
    let path = path.ok_or_else(|| anyhow!("dashboard was started without --key-file"))?;
    let key = validate_ring_key(key)?;
    std::fs::write(path, format!("{key}\n"))
        .with_context(|| format!("writing key file {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(key)
}

fn validate_ring_key(key: &str) -> Result<String> {
    let key = key.trim();
    if key.len() != 32 || !key.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return Err(anyhow!("auth key must be exactly 16 bytes of hex"));
    }
    Ok(key.to_ascii_lowercase())
}

fn feature_modes_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or(Path::new(".")).join("feature_modes.json")
}

/// Real on-ring feature modes snapshotted at the last sync (`feature_modes.json` next
/// to the DB), as `{ feature_name: mode_int }`. `Value::Null` if not captured yet.
fn read_feature_modes(db: &Path) -> Value {
    std::fs::read_to_string(feature_modes_path(db))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null)
}

/// Record a feature's new mode into `feature_modes.json` right after a successful
/// toggle, so the dashboard reflects it immediately instead of waiting for the next
/// sync to re-snapshot. `mode_int`: 0 = off, 1 = automatic (matches `feature_mode`).
fn write_feature_mode(db: &Path, feature: &str, mode_int: i64) {
    let mut modes = match read_feature_modes(db) {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    modes.insert(feature.to_string(), json!(mode_int));
    let at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    modes.insert("_at".into(), json!(at));
    let _ = std::fs::write(feature_modes_path(db), serde_json::to_vec_pretty(&Value::Object(modes)).unwrap_or_default());
}

// ── small date helpers (no chrono dep) ────────────────────────────────────────
/// Howard Hinnant's civil_from_days: days since 1970-01-01 → (year, month, day).
fn civil(days: i64) -> (i64, u32, u32) {
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
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
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

// ── python model orchestration (same pattern as `oura sessions`) ──────────────
fn repo_root() -> Option<PathBuf> {
    let marker = Path::new("tools/run_activity_model.py");
    let find = |start: &Path| {
        start
            .ancestors()
            .find(|d| d.join(marker).is_file())
            .map(Path::to_path_buf)
    };
    std::env::current_dir()
        .ok()
        .and_then(|d| find(&d))
        .or_else(|| find(Path::new(env!("CARGO_MANIFEST_DIR"))))
}

fn python_bin(root: &Path) -> PathBuf {
    let venv = root.join(".venv/bin/python");
    if venv.is_file() {
        venv
    } else {
        PathBuf::from("python3")
    }
}

/// Run a python runner and parse its `--json` stdout. Returns None on any failure
/// (missing venv/model, night with no data, …) so the dashboard degrades softly.
fn run_py_json(root: &Path, py: &Path, script: &str, args: &[String]) -> Option<Value> {
    let out = Command::new(py)
        .current_dir(root)
        .arg(root.join(script))
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// Like `run_py_json` but feeds `stdin` to the process — used for the batched sleep
/// runner, which reads its list of night ranges from stdin. The payload is small
/// (a few night pairs), so writing it before draining stdout can't deadlock.
fn run_py_json_stdin(
    root: &Path,
    py: &Path,
    script: &str,
    args: &[String],
    stdin: &[u8],
) -> Option<Value> {
    use std::io::Write;
    let mut child = Command::new(py)
        .current_dir(root)
        .arg(root.join(script))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(stdin).ok()?; // dropped here → EOF
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

// ── per-night signal accumulation (one pass over events) ──────────────────────
#[derive(Default)]
struct Night {
    start_ds: i64,
    end_ds: i64,
    rmssd: Vec<f64>,
    hr: Vec<f64>,
    temp: Vec<f64>,
    spo2: Vec<f64>,
}

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// A per-night vital (oldest→newest, `None` where a night had no sample) reduced to
/// `(sparkline series, latest, baseline)`. `latest` is the *most recent night's*
/// value — `None` if that night had no sample, so a stale older night is never shown
/// as current. `baseline` is the mean of the prior nights' available values. The
/// tiles and the digest both read from this, so their numbers can't disagree.
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

/// Assemble the full dashboard summary as a JSON value.
pub fn build_summary(db: &Path, tz: i64) -> Result<Value> {
    // Resolve the DB to an absolute path up front. The Python model runners execute
    // with `current_dir` set to the repo root, so a relative `--db` would resolve
    // against a different directory than Rust (which uses the process cwd) — mixing
    // vitals from one file with sleep/activity/CVA from another. An absolute path
    // makes both sides open the same DB.
    let db_abs = std::fs::canonicalize(db).unwrap_or_else(|_| {
        std::env::current_dir()
            .map(|d| d.join(db))
            .unwrap_or_else(|_| db.to_path_buf())
    });
    let db = db_abs.as_path();
    let demo = read_profile(db);
    let store = Store::open(db).context("opening DB")?;
    let events = store.decoded_events().context("reading events")?;
    if events.is_empty() {
        return Err(anyhow!(
            "no decoded events in {} — run `oura sync` first",
            db.display()
        ));
    }
    let (max_ds, anchor_unix) = events
        .iter()
        .map(|(ds, _, _, cu)| (*ds, *cu))
        .max_by_key(|(ds, _)| *ds)
        .unwrap();
    let min_ds = events.iter().map(|(ds, _, _, _)| *ds).min().unwrap();
    let unix_s = |ds: i64| -> f64 { anchor_unix as f64 - (max_ds - ds) as f64 / 10.0 };

    // distinct bedtime windows (dedup by start, keep the longest) + which event
    // types are present in the last ~day (for the "what's measuring" panel)
    let mut beds: Vec<(i64, i64)> = Vec::new();
    let mut present_recent = std::collections::HashSet::new();
    let recent_cut = max_ds - 10 * 86_400; // ~1 day of deciseconds
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

    // one pass: accumulate per-night HRV/HR/temp/SpO2
    let mut nights: Vec<Night> = beds
        .iter()
        .map(|&(s, e)| Night {
            start_ds: s,
            end_ds: e,
            ..Default::default()
        })
        .collect();
    let find_night = |ds: i64, nights: &[Night]| {
        nights
            .iter()
            .position(|nt| nt.start_ds - 600 <= ds && ds <= nt.end_ds + 600)
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
                if let Some(c) = v["temps_c"]
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|x| x.as_f64())
                {
                    nights[idx].temp.push(c);
                }
            }
            "spo2_r_pi_event" => {
                if let Some(a) = v["r"].as_array() {
                    nights[idx].spo2.extend(
                        a.iter()
                            .filter_map(|x| x.as_f64())
                            .filter(|&x| x > 0.0)
                            .map(spo2_pct),
                    );
                }
            }
            _ => {}
        }
    }

    // shell out to the models. Sleep (all nights in one batched process), cardio,
    // and activity are independent torch runs, so launch them concurrently instead
    // of paying ~1 s of torch import + a full DB scan once per call, serially.
    let root = repo_root();
    let py = root.as_deref().map(python_bin);

    let sleep_ranges: Vec<[i64; 2]> = nights.iter().map(|nt| [nt.start_ds, nt.end_ds]).collect();
    let sleep_stdin = serde_json::to_vec(&sleep_ranges).unwrap_or_default();
    let sleep_args = vec![
        db.display().to_string(),
        tz.to_string(),
        "--json".into(),
        "--batch".into(),
    ];
    let cva_args = vec![
        db.display().to_string(),
        "--json".into(),
        "--sex".into(),
        demo.sex.to_string(),
        "--age".into(),
        demo.age.to_string(),
        "--height".into(),
        demo.height_m.to_string(),
        "--weight".into(),
        demo.weight_kg.to_string(),
        "--ring".into(),
        demo.ring_size.to_string(),
    ];
    let act_args = vec![
        db.display().to_string(),
        "--tz".into(),
        tz.to_string(),
        "--json".into(),
    ];

    let (sleep_batch, cva, activity_raw) = match (root.as_deref(), py.as_deref()) {
        (Some(r), Some(p)) => std::thread::scope(|s| {
            let sh = s.spawn(|| {
                run_py_json_stdin(r, p, "tools/run_sleep_model.py", &sleep_args, &sleep_stdin)
            });
            let ch = s.spawn(|| run_py_json(r, p, "tools/run_cva_model.py", &cva_args));
            let ah = s.spawn(|| run_py_json(r, p, "tools/run_activity_model.py", &act_args));
            (
                sh.join().ok().flatten(),
                ch.join().ok().flatten(),
                ah.join().ok().flatten(),
            )
        }),
        _ => (None, None, None),
    };

    // index the batched sleep results by start_ds so each night picks up its own
    // hypnogram (the runner preserves order, but match by key to be safe).
    let hyps: std::collections::HashMap<i64, Value> = sleep_batch
        .as_ref()
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|h| Some((h["start_ds"].as_i64()?, h.clone())))
                .collect()
        })
        .unwrap_or_default();

    // nights JSON (newest first), each enriched with the hypnogram from the model
    let mut nights_json = Vec::new();
    for nt in &nights {
        let hyp = hyps.get(&nt.start_ds);
        // downsample the per-30s stage array to ~120 cells for the bar
        let stage_cells = hyp
            .and_then(|h| h["stages"].as_array())
            .map(|s| downsample(s, 120));
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

    // activity sessions
    let mut activity = activity_raw
        .as_ref()
        .and_then(|v| v["sessions"].as_array().cloned())
        .unwrap_or_default();

    // per-day movement profile: 96 buckets/day (15 min) of mean MET above rest,
    // so the actogram can show a continuous activity ridge, not just sparse blocks.
    let mut prof_sum: std::collections::BTreeMap<String, [f64; 96]> = Default::default();
    let mut prof_cnt: std::collections::BTreeMap<String, [u32; 96]> = Default::default();
    // per-day energy + steps: active kcal = Σ(MET-1)·weight/60; steps estimated from
    // MET cadence (the exact count needs the on-ring step-counter model).
    let mut daily: std::collections::BTreeMap<String, (f64, f64)> = Default::default(); // (active_kcal, steps)
                                                                                        // per-local-minute MET above rest, so we can sum active kcal over a session's window.
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
                    e.0 += (mv - 1.0).max(0.0) * weight / 60.0; // active kcal this minute
                    e.1 += step_rate; // steps this minute
                    *met_min.entry((local / 60.0).floor() as i64).or_insert(0.0) +=
                        (mv - 1.0).max(0.0);
                }
            }
        }
    }
    // estimated active kcal per detected session: Σ(MET-1)·weight/60 over its minutes,
    // matched by local time. start is "YYYY-MM-DD HH:MM" (local); duration_min is its length.
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

    // total daily kcal ≈ 24h basal (weight kcal/kg/h) + active
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
                .map(|(s, c)| {
                    if *c > 0 {
                        (s / *c as f64 * 100.0).round() / 100.0
                    } else {
                        0.0
                    }
                })
                .collect();
            (k.clone(), json!(arr))
        })
        .collect::<serde_json::Map<_, _>>()
        .into();

    // vitals trend, per night oldest→newest (None where a night had no sample, so
    // the "latest" stays tied to the most recent night rather than silently sliding
    // to an older one).
    let hrv_by_night: Vec<Option<f64>> = nights.iter().map(|n| mean(&n.rmssd)).collect();
    let rhr_by_night: Vec<Option<f64>> = nights
        .iter()
        .map(|n| n.hr.iter().cloned().reduce(f64::min))
        .collect();
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

    // device & data-health
    let has = |evname: &str| present_recent.contains(evname);
    // Prefer the real on-ring feature mode (captured into feature_modes.json on the
    // last sync); fall back to "events seen recently" when we don't have a mode yet.
    let modes = read_feature_modes(db);
    let cap_on = |feature: &str, present: bool| -> bool {
        modes
            .get(feature)
            .and_then(Value::as_i64)
            .map(|m| m != 0)
            .unwrap_or(present)
    };
    let measuring = json!([
        feat(
            "Daytime HR",
            cap_on(
                "daytime_hr",
                has("ibi_and_amplitude_event") || has("green_ibi_quality_event")
            ),
            "daytime_hr"
        ),
        feat("SpO2", cap_on("spo2", has("spo2_r_pi_event")), "spo2"),
        feat(
            "Exercise HR",
            cap_on("exercise_hr", has("ehr_trace_event")),
            "exercise_hr"
        ),
        feat(
            "Real steps",
            cap_on(
                "real_steps",
                has("real_step_event_feature_1") || has("real_step_event_feature_2")
            ),
            "real_steps"
        ),
        feat(
            "Cardio PPG (CVA)",
            cap_on("cva_ppg", has("cva_raw_ppg_data")),
            "cva_ppg"
        ),
    ]);

    // data streams: how much of each biometric the ring is actually recording
    let mut sc: std::collections::BTreeMap<&str, i64> = Default::default();
    for (_ds, tag, _j, _) in &events {
        let cat = match name_of(*tag) {
            "spo2_r_pi_event" => Some("Blood oxygen"),
            "ibi_and_amplitude_event" | "green_ibi_quality_event" => Some("Heart beats"),
            "ehr_trace_event" | "ehr_acm_intensity_event" => Some("Exercise HR"),
            "motion_event" | "sleep_acm_period" => Some("Motion"),
            "temp_event" => Some("Skin temp"),
            "real_step_event_feature_1" => Some("Steps"),
            "cva_raw_ppg_data" => Some("Cardio PPG"),
            _ => None,
        };
        if let Some(c) = cat {
            *sc.entry(c).or_insert(0) += 1;
        }
    }
    let mut sv: Vec<(&str, i64)> = sc.into_iter().collect();
    sv.sort_by(|a, b| b.1.cmp(&a.1));
    let streams = json!(sv
        .iter()
        .map(|(n, c)| json!({ "name": n, "count": c }))
        .collect::<Vec<_>>());

    // full per-type event breakdown (for the advanced/debug section)
    let mut allc: std::collections::BTreeMap<&str, i64> = Default::default();
    for (_ds, tag, _j, _) in &events {
        *allc.entry(name_of(*tag)).or_insert(0) += 1;
    }
    let mut allv: Vec<(&str, i64)> = allc.into_iter().collect();
    allv.sort_by(|a, b| b.1.cmp(&a.1));
    let event_counts = json!(allv
        .iter()
        .map(|(n, c)| json!({ "name": n, "count": c }))
        .collect::<Vec<_>>());
    let insight = |name: &str, live: bool, why: &str| json!({"name": name, "status": if live {"live"} else {"gated"}, "why": why});
    let insights = json!([
        insight("Sleep stages", true, ""),
        insight(
            "Apnea / breathing",
            has("ibi_and_amplitude_event"),
            "needs overnight IBI"
        ),
        insight(
            "Cardiovascular age",
            has("cva_raw_ppg_data"),
            "enable cva_ppg"
        ),
        insight("SpO2", has("spo2_r_pi_event"), "enable spo2"),
        insight("Activity sessions", true, ""),
        insight("HRV / resting HR", true, ""),
        insight("Steps", false, "RData entitlement (firmware-locked)"),
        insight("Stress / resilience", false, "needs cloud scores"),
    ]);
    // latest battery reading (offline, from debug_data 'battery_level_changed').
    // events are sorted by ds ascending, so the last match is the most recent.
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

    // device identity (serial / firmware / mac …) + the true last-sync time
    let dev = store.device_info().ok().flatten();
    // only a real recorded BLE sync timestamp counts; falling back to anchor_unix
    // (the latest event's capture time) would make the panel look freshly synced
    // even when no sync was ever recorded. Absent → null (the UI shows "—").
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

    // one-line digest
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

fn feat(name: &str, on: bool, feature: &str) -> Value {
    json!({ "name": name, "on": on, "feature": feature })
}

/// Downsample an int array to at most `n` cells (majority per bucket).
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
            // majority in [a,b)
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
    // exact same (latest, baseline) the vitals tiles read, so the headline can't
    // disagree with them: HRV reuses the tile's % (zero-baseline → no fragment),
    // resting HR shows the bpm delta.
    let hrv_pct = vital_delta_pct(hrv);
    let rhr_delta = match (rhr.1, rhr.2) {
        (Some(l), Some(b)) => Some(l - b),
        _ => None,
    };
    if let Some(p) = hrv_pct {
        parts.push(format!("HRV {}{:.0}%", if p >= 0.0 { "+" } else { "" }, p));
    }
    if let Some(d) = rhr_delta {
        parts.push(format!(
            "resting HR {}{:.0} bpm",
            if d >= 0.0 { "+" } else { "" },
            d
        ));
    }
    // recovery reads the actual signals: rising HRV is good; if HRV is unavailable,
    // fall back to resting HR *falling* being good (never the raw sign of an HR rise).
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

// ── HTTP server ───────────────────────────────────────────────────────────────
// ── summary cache ─────────────────────────────────────────────────────────────
// build_summary spawns torch subprocesses (~seconds); without a cache every page
// load re-pays that. We memoise the last result and reuse it until the inputs
// change — the DB (a sync appends events) or profile.json (an edit changes the CVA
// inputs / weight-based kcal). Both are cheap mtime stats, so a sync or profile
// edit transparently invalidates the cache with no explicit wiring.
struct SummaryCache {
    db: PathBuf,
    tz: i64,
    token: CacheToken, // (db, profile.json, feature_modes.json) mtimes
    value: Arc<Value>,
}

type CacheToken = (Option<SystemTime>, Option<SystemTime>, Option<SystemTime>);

/// mtimes of every input the summary depends on — any change rebuilds it. Covers a
/// sync (oura.db), a profile edit (profile.json), and a feature toggle (feature_modes.json).
fn summary_token(db: &Path) -> CacheToken {
    (mtime(db), mtime(&profile_path(db)), mtime(&feature_modes_path(db)))
}

fn summary_cache() -> &'static Mutex<Option<SummaryCache>> {
    static C: OnceLock<Mutex<Option<SummaryCache>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Cached `build_summary`: recompute only when oura.db, profile.json, or
/// feature_modes.json changes.
fn cached_summary(db: &Path, tz: i64) -> Result<Arc<Value>> {
    let token = summary_token(db);
    if let Some(c) = summary_cache().lock().unwrap().as_ref() {
        if c.db == db && c.tz == tz && c.token == token {
            return Ok(c.value.clone());
        }
    }
    // build outside the lock so concurrent first-loads don't serialise on it
    let value = Arc::new(build_summary(db, tz)?);
    // re-stat the inputs: if any changed while we were building (e.g. a sync landed),
    // the result may predate that change — don't cache it, so the next request
    // rebuilds against the new state instead of serving stale data.
    let token_after = summary_token(db);
    if token_after == token {
        let mut guard = summary_cache().lock().unwrap();
        // don't clobber a fresher entry a concurrent build already stored (compare
        // the DB mtime, which a sync advances)
        let newer_exists = guard
            .as_ref()
            .is_some_and(|c| c.db == db && c.tz == tz && c.token.0 > token.0);
        if !newer_exists {
            *guard = Some(SummaryCache {
                db: db.to_path_buf(),
                tz,
                token,
                value: value.clone(),
            });
        }
    }
    Ok(value)
}

pub async fn serve(
    port: u16,
    db: PathBuf,
    tz: i64,
    name: String,
    key_file: Option<PathBuf>,
    seed: Demographics,
) -> Result<()> {
    // seed profile.json from the CLI demographics on first run; afterwards the
    // file is the editable source of truth.
    if !profile_path(&db).exists() {
        let _ = write_profile(&db, &seed.to_json());
    }
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .context("binding port")?;
    println!("open_oura dashboard running — open http://127.0.0.1:{port} in your browser");
    loop {
        let (sock, _) = listener.accept().await?;
        let (db, name, key_file) = (db.clone(), name.clone(), key_file.clone());
        tokio::spawn(async move {
            let _ = handle(sock, port, db, tz, name, key_file).await;
        });
    }
}

/// Serve a web asset: prefer the on-disk file under `dashboard/web/` (so the UI
/// can be edited and refreshed without recompiling) and fall back to the copy
/// embedded at build time.
fn asset(name: &str, embedded: &'static str) -> Vec<u8> {
    if let Some(root) = repo_root() {
        if let Ok(bytes) = std::fs::read(root.join("dashboard/web").join(name)) {
            return bytes;
        }
    }
    embedded.as_bytes().to_vec()
}

fn header<'a>(req: &'a str, name: &str) -> Option<&'a str> {
    req.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

async fn write_resp(sock: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(body).await?;
    Ok(())
}

async fn json_resp(sock: &mut TcpStream, v: &Value) -> Result<()> {
    write_resp(
        sock,
        "200 OK",
        "application/json",
        serde_json::to_vec(v)?.as_slice(),
    )
    .await
}

async fn handle(
    mut sock: TcpStream,
    port: u16,
    db: PathBuf,
    tz: i64,
    name: String,
    key_file: Option<PathBuf>,
) -> Result<()> {
    let mut buf = [0u8; 8192];
    let n = sock.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let method = req.split_whitespace().next().unwrap_or("GET");
    let target = req.split_whitespace().nth(1).unwrap_or("/");
    // route on the path only — a query string (e.g. cache-buster) must not 404 the API
    let path = target.split('?').next().unwrap_or(target);
    let body = req.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");

    // Loopback-only (DNS-rebind guard).
    let host_ok = header(&req, "host")
        .is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"));
    if !host_ok {
        return write_resp(&mut sock, "403 Forbidden", "text/plain", b"forbidden").await;
    }
    // vendored SVG icons (Phosphor), served from disk
    if path.starts_with("/icons/") && path.ends_with(".svg") && !path.contains("..") {
        if let Some(root) = repo_root() {
            if let Ok(bytes) = std::fs::read(
                root.join("dashboard/web")
                    .join(path.trim_start_matches('/')),
            ) {
                return write_resp(&mut sock, "200 OK", "image/svg+xml", &bytes).await;
            }
        }
        return write_resp(&mut sock, "404 Not Found", "text/plain", b"not found").await;
    }

    // CSRF guard for mutating/action endpoints: a same-origin fetch can set this
    // header; an <img>/<form> or cross-origin fetch cannot (CORS preflight we never approve).
    // GET /api/ring-key is guarded too — it discloses the ring auth key, so only the
    // dashboard's own same-origin page (which sets the header) may read it.
    let csrf_ok = header(&req, "x-oura-dash").is_some();
    let forbid = matches!(
        (method, path),
        ("POST", "/api/profile")
            | ("POST", "/api/sync")
            | ("POST", "/api/feature")
            | ("POST", "/api/ring-key")
            | ("GET", "/api/ring-key")
    ) && !csrf_ok;
    if forbid {
        return write_resp(&mut sock, "403 Forbidden", "text/plain", b"forbidden").await;
    }

    match (method, path) {
        (_, "/") | (_, "/index.html") => {
            write_resp(
                &mut sock,
                "200 OK",
                "text/html; charset=utf-8",
                &asset("index.html", INDEX_HTML),
            )
            .await
        }
        (_, "/styles.css") => {
            write_resp(
                &mut sock,
                "200 OK",
                "text/css; charset=utf-8",
                &asset("styles.css", STYLES_CSS),
            )
            .await
        }
        (_, "/app.js") => {
            write_resp(
                &mut sock,
                "200 OK",
                "text/javascript; charset=utf-8",
                &asset("app.js", APP_JS),
            )
            .await
        }
        ("GET", "/api/summary") => {
            // building the summary shells out to torch models → off the async
            // executor; cached so only the first load (or post-sync/edit) pays for it.
            let body = tokio::task::spawn_blocking(move || cached_summary(&db, tz))
                .await
                .map_err(|e| anyhow!(e))?;
            match body {
                Ok(v) => json_resp(&mut sock, &v).await,
                Err(e) => json_resp(&mut sock, &json!({ "error": e.to_string() })).await,
            }
        }
        ("GET", "/api/profile") => json_resp(&mut sock, &read_profile(&db).to_json()).await,
        ("POST", "/api/profile") => {
            match serde_json::from_str::<Value>(body.trim_end_matches('\0')) {
                Ok(v) => match write_profile(&db, &v) {
                    Ok(d) => json_resp(&mut sock, &d.to_json()).await,
                    Err(e) => json_resp(&mut sock, &json!({ "error": e.to_string() })).await,
                },
                Err(e) => json_resp(&mut sock, &json!({ "error": e.to_string() })).await,
            }
        }
        ("GET", "/api/ring-key") => match read_ring_key(key_file.as_deref()) {
            Ok(key) => json_resp(&mut sock, &json!({ "ok": true, "key": key })).await,
            Err(e) => json_resp(&mut sock, &json!({ "ok": false, "message": e.to_string() })).await,
        },
        ("POST", "/api/ring-key") => {
            let req =
                serde_json::from_str::<Value>(body.trim_end_matches('\0')).unwrap_or(Value::Null);
            match write_ring_key(key_file.as_deref(), req["key"].as_str().unwrap_or("")) {
                Ok(_) => json_resp(&mut sock, &json!({ "ok": true })).await,
                Err(e) => {
                    json_resp(&mut sock, &json!({ "ok": false, "message": e.to_string() })).await
                }
            }
        }
        ("POST", "/api/sync") => {
            let res =
                tokio::task::spawn_blocking(move || run_sync(&db, &name, key_file.as_deref()))
                    .await
                    .map_err(|e| anyhow!(e))?;
            let v = match res {
                Ok(msg) => json!({ "ok": true, "message": msg }),
                Err(e) => json!({ "ok": false, "message": e.to_string() }),
            };
            json_resp(&mut sock, &v).await
        }
        ("POST", "/api/feature") => {
            let req =
                serde_json::from_str::<Value>(body.trim_end_matches('\0')).unwrap_or(Value::Null);
            let feature = req["feature"].as_str().unwrap_or("").to_string();
            let mode = req["mode"].as_str().unwrap_or("").to_string();
            let res = tokio::task::spawn_blocking(move || {
                run_feature(&db, &name, key_file.as_deref(), &feature, &mode)
            })
            .await
            .map_err(|e| anyhow!(e))?;
            let v = match res {
                Ok(msg) => json!({ "ok": true, "message": msg }),
                Err(e) => json!({ "ok": false, "message": e.to_string() }),
            };
            json_resp(&mut sock, &v).await
        }
        _ => write_resp(&mut sock, "404 Not Found", "text/plain", b"not found").await,
    }
}

/// Drain the ring by invoking our own binary's `sync` subcommand (reuses all the
/// BLE + cursor logic). Returns the last stdout line on success.
///
/// BLE scanning on macOS is flaky: the ring advertises only periodically (and not at
/// all for a few seconds after a disconnect), so a single short scan often misses a
/// ring that is right there. We use a longer scan window and retry the transient
/// "no matching ring" miss a couple of times before giving up.
fn run_sync(db: &Path, name: &str, key_file: Option<&Path>) -> Result<String> {
    let exe = std::env::current_exe().context("locating oura binary")?;
    let mut last = String::from("sync failed");
    for attempt in 0..3 {
        let mut c = Command::new(&exe);
        c.arg("--db")
            .arg(db)
            .arg("--name")
            .arg(name)
            .arg("--scan-timeout")
            .arg("40"); // global flag; wider than the 25 s default
        if let Some(k) = key_file {
            c.arg("--key-file").arg(k);
        }
        c.arg("sync");
        let out = c.output().context("running `oura sync`")?;
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            return Ok(stdout.lines().last().unwrap_or("synced").trim().to_string());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        last = stderr
            .lines()
            .last()
            .unwrap_or("sync failed")
            .trim()
            .to_string();
        // only a scan miss is worth retrying; a real error (auth, etc.) is not
        let transient = {
            let l = last.to_lowercase();
            l.contains("no matching")
                || l.contains("not found")
                || l.contains("no device")
                || l.contains("timed out")
                || l.contains("timeout")
        };
        if !transient || attempt == 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Err(anyhow!("{last}"))
}

/// Toggle an on-ring feature via our `feature-mode` subcommand (BLE, auth-gated).
/// `feature` is whitelisted; `mode` is off|automatic. Writing a feature mode REQUIRES
/// the auth key, so we fail early (with a clear message) if the dashboard was launched
/// without `--key-file`. Scan misses retry; the ring's "already in that mode" rejection
/// (result `0x20`) is reported as a no-op success rather than an error.
fn run_feature(
    db: &Path,
    name: &str,
    key_file: Option<&Path>,
    feature: &str,
    mode: &str,
) -> Result<String> {
    const FEATS: &[&str] = &[
        "daytime_hr",
        "spo2",
        "exercise_hr",
        "real_steps",
        "cva_ppg",
        "ambient",
        "resting_hr",
    ];
    if !FEATS.contains(&feature) {
        return Err(anyhow!("unknown feature"));
    }
    if mode != "off" && mode != "automatic" {
        return Err(anyhow!("invalid mode"));
    }
    let key_file = key_file.ok_or_else(|| {
        anyhow!("can't change ring features: the dashboard was started without --key-file (auth key needed to write feature modes)")
    })?;
    let exe = std::env::current_exe().context("locating oura binary")?;
    let on = if mode == "off" { "off" } else { "on" };
    let mut last = String::from("toggle failed");
    for attempt in 0..3 {
        let mut c = Command::new(&exe);
        c.arg("--db")
            .arg(db)
            .arg("--name")
            .arg(name)
            .arg("--scan-timeout")
            .arg("40")
            .arg("--key-file")
            .arg(key_file)
            .arg("feature-mode")
            .arg(feature)
            .arg("--mode")
            .arg(mode);
        let out = c.output().context("running `oura feature-mode`")?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        // 0 = off, 1 = automatic — keep feature_modes.json in step with the toggle so
        // the dashboard shows the new state on the next reload, not only after a sync.
        let mode_int = if mode == "off" { 0 } else { 1 };
        if stdout.contains("SUCCESS") {
            write_feature_mode(db, feature, mode_int);
            return Ok(format!("{feature} turned {on}"));
        }
        if stdout.contains("result 0x20") {
            // ring refuses to set a mode it's already in → it's already in that state
            write_feature_mode(db, feature, mode_int);
            return Ok(format!("{feature} is already {on} (no change needed)"));
        }
        last = stdout
            .lines()
            .chain(stderr.lines())
            .filter(|l| !l.trim().is_empty())
            .last()
            .unwrap_or("toggle failed")
            .trim()
            .to_string();
        let transient = {
            let l = last.to_lowercase();
            l.contains("no matching")
                || l.contains("not found")
                || l.contains("no device")
                || l.contains("timed out")
                || l.contains("timeout")
        };
        if !transient || attempt == 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Err(anyhow!("{last}"))
}
