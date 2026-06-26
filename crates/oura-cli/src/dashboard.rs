//! `oura dashboard` — a local web health dashboard following
//! `notes/dashboard-v2-brainstorm.md`.
//!
//! Rust owns the DB + every non-model calculation (per-night HRV/RHR/skin-temp,
//! SpO2 % via Oura's calibration, device/data-health, baselines + deltas, the
//! digest) and the HTTP server. The torch models (sleep hypnogram, activity, CVA)
//! run through the Python runners, shelled out exactly like `oura sessions`. All
//! data stays on this machine.

use std::path::{Path, PathBuf};
use std::process::Command;
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
/// signal-derived. Passed from the CLI.
#[derive(Clone, Copy)]
pub struct Demographics {
    pub sex: char, // 'M' | 'F' | 'O'
    pub age: f64,
    pub height_m: f64,
    pub weight_kg: f64,
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
    let find = |start: &Path| start.ancestors().find(|d| d.join(marker).is_file()).map(Path::to_path_buf);
    std::env::current_dir().ok().and_then(|d| find(&d)).or_else(|| find(Path::new(env!("CARGO_MANIFEST_DIR"))))
}

fn python_bin(root: &Path) -> PathBuf {
    let venv = root.join(".venv/bin/python");
    if venv.is_file() { venv } else { PathBuf::from("python3") }
}

/// Run a python runner and parse its `--json` stdout. Returns None on any failure
/// (missing venv/model, night with no data, …) so the dashboard degrades softly.
fn run_py_json(root: &Path, py: &Path, script: &str, args: &[String]) -> Option<Value> {
    let out = Command::new(py).current_dir(root).arg(root.join(script)).args(args).output().ok()?;
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

/// Assemble the full dashboard summary as a JSON value.
pub fn build_summary(db: &Path, tz: i64, demo: Demographics) -> Result<Value> {
    let store = Store::open(db).context("opening DB")?;
    let events = store.decoded_events().context("reading events")?;
    if events.is_empty() {
        return Err(anyhow!("no decoded events in {} — run `oura sync` first", db.display()));
    }
    let (max_ds, anchor_unix) = events.iter().map(|(ds, _, _, cu)| (*ds, *cu)).max_by_key(|(ds, _)| *ds).unwrap();
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
                if let (Some(s), Some(e)) = (v["bedtime_start_ds"].as_i64(), v["bedtime_end_ds"].as_i64()) {
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
    let mut nights: Vec<Night> = beds.iter().map(|&(s, e)| Night { start_ds: s, end_ds: e, ..Default::default() }).collect();
    let find_night = |ds: i64, nights: &[Night]| nights.iter().position(|nt| nt.start_ds - 600 <= ds && ds <= nt.end_ds + 600);
    for (ds, tag, jstr, _) in &events {
        let Some(idx) = find_night(*ds, &nights) else { continue };
        let n = name_of(*tag);
        let v: Value = match serde_json::from_str(jstr) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match n {
            "hrv_event" => {
                if let Some(a) = v["rmssd_ms"].as_array() {
                    nights[idx].rmssd.extend(a.iter().filter_map(|x| x.as_f64()).filter(|&x| x > 0.0));
                }
                if let Some(a) = v["hr_bpm"].as_array() {
                    nights[idx].hr.extend(a.iter().filter_map(|x| x.as_f64()).filter(|&x| x > 0.0));
                }
            }
            "temp_event" | "sleep_temp_event" => {
                if let Some(c) = v["temps_c"].as_array().and_then(|a| a.first()).and_then(|x| x.as_f64()) {
                    nights[idx].temp.push(c);
                }
            }
            "spo2_r_pi_event" => {
                if let Some(a) = v["r"].as_array() {
                    nights[idx].spo2.extend(a.iter().filter_map(|x| x.as_f64()).filter(|&x| x > 0.0).map(spo2_pct));
                }
            }
            _ => {}
        }
    }

    // shell out to the models
    let root = repo_root();
    let py = root.as_deref().map(python_bin);
    let model_json = |script: &str, args: Vec<String>| -> Option<Value> {
        let (r, p) = (root.as_deref()?, py.as_deref()?);
        run_py_json(r, p, script, &args)
    };

    // nights JSON (newest first), each enriched with the hypnogram from the model
    let mut nights_json = Vec::new();
    for nt in &nights {
        let hyp = model_json(
            "tools/run_sleep_model.py",
            vec![nt.start_ds.to_string(), nt.end_ds.to_string(), db.display().to_string(), "--json".into()],
        );
        // downsample the per-30s stage array to ~120 cells for the bar
        let stage_cells = hyp.as_ref().and_then(|h| h["stages"].as_array()).map(|s| downsample(s, 120));
        nights_json.push(json!({
            "date": date_label(unix_s(nt.start_ds), tz),
            "start": hm(unix_s(nt.start_ds), tz),
            "end": hm(unix_s(nt.end_ds), tz),
            "in_bed_h": ((nt.end_ds - nt.start_ds) as f64 / 10.0 / 3600.0 * 10.0).round() / 10.0,
            "hrv_ms": mean(&nt.rmssd).map(|x| x.round()),
            "rhr": nt.hr.iter().cloned().fold(f64::INFINITY, f64::min).is_finite().then(|| nt.hr.iter().cloned().fold(f64::INFINITY, f64::min).round()),
            "skin_temp": mean(&nt.temp).map(|x| (x * 10.0).round() / 10.0),
            "spo2_mean": mean(&nt.spo2).map(|x| x.round()),
            "deep_pct": hyp.as_ref().map(|h| h["deep_pct"].clone()),
            "light_pct": hyp.as_ref().map(|h| h["light_pct"].clone()),
            "rem_pct": hyp.as_ref().map(|h| h["rem_pct"].clone()),
            "wake_pct": hyp.as_ref().map(|h| h["wake_pct"].clone()),
            "efficiency": hyp.as_ref().map(|h| h["efficiency_pct"].clone()),
            "stages": stage_cells,
        }));
    }
    nights_json.reverse();

    // cardio (CVA)
    let cva = model_json(
        "tools/run_cva_model.py",
        vec![
            db.display().to_string(),
            "--json".into(),
            "--sex".into(), demo.sex.to_string(),
            "--age".into(), demo.age.to_string(),
            "--height".into(), demo.height_m.to_string(),
            "--weight".into(), demo.weight_kg.to_string(),
        ],
    );

    // activity
    let activity = model_json(
        "tools/run_activity_model.py",
        vec![db.display().to_string(), "--tz".into(), tz.to_string(), "--json".into()],
    )
    .and_then(|v| v["sessions"].as_array().cloned())
    .unwrap_or_default();

    // vitals trend (from nights, oldest→newest)
    let hrv_series: Vec<f64> = nights.iter().filter_map(|n| mean(&n.rmssd)).collect();
    let rhr_series: Vec<f64> = nights.iter().filter_map(|n| n.hr.iter().cloned().reduce(f64::min)).collect();
    let trend = |s: &[f64]| -> Value {
        if s.len() < 2 {
            return json!({ "series": s, "latest": s.last(), "baseline": s.last(), "delta_pct": 0 });
        }
        let latest = *s.last().unwrap();
        let base = s[..s.len() - 1].iter().sum::<f64>() / (s.len() - 1) as f64;
        json!({
            "series": s.iter().map(|x| x.round()).collect::<Vec<_>>(),
            "latest": latest.round(),
            "baseline": (base * 10.0).round() / 10.0,
            "delta_pct": ((latest - base) / base * 100.0).round(),
        })
    };

    // device & data-health
    let has = |evname: &str| present_recent.contains(evname);
    let measuring = json!([
        feat("Daytime HR", has("ibi_and_amplitude_event") || has("green_ibi_quality_event")),
        feat("SpO2", has("spo2_r_pi_event")),
        feat("Exercise HR", has("ehr_trace_event")),
        feat("Real steps", has("real_step_event_feature_2")),
        feat("Cardio PPG (CVA)", has("cva_raw_ppg_data")),
    ]);
    let insight = |name: &str, live: bool, why: &str| json!({"name": name, "status": if live {"live"} else {"gated"}, "why": why});
    let insights = json!([
        insight("Sleep stages", true, ""),
        insight("Apnea / breathing", has("ibi_and_amplitude_event"), "needs overnight IBI"),
        insight("Cardiovascular age", has("cva_raw_ppg_data"), "enable cva_ppg"),
        insight("SpO2", has("spo2_r_pi_event"), "enable spo2"),
        insight("Activity sessions", true, ""),
        insight("HRV / resting HR", true, ""),
        insight("Steps", false, "RData entitlement (firmware-locked)"),
        insight("Stress / resilience", false, "needs cloud scores"),
    ]);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as f64).unwrap_or(anchor_unix as f64);
    let device = json!({
        "synced": date_label(anchor_unix as f64, tz),
        "synced_hm": hm(anchor_unix as f64, tz),
        "fresh_hours": ((now - anchor_unix as f64) / 3600.0 * 10.0).round() / 10.0,
        "days_of_data": ((max_ds - min_ds) as f64 / 10.0 / 86400.0 * 10.0).round() / 10.0,
        "total_events": events.len(),
        "nights": nights.len(),
        "measuring": measuring,
        "insights": insights,
    });

    // one-line digest
    let digest = make_digest(&hrv_series, &rhr_series, &nights, &cva);

    Ok(json!({
        "generated_at": now,
        "tz": tz,
        "digest": digest,
        "device": device,
        "nights": nights_json,
        "cardio": cva,
        "activity": activity,
        "vitals": { "hrv": trend(&hrv_series), "rhr": trend(&rhr_series) },
    }))
}

fn feat(name: &str, on: bool) -> Value {
    json!({ "name": name, "on": on })
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

fn make_digest(hrv: &[f64], rhr: &[f64], nights: &[Night], cva: &Option<Value>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if hrv.len() >= 2 {
        let d = hrv.last().unwrap() - hrv[..hrv.len() - 1].iter().sum::<f64>() / (hrv.len() - 1) as f64;
        parts.push(format!("HRV {}{:.0}%", if d >= 0.0 { "+" } else { "" }, d / hrv[0].max(1.0) * 100.0));
    }
    if rhr.len() >= 2 {
        let d = rhr.last().unwrap() - rhr[..rhr.len() - 1].iter().sum::<f64>() / (rhr.len() - 1) as f64;
        parts.push(format!("resting HR {}{:.0} bpm", if d >= 0.0 { "+" } else { "" }, d));
    }
    let recovering = parts.first().map(|p| p.contains('+')).unwrap_or(false);
    let mut s = parts.join(", ");
    if !s.is_empty() {
        s.push_str(if recovering { ". Recovering well." } else { ". Recovery dipping, take it easy." });
    }
    let _ = (nights, cva);
    if s.is_empty() {
        s = "Synced. Not enough history yet for trends.".into();
    }
    s
}

// ── HTTP server ───────────────────────────────────────────────────────────────
pub async fn serve(port: u16, db: PathBuf, tz: i64, demo: Demographics) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await.context("binding port")?;
    println!("open_oura dashboard → http://127.0.0.1:{port}");
    loop {
        let (sock, _) = listener.accept().await?;
        let db = db.clone();
        tokio::spawn(async move {
            let _ = handle(sock, port, db, tz, demo).await;
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

async fn handle(mut sock: TcpStream, port: u16, db: PathBuf, tz: i64, demo: Demographics) -> Result<()> {
    let mut buf = [0u8; 2048];
    let n = sock.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req.split_whitespace().nth(1).unwrap_or("/");

    // Loopback-only: reject anything not addressed to localhost (DNS-rebind guard).
    let host_ok = header(&req, "host")
        .is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"));
    if !host_ok {
        return write_resp(&mut sock, "403 Forbidden", "text/plain", b"forbidden").await;
    }

    match path {
        "/" | "/index.html" => write_resp(&mut sock, "200 OK", "text/html; charset=utf-8", &asset("index.html", INDEX_HTML)).await,
        "/styles.css" => write_resp(&mut sock, "200 OK", "text/css; charset=utf-8", &asset("styles.css", STYLES_CSS)).await,
        "/app.js" => write_resp(&mut sock, "200 OK", "text/javascript; charset=utf-8", &asset("app.js", APP_JS)).await,
        "/api/summary" => {
            // building the summary shells out to torch models → run off the async
            // executor so we don't stall other connections.
            let body = tokio::task::spawn_blocking(move || build_summary(&db, tz, demo))
                .await
                .map_err(|e| anyhow!(e))?;
            match body {
                Ok(v) => write_resp(&mut sock, "200 OK", "application/json", serde_json::to_vec(&v)?.as_slice()).await,
                Err(e) => {
                    let err = json!({ "error": e.to_string() });
                    write_resp(&mut sock, "200 OK", "application/json", serde_json::to_vec(&err)?.as_slice()).await
                }
            }
        }
        _ => write_resp(&mut sock, "404 Not Found", "text/plain", b"not found").await,
    }
}
