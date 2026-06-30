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
