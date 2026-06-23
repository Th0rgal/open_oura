//! `oura` — a command-line client that reads data directly from an Oura ring over
//! BLE, with no Oura cloud account. See `--help` for subcommands.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};

use oura_core::ble::{self, BleTransport};
use oura_core::storage::Store;
use oura_core::OuraClient;

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
    },
    /// Show stored event counts from the database (offline).
    Events,
}

fn load_key(path: &Option<PathBuf>) -> Result<Option<[u8; 16]>> {
    let Some(path) = path else { return Ok(None) };
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading key file {}", path.display()))?;
    let bytes = hex::decode(text.trim()).context("key file is not valid hex")?;
    let key: [u8; 16] = bytes
        .try_into()
        .map_err(|_| anyhow!("auth key must be exactly 16 bytes"))?;
    Ok(Some(key))
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
        Command::Info => cmd_info(&cli, &key).await,
        Command::Sync { sync_time } => cmd_sync(&cli, &key, *sync_time).await,
        Command::Latest => cmd_latest(&cli, &key).await,
        Command::LiveHr { seconds } => cmd_live_hr(&cli, &key, *seconds).await,
        Command::Events => cmd_events(&cli).await,
    }
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

    use oura_core::protocol::feature;
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

async fn cmd_live_hr(cli: &Cli, key: &Option<[u8; 16]>, seconds: u64) -> Result<()> {
    let client = connect(cli).await?;
    maybe_auth(&client, key).await?;
    let serial = client.serial().await.unwrap_or_else(|_| "unknown".into());
    let store = Store::open(&cli.db).ok();

    println!("Streaming live heart rate for {seconds}s (Ctrl-C to stop early)...");
    let mut count = 0u32;
    client
        .live_heart_rate(Duration::from_secs(seconds), |s| {
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
