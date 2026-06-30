//! `oura` — a command-line client that reads data directly from an Oura ring over
//! BLE, with no Oura cloud account. See `--help` for subcommands.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};

use oura_link::ble::{self, BleTransport};
use oura_store::storage::Store;
use oura_link::OuraClient;

#[cfg(feature = "torch")]
mod activity_model;
mod dashboard;
mod game;
mod motion_server;
mod viz;

/// Read sleep/HR/activity signals straight from an Oura ring (Ring 3/4/5).
#[derive(Parser, Debug)]
#[command(name = "oura", version, about)]
struct Cli {
    /// Case-insensitive device-name substring to match while scanning.
    #[arg(long, global = true, default_value = "Oura")]
    name: String,

    /// Connect only to a device whose platform id matches exactly.
    #[arg(long, global = true)]
    address: Option<String>,

    /// Seconds to scan before giving up.
    #[arg(long, global = true, default_value_t = 25)]
    scan_timeout: u64,

    /// SQLite database path.
    #[arg(long, global = true, default_value = "oura.db")]
    db: PathBuf,

    /// 16-byte app-auth key as hex (file contents). Required for auth-gated ops.
    #[arg(long, global = true)]
    key_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// List nearby Oura rings.
    Scan,
    /// Pair with a factory-reset ring: install a fresh 16-byte auth key and save
    /// it to `--key-file` (or `oura-<serial>.key`). If the key file already
    /// exists, that key is re-installed instead of generating a new one.
    Pair,
    /// Connect and print device info (firmware, serial, battery, capabilities).
    Info,
    /// Drain history events into the database (incremental).
    Sync {
        /// Also align the ring clock to host UTC before syncing.
        #[arg(long)]
        sync_time: bool,
    },
    /// Read the ring's latest cached HR / SpO2 values.
    Latest,
    /// Stream live heart rate for a number of seconds (ring must be worn).
    LiveHr {
        #[arg(long, default_value_t = 30)]
        seconds: u64,
        /// Also print every raw notification frame (for diagnosing the stream).
        #[arg(long)]
        raw: bool,
    },
    /// Show stored event counts from the database (offline).
    Events,
    /// Re-run decoders over already-stored raw event bodies (offline).
    Redecode,
    /// RData bulk raw-sampler control: `state` (read), `stop`/`clear` (teardown),
    /// `probe` (arm ACM-50 ~30 s, measure data rate + battery, auto-teardown).
    /// `probe` writes a persistent flash session but always tears itself down
    /// (see docs/rdata-capacity-probe.md).
    Rdata {
        /// One of: state | stop | clear | probe
        #[arg(default_value = "state")]
        action: String,
    },
    /// Stream live accelerometer (ACM) — wave your hand to see motion.
    Accel {
        #[arg(long, default_value_t = 15)]
        seconds: u64,
    },
    /// Real-time 3D motion visualizer (web UI with start/stop + sensitivity).
    Viz {
        /// Local HTTP port to serve the visualizer on.
        #[arg(long, default_value_t = 8088)]
        port: u16,
        /// Minutes the ring streams per "Start" (it auto-stops after this).
        #[arg(long, default_value_t = 5)]
        minutes: u16,
    },
    /// Tilt-controlled asteroid game (web UI) — steer a ship by tilting the ring.
    Game {
        /// Local HTTP port to serve the game on.
        #[arg(long, default_value_t = 8089)]
        port: u16,
        /// Minutes the ring streams per "Start" (it auto-stops after this).
        #[arg(long, default_value_t = 10)]
        minutes: u16,
    },
    /// Ask the ring to run sleep analysis (so it emits hypnogram/summary events).
    SleepAnalyze {
        #[arg(long)]
        force: bool,
    },
    /// Show feature status (HR, SpO2…) and optionally enable measurement.
    Features {
        /// Enable daytime-HR measurement (mode automatic).
        #[arg(long)]
        enable_hr: bool,
        /// Enable SpO2 measurement (mode automatic).
        #[arg(long)]
        enable_spo2: bool,
    },
    /// Detect & label activity sessions from stored events using Oura's OWN
    /// `automatic_activity_detection` model (runs via tools/run_activity_model.py;
    /// needs the Python venv with torch). Not a heuristic.
    Sessions {
        /// Timezone offset (hours from UTC) for displayed times.
        #[arg(long, default_value_t = 0)]
        tz_offset: i64,
        /// is_workout probability at/above which a segment is marked a workout.
        #[arg(long, default_value_t = 0.5)]
        threshold: f64,
        /// Emit machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Compute a Sleep Score for the latest night, live from ring data: Oura's
    /// SleepNet hypnogram → durations/efficiency/latency, bedtime regularity, then
    /// the contributor curves + combiner weights calibrated from a trends export.
    /// Runs via tools/score_sleep.py (needs the Python venv with torch).
    SleepScore {
        /// Timezone offset (hours from UTC) for the bedtime clock.
        #[arg(long, default_value_t = 0)]
        tz_offset: i64,
        /// Trends CSV for calibration (default: auto-find ~/Desktop/oura_*trends.csv).
        #[arg(long)]
        csv: Option<PathBuf>,
        /// Emit machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Subscribe a feature capability (real_steps | atlas | ambient | raw_data |
    /// research_data) via SetFeatureSubscription, to make the ring emit its events.
    Subscribe {
        /// Capability to subscribe.
        #[arg(value_parser = ["real_steps", "atlas", "ambient", "raw_data", "research_data"])]
        feature: String,
        /// Subscription mode: off | state | latest | data (default: data).
        #[arg(long, default_value = "data")]
        mode: String,
    },
    /// Set a feature's operating MODE via SetFeatureMode (e.g. turn on real_steps).
    /// This is the consumer-feature enable path (distinct from `subscribe`).
    FeatureMode {
        /// Feature: real_steps | exercise_hr | resting_hr | cva_ppg | ambient,
        /// or a raw id like 0x0b.
        feature: String,
        /// Mode: off | automatic | requested | connected_live (default: automatic).
        #[arg(long, default_value = "automatic")]
        mode: String,
    },
    /// Read the real on-ring MODE/status of the data features (what's actually on).
    FeatureStatus,
    /// Serve a local web health dashboard (sleep, cardio, SpO2, activity, device)
    /// at http://127.0.0.1:PORT. Rust computes from oura.db; torch models run via
    /// the Python runners. All data stays on this machine.
    Dashboard {
        /// Local HTTP port.
        #[arg(long, default_value_t = 8090)]
        port: u16,
        /// Timezone offset (hours from UTC) for displayed times.
        #[arg(long, default_value_t = 0)]
        tz_offset: i64,
        /// Sex for the cardiovascular-age model: M | F | O.
        #[arg(long, default_value = "M")]
        sex: String,
        /// Age (years) for the CVA model.
        #[arg(long, default_value_t = 30.0)]
        age: f64,
        /// Height (meters) for the CVA model.
        #[arg(long, default_value_t = 1.78)]
        height: f64,
        /// Weight (kg) for the CVA model.
        #[arg(long, default_value_t = 75.0)]
        weight: f64,
    },
}

fn feature_mode_name(mode: u8) -> &'static str {
    match mode {
        0 => "off",
        1 => "automatic",
        2 => "requested",
        3 => "connected_live",
        _ => "?",
    }
}

fn feature_state_name(state: u8) -> &'static str {
    match state {
        0 => "idle",
        1 => "scanning",
        2 => "measuring",
        3 => "postprocessing",
        _ => "?",
    }
}

fn load_key(path: &Option<PathBuf>) -> Result<Option<[u8; 16]>> {
    let Some(path) = path else { return Ok(None) };
    // A missing file is not an error: `pair` writes it, others treat it as "no key".
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading key file {}", path.display()))?;
    let bytes = hex::decode(text.trim()).context("key file is not valid hex")?;
    let key: [u8; 16] = bytes
        .try_into()
        .map_err(|_| anyhow!("auth key must be exactly 16 bytes"))?;
    Ok(Some(key))
}

/// Generate a random 16-byte auth key from the OS CSPRNG.
fn generate_key() -> Result<[u8; 16]> {
    let mut file = std::fs::File::open("/dev/urandom").context("opening /dev/urandom")?;
    let mut key = [0u8; 16];
    file.read_exact(&mut key).context("reading random bytes")?;
    Ok(key)
}

/// Persist a key as hex with owner-only permissions.
fn save_key(path: &Path, key: &[u8; 16]) -> Result<()> {
    std::fs::write(path, format!("{}\n", hex::encode(key)))
        .with_context(|| format!("writing key file {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

async fn connect(cli: &Cli) -> Result<OuraClient<BleTransport>> {
    let transport = BleTransport::connect(
        &cli.name,
        cli.address.as_deref(),
        Duration::from_secs(cli.scan_timeout),
    )
    .await
    .context("connecting to ring")?;
    Ok(OuraClient::new(transport))
}

/// Authenticate if a key was supplied; returns whether auth was performed.
async fn maybe_auth(client: &OuraClient<BleTransport>, key: &Option<[u8; 16]>) -> Result<bool> {
    if let Some(key) = key {
        client.authenticate(key).await.context("authenticating")?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // btleplug logs a benign dispatch error after we disconnect.
                .unwrap_or_else(|_| "warn,oura=info,btleplug=off".into()),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let key = load_key(&cli.key_file)?;

    match &cli.command {
        Command::Scan => cmd_scan(&cli).await,
        Command::Pair => cmd_pair(&cli).await,
        Command::Info => cmd_info(&cli, &key).await,
        Command::Sync { sync_time } => cmd_sync(&cli, &key, *sync_time).await,
        Command::Latest => cmd_latest(&cli, &key).await,
        Command::LiveHr { seconds, raw } => cmd_live_hr(&cli, &key, *seconds, *raw).await,
        Command::Accel { seconds } => cmd_accel(&cli, &key, *seconds).await,
        Command::SleepAnalyze { force } => cmd_sleep_analyze(&cli, &key, *force).await,
        Command::Viz { port, minutes } => {
            let client = connect(&cli).await?;
            maybe_auth(&client, &key).await?;
            viz::run(client, *port, *minutes).await
        }
        Command::Game { port, minutes } => {
            let client = connect(&cli).await?;
            maybe_auth(&client, &key).await?;
            game::run(client, *port, *minutes).await
        }
        Command::Rdata { action } => cmd_rdata(&cli, &key, action).await,
        Command::Events => cmd_events(&cli).await,
        Command::Redecode => {
            let store = Store::open(&cli.db)?;
            let (decoded, total) = store.redecode()?;
            println!("Re-decoded {decoded}/{total} stored events.");
            Ok(())
        }
        Command::Features {
            enable_hr,
            enable_spo2,
        } => cmd_features(&cli, &key, *enable_hr, *enable_spo2).await,
        Command::Sessions { tz_offset, threshold, json } => {
            cmd_sessions(&cli, *tz_offset, *threshold, *json)
        }
        Command::SleepScore { tz_offset, csv, json } => {
            cmd_sleep_score(&cli, *tz_offset, csv.clone(), *json)
        }
        Command::Subscribe { feature, mode } => cmd_subscribe(&cli, &key, feature, mode).await,
        Command::FeatureMode { feature, mode } => cmd_feature_mode(&cli, &key, feature, mode).await,
        Command::FeatureStatus => cmd_feature_status(&cli, &key).await,
        Command::Dashboard { port, tz_offset, sex, age, height, weight } => {
            let seed = dashboard::Demographics {
                sex: sex.chars().next().unwrap_or('M').to_ascii_uppercase(),
                age: *age,
                height_m: *height,
                weight_kg: *weight,
                ring_size: 10.0,
            };
            dashboard::serve(*port, cli.db.clone(), *tz_offset, cli.name.clone(), cli.key_file.clone(), seed).await
        }
    }
}

/// Subscribe a feature capability via SetFeatureSubscription (needs auth).
async fn cmd_subscribe(
    cli: &Cli,
    key: &Option<[u8; 16]>,
    feature: &str,
    mode: &str,
) -> Result<()> {
    use oura_protocol::protocol::{capability, subscription_mode};
    let cap = match feature {
        "real_steps" => capability::REAL_STEPS,
        "atlas" => capability::ATLAS,
        "ambient" => capability::AMBIENT_LIGHT,
        "raw_data" => capability::RAW_DATA_SAMPLER,
        "research_data" => capability::RESEARCH_DATA,
        other => return Err(anyhow!("unknown feature {other}")),
    };
    let m = match mode {
        "off" => subscription_mode::OFF,
        "state" => subscription_mode::STATE,
        "latest" => subscription_mode::LATEST,
        "data" => subscription_mode::FEATURE_DATA,
        other => return Err(anyhow!("unknown mode {other}")),
    };
    let client = connect(cli).await?;
    if !maybe_auth(&client, key).await? {
        return Err(anyhow!("subscription requires --key-file (authentication)"));
    }
    let result = client
        .set_feature_subscription(cap, m)
        .await
        .context("set_feature_subscription")?;
    let name = match result {
        0 => "SUCCESS",
        1 => "NOT_SUPPORTED (firmware lacks this feature)",
        2 => "NOT_AVAILABLE (supported, but a precondition isn't met)",
        3 => "NOT_IN_FINGER (wear the ring)",
        4 => "MESSAGE_TOO_SHORT",
        5 => "LOW_BATTERY",
        _ => "unknown",
    };
    if result == 0 {
        println!("Subscribed {feature} (mode {mode}): SUCCESS.");
        println!("Wear the ring; the feature's events will appear on the next sync.");
    } else {
        println!("Ring rejected {feature} (mode {mode}): {result:#04x} = {name}.");
    }
    Ok(())
}

/// Set a feature's operating mode (SetFeatureMode). The consumer-feature enable
/// path — e.g. `feature-mode real_steps` turns on the on-ring step/gait DSP whose
/// `real_steps_features` events (0x7e/0x7f) feed the steps_motion_decoder.
async fn cmd_feature_mode(cli: &Cli, key: &Option<[u8; 16]>, feature: &str, mode: &str) -> Result<()> {
    use oura_protocol::protocol::feature_mode;
    let id: u8 = match feature {
        "real_steps" => 0x0b,
        "daytime_hr" => 0x02,
        "exercise_hr" => 0x03,
        "spo2" => 0x04,
        "resting_hr" => 0x08,
        "cva_ppg" => 0x0d,
        "ambient" => 0x10,
        other => other
            .strip_prefix("0x")
            .and_then(|h| u8::from_str_radix(h, 16).ok())
            .or_else(|| other.parse().ok())
            .ok_or_else(|| anyhow!("unknown feature {other} (use a name or 0xNN)"))?,
    };
    let m = match mode {
        "off" => feature_mode::OFF,
        "automatic" => feature_mode::AUTOMATIC,
        "requested" => feature_mode::REQUESTED,
        "connected_live" => feature_mode::CONNECTED_LIVE,
        other => return Err(anyhow!("unknown mode {other}")),
    };
    let client = connect(cli).await?;
    if !maybe_auth(&client, key).await? {
        return Err(anyhow!("set-feature-mode requires --key-file (authentication)"));
    }
    match client.set_feature_mode(id, m).await {
        Ok(()) => {
            println!("SetFeatureMode({feature}=0x{id:02x}, {mode}): SUCCESS.");
            println!("Wear the ring; the feature's events should appear on the next sync.");
        }
        Err(e) => println!("SetFeatureMode({feature}=0x{id:02x}, {mode}) rejected: {e}"),
    }
    Ok(())
}

/// Read the actual on-ring mode/status of the data-producing features.
async fn cmd_feature_status(cli: &Cli, key: &Option<[u8; 16]>) -> Result<()> {
    let client = connect(cli).await?;
    if !maybe_auth(&client, key).await? {
        return Err(anyhow!("feature-status requires --key-file (authentication)"));
    }
    let mode_name = |m: u8| match m {
        0 => "OFF",
        1 => "AUTOMATIC",
        2 => "REQUESTED",
        3 => "CONNECTED_LIVE",
        _ => "?",
    };
    let feats = [
        (0x02u8, "daytime_hr"), (0x03, "exercise_hr"), (0x04, "spo2"),
        (0x08, "resting_hr"), (0x0b, "real_steps"), (0x0c, "experimental"),
        (0x0d, "cva_ppg"),
    ];
    println!("  {:<14} {:>3}  {:<14} status state sub", "feature", "id", "mode");
    for (id, name) in feats {
        match client.feature_status(id).await {
            Ok(s) => println!(
                "  {name:<14} {id:>3}  {:<14} {:>6} {:>5} {:>3}",
                mode_name(s.mode), s.status, s.state, s.subscription
            ),
            Err(e) => println!("  {name:<14} {id:>3}  <read failed: {e}>"),
        }
    }
    Ok(())
}

/// Label activity sessions by running Oura's own `automatic_activity_detection`
/// TorchScript model over the stored events. Built with `--features torch` it
/// runs the model in-process via LibTorch; otherwise it shells out to the
/// equivalent `tools/run_activity_model.py`. Either way the model — not a
/// heuristic — produces the labels.
fn cmd_sessions(cli: &Cli, tz_offset: i64, threshold: f64, json: bool) -> Result<()> {
    // Locate the repo root by a stable marker present for both backends. Try the
    // current dir first, then the compiled-in manifest dir, so the binary keeps
    // working when invoked from elsewhere in the checkout.
    let marker = Path::new("notes/models/automatic_activity_detection_3_1_11.pt");
    let find_root = |start: &Path| -> Option<PathBuf> {
        start.ancestors().find(|d| d.join(marker).is_file()).map(Path::to_path_buf)
    };
    let root = std::env::current_dir()
        .ok()
        .and_then(|d| find_root(&d))
        .or_else(|| find_root(Path::new(env!("CARGO_MANIFEST_DIR"))))
        .ok_or_else(|| {
            anyhow!(
                "could not locate the model under notes/models — run `oura sessions` \
                 from inside the open_oura checkout"
            )
        })?;

    // Absolute DB path (the Python child runs with cwd = repo root).
    let db = cli
        .db
        .canonicalize()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(&cli.db));

    // Fail clearly on a missing DB instead of letting the LibTorch path's
    // Store::open create an empty one (matches the Python runner's resolve_db).
    if !db.exists() {
        return Err(anyhow!(
            "database not found: {} (run `oura sync` first)",
            db.display()
        ));
    }

    #[cfg(feature = "torch")]
    {
        return activity_model::run(&db, &root, tz_offset, threshold, json);
    }

    #[cfg(not(feature = "torch"))]
    {
        use std::process::Command as Proc;
        let script_rel = Path::new("tools/run_activity_model.py");
        // Prefer the repo's venv python (has torch); fall back to python3 on PATH.
        let venv_py = root.join(".venv/bin/python");
        let python = if venv_py.is_file() { venv_py } else { PathBuf::from("python3") };
        let mut cmd = Proc::new(&python);
        cmd.current_dir(&root)
            .arg(root.join(script_rel))
            .arg(&db)
            .arg("--tz")
            .arg(tz_offset.to_string())
            .arg("--threshold")
            .arg(threshold.to_string());
        if json {
            cmd.arg("--json");
        }
        let status = cmd.status().with_context(|| {
            format!(
                "running the activity model via {} (is the Python venv with torch set up?)",
                python.display()
            )
        })?;
        if !status.success() {
            return Err(anyhow!("activity model exited with {status}"));
        }
        Ok(())
    }
}

/// Sleep Score for the latest night, computed live from ring data via
/// tools/score_sleep.py (SleepNet hypnogram + calibrated contributor curves +
/// combiner weights). Always shells out to Python, which already owns the torch
/// model path; there is no native LibTorch backend for the scorer.
fn cmd_sleep_score(cli: &Cli, tz_offset: i64, csv: Option<PathBuf>, json: bool) -> Result<()> {
    let marker = Path::new("tools/score_sleep.py");
    let find_root = |start: &Path| -> Option<PathBuf> {
        start.ancestors().find(|d| d.join(marker).is_file()).map(Path::to_path_buf)
    };
    let root = std::env::current_dir()
        .ok()
        .and_then(|d| find_root(&d))
        .or_else(|| find_root(Path::new(env!("CARGO_MANIFEST_DIR"))))
        .ok_or_else(|| {
            anyhow!("could not locate tools/score_sleep.py — run from inside the open_oura checkout")
        })?;

    let db = cli
        .db
        .canonicalize()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(&cli.db));
    if !db.exists() {
        return Err(anyhow!("database not found: {} (run `oura sync` first)", db.display()));
    }

    use std::process::Command as Proc;
    let venv_py = root.join(".venv/bin/python");
    let python = if venv_py.is_file() { venv_py } else { PathBuf::from("python3") };
    let mut cmd = Proc::new(&python);
    cmd.current_dir(&root)
        .arg(root.join("tools/score_sleep.py"))
        .arg(&db)
        .arg("--tz")
        .arg(tz_offset.to_string());
    if let Some(c) = csv {
        cmd.arg("--csv").arg(c);
    }
    if json {
        cmd.arg("--json");
    }
    let status = cmd.status().with_context(|| {
        format!(
            "running the sleep scorer via {} (is the Python venv with torch set up?)",
            python.display()
        )
    })?;
    if !status.success() {
        return Err(anyhow!("sleep scorer exited with {status}"));
    }
    Ok(())
}

async fn cmd_features(
    cli: &Cli,
    key: &Option<[u8; 16]>,
    enable_hr: bool,
    enable_spo2: bool,
) -> Result<()> {
    use oura_protocol::protocol::{feature, feature_mode};
    let client = connect(cli).await?;
    maybe_auth(&client, key).await?;

    if enable_hr {
        client
            .set_feature_mode(feature::DAYTIME_HR, feature_mode::AUTOMATIC)
            .await
            .context("enabling daytime HR")?;
        println!("Enabled daytime HR (automatic).");
    }
    if enable_spo2 {
        client
            .set_feature_mode(feature::SPO2, feature_mode::AUTOMATIC)
            .await
            .context("enabling SpO2")?;
        println!("Enabled SpO2 (automatic).");
    }

    for (label, id) in [
        ("daytime HR", feature::DAYTIME_HR),
        ("exercise HR", feature::EXERCISE_HR),
        ("SpO2", feature::SPO2),
        ("resting HR", feature::RESTING_HR),
    ] {
        match client.feature_status(id).await {
            Ok(s) => println!(
                "{label:>12}: mode={} state={} status={} sub={}",
                feature_mode_name(s.mode),
                feature_state_name(s.state),
                s.status,
                s.subscription
            ),
            Err(e) => println!("{label:>12}: <{e}>"),
        }
    }

    let _ = client.transport().disconnect().await;
    Ok(())
}

async fn cmd_scan(cli: &Cli) -> Result<()> {
    let found = ble::scan(&cli.name, Duration::from_secs(cli.scan_timeout)).await?;
    if found.is_empty() {
        println!("No Oura rings found.");
        return Ok(());
    }
    for d in found {
        println!("{:>5} dBm  {}  ({})", d.rssi, d.name, d.id);
    }
    Ok(())
}

async fn cmd_pair(cli: &Cli) -> Result<()> {
    let client = connect(cli).await?;
    let serial = client.serial().await.unwrap_or_else(|_| "unknown".into());

    // Reuse an existing key file if present; otherwise mint a fresh key.
    let (key, reused) = match &cli.key_file {
        Some(p) if p.exists() => (
            load_key(&cli.key_file)?.expect("key file exists"),
            true,
        ),
        _ => (generate_key()?, false),
    };
    let out = cli
        .key_file
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("oura-{serial}.key")));

    // Persist the key before installing it, so a crash mid-pair never loses the
    // only copy of a key that may already be live on the ring.
    save_key(&out, &key)?;

    client
        .set_auth_key(&key)
        .await
        .context("set_auth_key failed (is the ring factory-reset / removed from the app?)")?;
    println!(
        "Installed {} auth key on {serial}; saved to {}",
        if reused { "existing" } else { "new" },
        out.display()
    );

    let result = client.authenticate(&key).await.context("verifying auth")?;
    println!("Authenticated: {result:?}");
    match client.battery().await {
        Ok(b) => println!("Battery: {}%", b.percent),
        Err(e) => println!("Battery: <{e}>"),
    }

    println!("\nPaired. Use it with:  oura --key-file {} info", out.display());
    let _ = client.transport().disconnect().await;
    Ok(())
}

async fn cmd_info(cli: &Cli, key: &Option<[u8; 16]>) -> Result<()> {
    let client = connect(cli).await?;

    let info = client.firmware().await?;
    println!("Firmware : {}", info.firmware_version);
    println!("API      : {}", info.api_version);
    println!("BT stack : {}", info.bt_stack_version);
    println!("MAC      : {}", info.mac);

    if let Ok(serial) = client.serial().await {
        println!("Serial   : {serial}");
    }
    if let Ok(hw) = client.hardware_id().await {
        println!("Hardware : {hw}");
    }

    let caps = client.capabilities().await.unwrap_or_default();
    if !caps.is_empty() {
        let rendered: Vec<String> = caps
            .iter()
            .map(|c| format!("{}:{}", c.feature, c.value))
            .collect();
        println!("Caps     : {}", rendered.join(" "));
    }

    if maybe_auth(&client, key).await? {
        match client.battery().await {
            Ok(b) => println!("Battery  : {}%", b.percent),
            Err(e) => println!("Battery  : <{e}>"),
        }
    } else {
        println!("Battery  : <pass --key-file to read (auth required)>");
    }

    let _ = client.transport().disconnect().await;
    Ok(())
}

async fn cmd_sync(cli: &Cli, key: &Option<[u8; 16]>, sync_time: bool) -> Result<()> {
    let key = key
        .as_ref()
        .ok_or_else(|| anyhow!("sync requires --key-file (history events are auth-gated)"))?;

    let client = connect(cli).await?;
    client.authenticate(key).await.context("authenticating")?;

    if sync_time {
        client.sync_time().await.context("syncing time")?;
    }

    let serial = client.serial().await.unwrap_or_else(|_| "unknown".into());
    let info = client.firmware().await.ok();

    let store = Store::open(&cli.db)?;
    store.upsert_device(&serial, None, info.as_ref())?;

    let cursor = store.cursor(&serial)?;
    println!("Syncing events for {serial} from cursor {cursor} ...");

    let mut inserted = 0u32;
    // Capture any DB error (insert or cursor write) so a failure surfaces loudly
    // instead of being swallowed — and so the cursor is never advanced past events
    // we failed to store (which would drop them permanently on the next sync).
    let db_err = std::cell::RefCell::new(None);
    // Track whether any batch cursor was actually persisted, so the error message
    // below is accurate even when earlier batches already advanced the cursor.
    let cursor_advanced = std::cell::Cell::new(false);
    let outcome = client
        .drain_events(
            cursor,
            |ev| {
                if db_err.borrow().is_some() {
                    return;
                }
                match store.insert_event(&serial, ev) {
                    Ok(true) => inserted += 1,
                    Ok(false) => {}
                    Err(e) => *db_err.borrow_mut() = Some(e),
                }
            },
            // Persist the cursor after each fully-drained batch (so an interrupted
            // sync still makes progress) — but not once a DB write has failed. A
            // failed cursor write is itself recorded so it can't be silently lost.
            |c| {
                if db_err.borrow().is_some() {
                    return;
                }
                match store.set_cursor(&serial, c) {
                    Ok(()) => cursor_advanced.set(true),
                    Err(e) => *db_err.borrow_mut() = Some(e),
                }
            },
        )
        .await?;
    if let Some(e) = db_err.into_inner() {
        let ctx = if cursor_advanced.get() {
            "storing event during sync (cursor advanced through earlier batches)"
        } else {
            "storing event during sync (cursor not advanced)"
        };
        return Err(e).context(ctx);
    }

    store.set_cursor(&serial, outcome.next_cursor)?;
    println!(
        "Done: {} events received, {} new rows, next cursor {}.",
        outcome.events_synced, inserted, outcome.next_cursor
    );

    // While connected + authed, snapshot the real on-ring feature modes so the
    // dashboard can show actual on/off (not just "events seen recently"). Best-effort:
    // written next to the DB as feature_modes.json; a read failure never fails the sync.
    {
        let feats = [
            (0x02u8, "daytime_hr"), (0x03, "exercise_hr"), (0x04, "spo2"),
            (0x08, "resting_hr"), (0x0b, "real_steps"), (0x0c, "experimental"), (0x0d, "cva_ppg"),
        ];
        let mut modes = serde_json::Map::new();
        for (id, name) in feats {
            if let Ok(s) = client.feature_status(id).await {
                modes.insert(name.to_string(), serde_json::json!(s.mode));
            }
        }
        if !modes.is_empty() {
            let at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            modes.insert("_at".into(), serde_json::json!(at));
            let path = cli.db.parent().unwrap_or_else(|| std::path::Path::new(".")).join("feature_modes.json");
            let _ = std::fs::write(path, serde_json::to_vec_pretty(&serde_json::Value::Object(modes)).unwrap_or_default());
        }
    }

    let _ = client.transport().disconnect().await;
    Ok(())
}

async fn cmd_latest(cli: &Cli, key: &Option<[u8; 16]>) -> Result<()> {
    let client = connect(cli).await?;
    maybe_auth(&client, key).await?;
    let serial = client.serial().await.unwrap_or_else(|_| "unknown".into());
    let store = Store::open(&cli.db).ok();

    use oura_protocol::protocol::feature;
    for (label, id) in [
        ("daytime HR", feature::DAYTIME_HR),
        ("exercise HR", feature::EXERCISE_HR),
        ("SpO2", feature::SPO2),
    ] {
        match client.feature_latest(id).await {
            Ok(v) => {
                let mut parts = Vec::new();
                if let Some(bpm) = v.bpm {
                    parts.push(format!("{bpm} bpm"));
                    if let Some(s) = &store {
                        let _ = s.insert_reading(&serial, "heart_rate", bpm as f64, "bpm");
                    }
                }
                if let Some(spo2) = v.spo2_percent {
                    parts.push(format!("{spo2}% SpO2"));
                    if let Some(s) = &store {
                        let _ = s.insert_reading(&serial, "spo2", spo2 as f64, "%");
                    }
                }
                let summary = if parts.is_empty() {
                    "no value (worn?)".to_string()
                } else {
                    parts.join(", ")
                };
                println!("{label:>12}: {summary}");
            }
            Err(e) => println!("{label:>12}: <{e}>"),
        }
    }

    let _ = client.transport().disconnect().await;
    Ok(())
}

async fn cmd_live_hr(cli: &Cli, key: &Option<[u8; 16]>, seconds: u64, raw: bool) -> Result<()> {
    let client = connect(cli).await?;
    maybe_auth(&client, key).await?;
    let serial = client.serial().await.unwrap_or_else(|_| "unknown".into());
    let store = Store::open(&cli.db).ok();

    println!("Streaming live heart rate for {seconds}s (Ctrl-C to stop early)...");
    let mut count = 0u32;
    client
        .live_heart_rate(Duration::from_secs(seconds), raw, |s| {
            count += 1;
            println!("  {} bpm (IBI {} ms)", s.bpm, s.ibi_ms);
            if let Some(store) = &store {
                let _ = store.insert_reading(&serial, "heart_rate_live", s.bpm as f64, "bpm");
            }
        })
        .await?;

    if count == 0 {
        println!("No beats captured. Make sure the ring is worn.");
    }
    let _ = client.transport().disconnect().await;
    Ok(())
}

async fn cmd_sleep_analyze(cli: &Cli, key: &Option<[u8; 16]>, force: bool) -> Result<()> {
    let client = connect(cli).await?;
    maybe_auth(&client, key).await?;
    let status = client.check_sleep_analysis(force).await?;
    println!(
        "Sleep analysis triggered (force={force}); status={status} (0 = ok). \
         Give the ring time to postprocess, then `sync` to fetch sleep events."
    );
    let _ = client.transport().disconnect().await;
    Ok(())
}

async fn cmd_accel(cli: &Cli, key: &Option<[u8; 16]>, seconds: u64) -> Result<()> {
    let client = connect(cli).await?;
    maybe_auth(&client, key).await?;

    println!("Streaming accelerometer for {seconds}s — wave your hand!");
    let mut count = 0u32;
    let mut mags: Vec<f64> = Vec::new();
    client
        .stream_accelerometer(Duration::from_secs(seconds), |s| {
            count += 1;
            let m = s.magnitude();
            mags.push(m);
            if count.is_multiple_of(10) {
                println!("  x={:>6} y={:>6} z={:>6}  |a|={:.0}", s.x, s.y, s.z, m);
            }
        })
        .await?;

    if count == 0 {
        println!("No samples. Make sure the ring is worn.");
    } else {
        let min = mags.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = mags.iter().cloned().fold(0.0, f64::max);
        let moved = (max - min) > 2000.0;
        println!(
            "{count} samples; |a| range {min:.0}..{max:.0} — {}",
            if moved { "motion detected ✋" } else { "mostly still" }
        );
    }
    let _ = client.transport().disconnect().await;
    Ok(())
}

async fn cmd_rdata(cli: &Cli, key: &Option<[u8; 16]>, action: &str) -> Result<()> {
    let client = connect(cli).await?;
    maybe_auth(&client, key).await?;
    match action {
        "state" => {
            let (subtag, status) = client.rdata_state().await?;
            println!("RData state: subtag={subtag} status={status} (0 = idle/none active)");
        }
        "stop" => {
            let s = client.rdata_stop().await?;
            println!("RData stop -> status {s} ({})", rdata_status_name(s));
        }
        "clear" => {
            let s = client.rdata_clear().await?;
            println!("RData clear -> status {s} ({})", rdata_status_name(s));
        }
        "probe" => {
            rdata_probe(&client, 30).await?;
        }
        "sweep" => {
            rdata_sweep(&client).await?;
        }
        "recipe" => {
            rdata_recipe(&client).await?;
        }
        "unlock" => {
            rdata_unlock(&client).await?;
        }
        other => {
            anyhow::bail!("unknown rdata action '{other}' (use state | stop | clear | probe)");
        }
    }
    let _ = client.transport().disconnect().await;
    Ok(())
}

/// RData capacity rate-probe (Phase-2 spike): arm ACM-50, record briefly, then
/// stop and drain pages to measure the real byte rate and page size. **ALWAYS**
/// tears down (stop + clear) even on error. Ring should be on the charger and
/// still — sampling rate is fixed at 50 Hz regardless of motion.
async fn rdata_probe(client: &OuraClient<BleTransport>, secs: u64) -> Result<()> {
    use oura_protocol::protocol::rdata::DataType;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    // The ring validates configure timestamps against its own clock, so sync it
    // to host UTC first — without this, arming returns idle (status 3).
    client.sync_time().await?;
    let (sub0, st0) = client.rdata_state().await?;
    let batt0 = client.battery().await?;
    println!("After sync_time: RData state subtag={sub0} status={st0}; battery {}%", batt0.percent);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0);

    let (csub, cst) = client.rdata_configure(&[DataType::Acm2g50Hz], now, now).await?;
    println!("Armed ACM 2g @ 50 Hz: configure subtag={csub} status={cst}");

    // From here on, tear down NO MATTER WHAT.
    let body = async {
        println!("Recording for {secs}s ({:.1} min)...", secs as f64 / 60.0);
        tokio::time::sleep(Duration::from_secs(secs)).await;
        let (subm, stm) = client.rdata_state().await?;
        let batt1 = client.battery().await?;
        println!(
            "After record: RData state subtag={subm} status={stm}; battery {}% (Δ {} pts over {:.1} min)",
            batt1.percent,
            batt0.percent as i16 - batt1.percent as i16,
            secs as f64 / 60.0
        );
        // Drain BEFORE stop — the documented lifecycle is configure -> get_page ->
        // stop -> clear, and stop may discard the buffer.
        println!("Draining pages (before stop)...");
        let mut pages = 0u32;
        let mut total_bytes = 0usize;
        let mut page_len = 0usize;
        for page in 0u16..4000 {
            let (status, bytes) = client.rdata_get_page(page).await?;
            if page == 0 {
                println!("  page 0: status={status}, {} bytes, raw={}", bytes.len(), hex::encode(&bytes));
            }
            if status == 6 || bytes.is_empty() {
                break; // NO_DATA -> past the end of recorded data
            }
            page_len = page_len.max(bytes.len());
            total_bytes += bytes.len();
            pages += 1;
        }
        Ok::<_, anyhow::Error>((pages, total_bytes, page_len, batt1.percent))
    }
    .await;

    // Mandatory teardown.
    if let Err(e) = client.rdata_stop().await {
        eprintln!("warning: teardown stop failed: {e}");
    }
    if let Err(e) = client.rdata_clear().await {
        eprintln!("warning: teardown clear failed: {e}");
    }
    let st1 = client.rdata_state().await.map(|(_, s)| s).unwrap_or(255);
    println!("RData state after teardown: status={st1}");

    let (pages, total_bytes, page_len, batt1) = body?;
    if pages == 0 {
        println!(
            "\nNo pages drained. Either nothing recorded, or `stop` discards the \
             buffer (in which case we must drain BEFORE stop). Will adjust and retry."
        );
        return Ok(());
    }
    let rate = total_bytes as f64 / secs as f64;
    let batt_delta = batt0.percent as i16 - batt1 as i16;
    println!("\n--- RData rate probe (ACM 2g @ 50 Hz, {secs}s / {:.1} min) ---", secs as f64 / 60.0);
    println!("pages drained  : {pages}");
    println!("max page size  : {page_len} bytes (payload after subtag+status header)");
    println!("total bytes    : {total_bytes}");
    println!("byte rate      : {rate:.0} B/s  (~{:.1} KB/min)", rate * 60.0 / 1024.0);
    println!("battery        : {}% -> {batt1}% (Δ {batt_delta} pts)", batt0.percent);
    if batt_delta > 0 {
        let pct_per_hr = batt_delta as f64 * 3600.0 / secs as f64;
        println!("battery rate   : ~{pct_per_hr:.1} %/hr while sampling");
    } else {
        println!("battery        : drop below 1% resolution over this window");
    }
    if rate > 0.0 {
        let max_addr_bytes = (page_len.max(1) * 65536) as f64;
        println!(
            "addressable ceiling (65536 pages × {page_len} B, IF all flash-backed): \
             ~{:.0} min of capture",
            max_addr_bytes / rate / 60.0
        );
    }
    Ok(())
}

/// RData status-code name (from the decompiled app's `RDataStatusCode`).
fn rdata_status_name(s: u8) -> &'static str {
    match s {
        0 => "SUCCESS",
        3 => "INVALID_SUBTAG",
        5 => "NOT_INITIALIZED",
        7 => "RECORDING_ON",
        11 => "SYNC_NOT_IDLE",
        12 => "MEMORY_FULL",
        _ => "?",
    }
}

/// Test 1: run the app's strict start recipe — Clear (require SUCCESS) -> state ->
/// CONFIGURE with startTime=0 — and report each status. Tears down afterward.
async fn rdata_recipe(client: &OuraClient<BleTransport>) -> Result<()> {
    use oura_protocol::protocol::rdata::DataType;
    use std::time::{SystemTime, UNIX_EPOCH};

    client.sync_time().await?;
    let clear = client.rdata_clear().await?;
    println!("RDataClear  -> status {clear} ({})", rdata_status_name(clear));
    let (ssub, sbyte) = client.rdata_state().await?;
    println!("RDataState  -> subtag {ssub}, state byte {sbyte} (0 IDLE,1 SCHED,2 REC,3 STOP,4 BUSY)");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0);
    let (csub, cst) = client.rdata_configure(&[DataType::Acm2g50Hz], 0, now).await?;
    println!(
        "RDataConfigure(start=0) -> subtag {csub}, status {cst} ({})",
        rdata_status_name(cst)
    );
    if cst == 0 {
        println!(">>> START ACCEPTED — capability is enabled after all!");
    } else {
        println!(">>> still {} — capability gate confirmed.", rdata_status_name(cst));
    }
    // Teardown regardless.
    let _ = client.rdata_stop().await;
    let _ = client.rdata_clear().await;
    Ok(())
}

/// Combined in-session unlock attempt: confirm RAW_DATA_SAMPLER is advertised,
/// try enabling it (subscription + feature-mode) on the correct id 0x12, then
/// Clear -> Configure in the SAME session. If Configure is accepted, do a short
/// capture and drain. Always tears down (stop/clear/unsubscribe).
async fn rdata_unlock(client: &OuraClient<BleTransport>) -> Result<()> {
    use oura_protocol::protocol::rdata::DataType;
    use oura_protocol::protocol::{capability, feature_mode, subscription_mode};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    client.sync_time().await?;
    let caps = client.capabilities().await?;
    let ver = |id: u8| caps.iter().find(|c| c.feature == id).map(|c| c.value);
    println!(
        "capabilities: RAW_DATA_SAMPLER(0x12)={:?}  RESEARCH_DATA(0x01)={:?}",
        ver(capability::RAW_DATA_SAMPLER),
        ver(capability::RESEARCH_DATA),
    );

    // Try both enable paths (best-effort; report outcomes).
    match client.set_feature_subscription(capability::RAW_DATA_SAMPLER, subscription_mode::FEATURE_DATA).await {
        Ok(r) => println!("subscribe RAW_DATA_SAMPLER(data) -> result {r}"),
        Err(e) => println!("subscribe RAW_DATA_SAMPLER failed: {e}"),
    }
    match client.set_feature_mode(capability::RAW_DATA_SAMPLER, feature_mode::REQUESTED).await {
        Ok(()) => println!("set_feature_mode RAW_DATA_SAMPLER(REQUESTED) -> SUCCESS"),
        Err(e) => println!("set_feature_mode RAW_DATA_SAMPLER -> {e}"),
    }

    let clear = client.rdata_clear().await?;
    println!("RDataClear -> status {clear} ({})", rdata_status_name(clear));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0);
    let (_, cst) = client.rdata_configure(&[DataType::Acm2g50Hz], 0, now).await?;
    println!("RDataConfigure(start=0) -> status {cst} ({})", rdata_status_name(cst));

    let mut captured = (0u32, 0usize);
    if cst == 0 {
        println!(">>> ACCEPTED! Recording 30 s...");
        tokio::time::sleep(Duration::from_secs(30)).await;
        for page in 0u16..4000 {
            let (status, bytes) = client.rdata_get_page(page).await?;
            if status == 6 || bytes.is_empty() {
                break;
            }
            captured.0 += 1;
            captured.1 += bytes.len();
        }
        println!(">>> drained {} pages, {} bytes", captured.0, captured.1);
    } else {
        println!(">>> still {} — enabling did not unlock CONFIGURE.", rdata_status_name(cst));
    }

    // Teardown.
    let _ = client.rdata_stop().await;
    let _ = client.rdata_clear().await;
    let _ = client
        .set_feature_subscription(capability::RAW_DATA_SAMPLER, subscription_mode::OFF)
        .await;
    Ok(())
}

/// Try several RData `configure` argument variants and report which (if any)
/// moves the ring out of the idle state (status 3). Each attempt is immediately
/// torn down. Pure diagnostic for the Phase-2 spike — no long recording.
async fn rdata_sweep(client: &OuraClient<BleTransport>) -> Result<()> {
    use oura_protocol::protocol::rdata::DataType;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    client.sync_time().await?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0);

    // (label, types, start, current)
    let variants: &[(&str, &[DataType], u32, u32)] = &[
        ("ACM, start=now",          &[DataType::Acm2g50Hz], now, now),
        ("ACM, start=0",            &[DataType::Acm2g50Hz], 0, now),
        ("Metadata+ACM, start=now", &[DataType::Metadata, DataType::Acm2g50Hz], now, now),
        ("Metadata+ACM, start=0",   &[DataType::Metadata, DataType::Acm2g50Hz], 0, now),
        ("ACM 8g, start=now",       &[DataType::Acm8g50Hz], now, now),
    ];

    let (_, idle) = client.rdata_state().await?;
    println!("baseline idle status = {idle}\n");
    for (label, types, start, cur) in variants {
        let (csub, cst) = client.rdata_configure(types, *start, *cur).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let (ssub, sst) = client.rdata_state().await?;
        let engaged = sst != idle;
        println!(
            "{label:<26} configure(sub={csub},st={cst}) -> state(sub={ssub},st={sst}) {}",
            if engaged { "<<< STATE CHANGED" } else { "(still idle)" }
        );
        // tear down before the next attempt
        let _ = client.rdata_stop().await;
        let _ = client.rdata_clear().await;
    }
    println!("\nIf every variant stayed idle, RDataStart needs a precondition we");
    println!("haven't replicated — next step is decompiling the app's start path.");
    Ok(())
}

async fn cmd_events(cli: &Cli) -> Result<()> {
    let store = Store::open(&cli.db)?;
    let serials = store.device_serials()?;
    if serials.is_empty() {
        println!("No events stored yet. Run `oura sync --key-file <key>` first.");
        return Ok(());
    }
    for serial in serials {
        println!("Device {serial}:");
        for (name, count) in store.event_counts(&serial)? {
            println!("  {count:>6}  {name}");
        }
    }
    Ok(())
}
