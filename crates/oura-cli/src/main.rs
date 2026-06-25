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
    /// RData bulk raw-sampler control: `state` (read), `stop`/`clear` (teardown).
    /// Starting a collection is intentionally not exposed here — it writes a
    /// persistent flash session that must be torn down (see docs/native-decoder.md).
    Rdata {
        /// One of: state | stop | clear
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
    /// Detect activity/exposure sessions (workout, swim, sauna, cold) from stored
    /// events. Heuristic — open_oura's own, not an Oura algorithm.
    Sessions {
        /// Timezone offset (hours from UTC) for displayed times.
        #[arg(long, default_value_t = 0)]
        tz_offset: i64,
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
    let contents = format!("{}\n", hex::encode(key));
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        // Create with 0600 from the start so the key is never briefly world-readable.
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("writing key file {}", path.display()))?;
        f.write_all(contents.as_bytes())?;
        // mode() above only applies on create; tighten an already-existing file too.
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)
            .with_context(|| format!("writing key file {}", path.display()))?;
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
        Command::Sessions { tz_offset } => cmd_sessions(&cli, *tz_offset),
        Command::Subscribe { feature, mode } => cmd_subscribe(&cli, &key, feature, mode).await,
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

/// Detect activity/exposure sessions from stored events (open_oura heuristic).
fn cmd_sessions(cli: &Cli, tz_offset: i64) -> Result<()> {
    use oura_analysis::original::activity_session::{detect, Config, MinuteSample};
    use std::collections::BTreeMap;

    let store = Store::open(&cli.db)?;
    let events = store.decoded_events()?;
    if events.is_empty() {
        println!("No decoded events in {}. Run `oura sync` first.", cli.db.display());
        return Ok(());
    }
    // Anchor ring deciseconds to wall-clock using the latest event's capture time.
    let (max_ds, anchor_unix) = events
        .iter()
        .map(|(ds, _, _, cu)| (*ds, *cu))
        .max_by_key(|(ds, _)| *ds)
        .unwrap();
    let minute_of = |ds: i64| -> i64 { (anchor_unix - (max_ds - ds) / 10) / 60 };

    // Aggregate per-minute signals from the relevant event types.
    #[derive(Default)]
    struct Bucket {
        met: Option<f64>,
        motion: u32,
        temp: Vec<f64>,
        hr: Vec<u32>,
    }
    let mut buckets: BTreeMap<i64, Bucket> = BTreeMap::new();
    for (ds, tag, json, _) in &events {
        let v: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let b = buckets.entry(minute_of(*ds)).or_default();
        match tag {
            0x46 | 0x69 | 0x75 => {
                if let Some(a) = v.get("temps_c").and_then(|t| t.as_array()) {
                    b.temp.extend(a.iter().filter_map(|x| x.as_f64()));
                }
            }
            0x47 => {
                if let Some(hi) = v.get("high_intensity").and_then(|x| x.as_u64()) {
                    b.motion += hi as u32;
                }
            }
            0x50 => {
                if let Some(a) = v.get("met").and_then(|t| t.as_array()) {
                    let m = a.iter().filter_map(|x| x.as_f64()).fold(0.0, f64::max);
                    b.met = Some(b.met.unwrap_or(0.0).max(m));
                }
            }
            0x80 => {
                if let Some(a) = v.get("hr_bpm").and_then(|t| t.as_array()) {
                    b.hr.extend(a.iter().filter_map(|x| x.as_u64().map(|n| n as u32)));
                }
            }
            _ => {}
        }
    }

    let samples: Vec<MinuteSample> = buckets
        .into_iter()
        .map(|(minute, b)| MinuteSample {
            minute,
            met: b.met,
            motion: b.motion,
            temp_c: (!b.temp.is_empty()).then(|| b.temp.iter().sum::<f64>() / b.temp.len() as f64),
            hr: (!b.hr.is_empty()).then(|| b.hr.iter().sum::<u32>() / b.hr.len() as u32),
        })
        .collect();

    let sessions = detect(&samples, &Config::default());
    if sessions.is_empty() {
        println!("No activity/exposure sessions detected.");
        return Ok(());
    }
    let hm = |minute: i64| -> String {
        let sod = (minute * 60 + tz_offset * 3600).rem_euclid(86400);
        format!("{:02}:{:02}", sod / 3600, (sod % 3600) / 60)
    };
    println!("Detected sessions (open_oura heuristic — not Oura's classification):\n");
    println!("  {:<13}  {:<13}  {:>4}  {:>8}  {:>4}  temp(C)", "kind", "time", "min", "peakMET", "HR");
    for s in &sessions {
        let temp = match (s.temp_min, s.temp_max) {
            (Some(lo), Some(hi)) => format!("[{lo:.1}, {hi:.1}]"),
            _ => "-".into(),
        };
        let hr = s.mean_hr.map(|h| h.to_string()).unwrap_or_else(|| "-".into());
        let kind = format!("{:?}", s.kind);
        let span = format!("{}-{}", hm(s.start_minute), hm(s.end_minute));
        println!(
            "  {kind:<13}  {span:<13}  {:>4}  {:>8.1}  {hr:>4}  {temp}",
            s.minutes, s.peak_met,
        );
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
    let outcome = client
        .drain_events(cursor, |ev| {
            if store.insert_event(&serial, ev).unwrap_or(false) {
                inserted += 1;
            }
        })
        .await?;

    store.set_cursor(&serial, outcome.next_cursor)?;
    println!(
        "Done: {} events received, {} new rows, next cursor {}.",
        outcome.events_synced, inserted, outcome.next_cursor
    );

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
            client.rdata_stop().await?;
            println!("RData stop sent.");
        }
        "clear" => {
            client.rdata_clear().await?;
            println!("RData clear sent.");
        }
        other => {
            anyhow::bail!("unknown rdata action '{other}' (use state | stop | clear)");
        }
    }
    let _ = client.transport().disconnect().await;
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
