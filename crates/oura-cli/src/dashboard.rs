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
use std::time::SystemTime;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use oura_summary::{feature_modes_path, profile_path, ModelInputs, ModelOutputs, ModelRunner};
// Re-exported so `dashboard::Demographics` (main.rs) and the profile/feature
// handlers keep working after the summary logic moved into `oura-summary`.
pub use oura_summary::{read_profile, write_feature_mode, write_profile, Demographics};

const INDEX_HTML: &str = include_str!("../../../dashboard/web/index.html");
const STYLES_CSS: &str = include_str!("../../../dashboard/web/styles.css");
const APP_JS: &str = include_str!("../../../dashboard/web/app.js");

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



/// The web dashboard's [`ModelRunner`]: shells out to the Python torch runners,
/// exactly as before. The native client supplies an on-device `.ptl` runner.
struct PythonRunner;
impl ModelRunner for PythonRunner {
    fn run(&self, input: ModelInputs) -> ModelOutputs {
        let ModelInputs { db, tz, demo, sleep_ranges } = input;
        let root = repo_root();
        let py = root.as_deref().map(python_bin);
        let sleep_stdin = serde_json::to_vec(sleep_ranges).unwrap_or_default();
        let sleep_args =
            vec![db.display().to_string(), tz.to_string(), "--json".into(), "--batch".into()];
        let cva_args = vec![
            db.display().to_string(), "--json".into(),
            "--sex".into(), demo.sex.to_string(),
            "--age".into(), demo.age.to_string(),
            "--height".into(), demo.height_m.to_string(),
            "--weight".into(), demo.weight_kg.to_string(),
            "--ring".into(), demo.ring_size.to_string(),
        ];
        let act_args =
            vec![db.display().to_string(), "--tz".into(), tz.to_string(), "--json".into()];

        let (sleep_batch, cva, activity) = match (root.as_deref(), py.as_deref()) {
            (Some(r), Some(p)) => std::thread::scope(|s| {
                let sh = s.spawn(|| {
                    run_py_json_stdin(r, p, "tools/run_sleep_model.py", &sleep_args, &sleep_stdin)
                });
                let ch = s.spawn(|| run_py_json(r, p, "tools/run_cva_model.py", &cva_args));
                let ah = s.spawn(|| run_py_json(r, p, "tools/run_activity_model.py", &act_args));
                (sh.join().ok().flatten(), ch.join().ok().flatten(), ah.join().ok().flatten())
            }),
            _ => (None, None, None),
        };
        ModelOutputs { sleep_batch, cva, activity }
    }
}

/// `build_summary` for the web dashboard — runs the models via Python.
fn build_summary(db: &Path, tz: i64) -> Result<Value> {
    oura_summary::build_summary(db, tz, &PythonRunner)
}

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
