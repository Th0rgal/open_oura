//! `oura-core` — the UniFFI surface the iOS (and any native) client links against.
//!
//! Everything here delegates to the shared Rust crates that already power the web
//! dashboard (`oura-analysis`, `oura-store`, …). The contract returned to Swift is
//! the same JSON the web client renders, so the two clients never diverge. Bindings
//! are generated with UniFFI (`uniffi-bindgen`), packaged as an `.xcframework`.

use serde_json::json;

uniffi::setup_scaffolding!();

/// Build/version string — a trivial call to validate the FFI round-trip.
#[uniffi::export]
pub fn core_version() -> String {
    format!("oura-core {}", env!("CARGO_PKG_VERSION"))
}

/// HRV RMSSD (ms) over inter-beat intervals — the shared `oura-analysis` algorithm,
/// reachable natively. Returns -1 when the input is too short.
#[uniffi::export]
pub fn rmssd(ibi_ms: Vec<u16>) -> f64 {
    oura_analysis::ported::hrv::rmssd(&ibi_ms).unwrap_or(-1.0)
}

/// The full dashboard summary — the SAME `build_summary()` JSON the web client
/// renders, computed from the synced SQLite DB. `tz_offset` is hours from UTC.
///
/// Models (sleep hypnogram / cardiovascular age / activity sessions) use a
/// [`oura_summary::ModelRunner`]; on-device we'll pass the `.ptl` torch runner.
/// For now [`oura_summary::NoModelRunner`] yields the signal-derived panels
/// (vitals, cardio trend, activity profile, device & data-health, digest) — most
/// of the dashboard — with model fields null until the torch runner is wired.
///
/// Returns the summary JSON string, or `{ "error": "…" }`.
#[uniffi::export]
pub fn summary_json(db_path: String, tz_offset: i64) -> String {
    match oura_summary::build_summary(
        std::path::Path::new(&db_path),
        tz_offset,
        &oura_summary::NoModelRunner,
    ) {
        Ok(v) => v.to_string(),
        Err(e) => json!({ "error": e.to_string() }).to_string(),
    }
}

/// A lightweight, model-free summary (device + data-health only) — kept as a fast
/// path / fallback. Returns `{ serials, device, event_counts, decoded_events }`.
#[uniffi::export]
pub fn quick_summary_json(db_path: String) -> String {
    match quick_summary(&db_path) {
        Ok(v) => v.to_string(),
        Err(e) => json!({ "error": e }).to_string(),
    }
}

fn quick_summary(db_path: &str) -> Result<serde_json::Value, String> {
    let store = oura_store::storage::Store::open(db_path).map_err(|e| e.to_string())?;
    let serials = store.device_serials().map_err(|e| e.to_string())?;
    let primary = serials.first().cloned().unwrap_or_default();

    let device = store.device_info().map_err(|e| e.to_string())?.map(
        |(serial, hardware_id, firmware, api_version, mac, updated_unix, last_sync_unix, cursor)| {
            json!({ "serial": serial, "hardware_id": hardware_id, "firmware": firmware,
                    "api_version": api_version, "mac": mac, "updated_unix": updated_unix,
                    "last_sync_unix": last_sync_unix, "next_cursor": cursor })
        },
    );

    let event_counts: Vec<_> = store
        .event_counts(&primary)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(kind, n)| json!({ "kind": kind, "count": n }))
        .collect();

    let decoded = store.decoded_events().map_err(|e| e.to_string())?.len();

    Ok(json!({
        "serials": serials,
        "device": device,
        "event_counts": event_counts,
        "decoded_events": decoded,
    }))
}

// ── on-device BLE sync over a Swift-provided transport ────────────────────────
// The iOS app does CoreBluetooth; this drives the SAME oura-link OuraClient<T>
// (auth → app stream → drain → store) over a transport that bridges to Swift, so
// the device builds its own DB from a real ring — no btleplug, no cloud.
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use oura_link::transport::Transport;
use oura_link::OuraClient;
use oura_store::storage::Store;
use tokio::sync::broadcast;

/// Swift implements this to send one request frame over CoreBluetooth. Fire-and-
/// forget: the ring's responses come back asynchronously via `push_frame`.
#[uniffi::export(callback_interface)]
pub trait BleWriter: Send + Sync {
    fn write(&self, data: Vec<u8>);
}

#[derive(uniffi::Record)]
pub struct SyncReport {
    pub serial: String,
    pub events_synced: u32,
    pub inserted: u32,
    pub next_cursor: u32,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum SyncError {
    #[error("{0}")]
    Failed(String),
}

/// A live sync session bound to a connected ring: Swift creates it with a writer,
/// feeds inbound BLE frames via `push_frame`, then awaits `sync`.
#[derive(uniffi::Object)]
pub struct RingSession {
    tx: broadcast::Sender<Vec<u8>>,
    writer: Arc<dyn BleWriter>,
}

/// Bridges oura-link's `Transport` onto the Swift writer + the inbound frame channel.
struct FfiTransport {
    tx: broadcast::Sender<Vec<u8>>,
    writer: Arc<dyn BleWriter>,
}

#[async_trait::async_trait]
impl Transport for FfiTransport {
    async fn write(&self, data: &[u8]) -> oura_link::Result<()> {
        self.writer.write(data.to_vec());
        Ok(())
    }
    fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.tx.subscribe()
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl RingSession {
    #[uniffi::constructor]
    pub fn new(writer: Box<dyn BleWriter>) -> Arc<Self> {
        let (tx, _) = broadcast::channel(8192);
        Arc::new(Self { tx, writer: Arc::from(writer) })
    }

    /// Swift pushes each inbound BLE notification frame here.
    pub fn push_frame(&self, data: Vec<u8>) {
        let _ = self.tx.send(data);
    }

    /// Authenticate, set up the app stream, and drain history events into the DB at
    /// `db_path`. `key_hex` is the 32-char ring auth key. Returns the sync counts.
    pub async fn sync(&self, db_path: String, key_hex: String) -> Result<SyncReport, SyncError> {
        let fail = |e: String| SyncError::Failed(e);
        let key = parse_key(&key_hex).ok_or_else(|| fail("auth key must be 32 hex chars".into()))?;
        let transport = FfiTransport { tx: self.tx.clone(), writer: self.writer.clone() };
        let client = OuraClient::new(transport);

        client.authenticate(&key).await.map_err(|e| fail(e.to_string()))?;
        client.setup_app_stream().await.map_err(|e| fail(e.to_string()))?;
        let serial = client.serial().await.unwrap_or_else(|_| "unknown".into());
        let info = client.firmware().await.ok();

        // Mutex<Store> keeps the future Send across the drain's awaits (rusqlite's
        // Connection is !Sync), while still writing incrementally (no buffering).
        let store = Mutex::new(Store::open(&db_path).map_err(|e| fail(e.to_string()))?);
        store.lock().unwrap().upsert_device(&serial, None, info.as_ref()).map_err(|e| fail(e.to_string()))?;
        let cursor = store.lock().unwrap().cursor(&serial).map_err(|e| fail(e.to_string()))?;

        let inserted = AtomicU32::new(0);
        let db_err: Mutex<Option<String>> = Mutex::new(None);
        let outcome = client
            .drain_events(
                cursor,
                |ev| {
                    if db_err.lock().unwrap().is_some() {
                        return;
                    }
                    match store.lock().unwrap().insert_event(&serial, ev) {
                        Ok(true) => { inserted.fetch_add(1, Ordering::Relaxed); }
                        Ok(false) => {}
                        Err(e) => *db_err.lock().unwrap() = Some(e.to_string()),
                    }
                },
                |c| {
                    if db_err.lock().unwrap().is_some() {
                        return;
                    }
                    if let Err(e) = store.lock().unwrap().set_cursor(&serial, c) {
                        *db_err.lock().unwrap() = Some(e.to_string());
                    }
                },
            )
            .await
            .map_err(|e| fail(e.to_string()))?;

        if let Some(msg) = db_err.into_inner().unwrap() {
            return Err(fail(msg));
        }
        Ok(SyncReport {
            serial,
            events_synced: outcome.events_synced,
            inserted: inserted.into_inner(),
            next_cursor: outcome.next_cursor,
        })
    }
}

fn parse_key(hex: &str) -> Option<[u8; 16]> {
    let hex = hex.trim();
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut key = [0u8; 16];
    for i in 0..16 {
        key[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(key)
}
