//! Optional SQLite persistence (feature `storage`).
//!
//! Events are stored with their raw body retained, so unknown event types are
//! never lost and can be decoded later. A per-device sync cursor enables
//! incremental syncs. Re-syncing is idempotent: identical events are de-duped.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::Result;
use oura_protocol::device::{Battery, DeviceInfo};
use oura_protocol::events::RingEvent;

/// Current historical-event decoder schema version stored in `store_meta`.
///
/// Version 2 backfills historical `ring_start` (`0x41`) decodes while preserving
/// synthetic phone `time_sync` (`0x42`) provenance and valid existing JSON.
pub const DECODER_VERSION: i64 = 2;

/// Default number of event rows processed per transaction during `redecode`.
pub const REDECODE_BATCH_SIZE: usize = 1_000;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS store_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS device (
    serial        TEXT PRIMARY KEY,
    hardware_id   TEXT,
    firmware      TEXT,
    api_version   TEXT,
    mac           TEXT,
    updated_unix  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sync_state (
    serial        TEXT PRIMARY KEY,
    next_cursor   INTEGER NOT NULL,
    last_sync_unix INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    serial         TEXT NOT NULL,
    tag            INTEGER NOT NULL,
    name           TEXT NOT NULL,
    ring_timestamp INTEGER NOT NULL,
    body           BLOB NOT NULL,
    decoded_json   TEXT,
    captured_unix  INTEGER NOT NULL,
    UNIQUE(serial, tag, ring_timestamp, body)
);
CREATE INDEX IF NOT EXISTS idx_events_serial_tag ON events(serial, tag);
CREATE INDEX IF NOT EXISTS idx_events_capture ON events(captured_unix, id);
CREATE INDEX IF NOT EXISTS idx_events_tag_time ON events(tag, ring_timestamp);
-- Covers the iOS models' store digest (count / last id of decoded rows and anchors),
-- which otherwise scans every row of the table on each analysis pass.
CREATE INDEX IF NOT EXISTS idx_events_decoded_tag ON events(tag) WHERE decoded_json IS NOT NULL;

CREATE TABLE IF NOT EXISTS readings (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    serial        TEXT NOT NULL,
    kind          TEXT NOT NULL,
    value         REAL NOT NULL,
    unit          TEXT,
    captured_unix INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_readings_serial_kind ON readings(serial, kind);
"#;

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Decode an event body for storage or migration while preserving provenance and
/// existing valid JSON when decoding is unsupported or fails.
pub fn decode_stored_event(
    tag: u8,
    body: &[u8],
    existing_decoded_json: Option<&str>,
) -> Option<String> {
    let existing_val = existing_decoded_json
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());

    if let Some(mut decoded) = oura_protocol::events::decode_event_body(tag, body) {
        if tag == 0x42 {
            let existing_source = existing_val
                .as_ref()
                .and_then(|v| v.get("source"))
                .and_then(|s| s.as_str());
            if body.get(4..) == Some(b"phone") {
                decoded["source"] = serde_json::json!("phone");
            } else if decoded.get("source").is_none() {
                if let Some(src) = existing_source {
                    decoded["source"] = serde_json::json!(src);
                }
            }
        }
        return serde_json::to_string(&decoded).ok();
    }

    if existing_val.is_some() {
        return existing_decoded_json.map(ToOwned::to_owned);
    }
    None
}

/// A SQLite-backed store for ring data.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) a database at `path` and ensure the schema.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let conn = Connection::open(path)?;
        // Health data + device identifiers are sensitive; keep the DB owner-only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| crate::error::Error::Storage(e.to_string()))?;
        }
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        if mode != "wal" {
            return Err(crate::error::Error::Storage(format!(
                "WAL unavailable: {mode}"
            )));
        }
        conn.execute_batch("PRAGMA synchronous=FULL;")?;
        conn.execute_batch(SCHEMA)?;
        let store = Self { conn };
        store.ensure_decoder_version()?;
        Ok(store)
    }

    /// Upgrade historical event decodes in-place if `path` is a writable store
    /// with an `events` table and an outdated decoder version or interrupted migration.
    pub fn migrate_if_writable<P: AsRef<Path>>(path: P) -> Result<bool> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(false);
        }
        let Ok(conn) = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            return Ok(false);
        };
        let _ = conn.busy_timeout(std::time::Duration::from_millis(5000));
        let has_events: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='events'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !has_events {
            return Ok(false);
        }
        let store = Self { conn };
        if store.needs_decoder_migration()? {
            store.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
            )?;
            store.ensure_decoder_version()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Read without changing schema, permissions, or journal mode (including bundled seeds).
    pub fn open_read_only<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        Ok(Self { conn })
    }

    /// Commit a complete protocol batch and its cursor together, before ACK/progress.
    pub fn commit_batch(&self, serial: &str, events: &[RingEvent], cursor: u32) -> Result<u32> {
        let tx = self.conn.unchecked_transaction()?;
        let mut inserted = 0;
        for event in events {
            inserted += u32::from(self.insert_event(serial, event)?);
        }
        self.set_cursor(serial, cursor)?;
        tx.commit()?;
        Ok(inserted)
    }

    /// Write a self-contained copy of the database (WAL folded in) to `out_path`,
    /// for sharing a phone's raw ring records with the desktop tooling. Any file
    /// at `out_path` is replaced.
    pub fn export_to<P: AsRef<Path>>(&self, out_path: P) -> Result<()> {
        let out = out_path.as_ref();
        if out.exists() {
            std::fs::remove_file(out).map_err(|e| {
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                    Some(format!("remove {}: {e}", out.display())),
                )
            })?;
        }
        // Copy pages, including committed WAL content, without rebuilding every
        // table and index as VACUUM INTO does. One step runs inside one read
        // transaction, so a concurrent writer (a sync) cannot tear the copy: it
        // holds exactly what was committed when the step began.
        let mut destination = Connection::open(out)?;
        {
            let backup = rusqlite::backup::Backup::new(&self.conn, &mut destination)?;
            match backup.step(-1)? {
                rusqlite::backup::StepResult::Done => {}
                _ => {
                    return Err(crate::error::Error::Storage(
                        "database busy during export".into(),
                    ))
                }
            }
        }
        // A WAL source may copy its journal mode too. Make the shared file standalone.
        destination.execute_batch("PRAGMA journal_mode=DELETE;")?;
        Ok(())
    }

    pub fn integrity_check(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("PRAGMA quick_check", [], |r| r.get(0))?)
    }

    /// Open an in-memory database (useful for tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        let store = Self { conn };
        store.ensure_decoder_version()?;
        Ok(store)
    }

    /// Record/refresh device metadata.
    pub fn upsert_device(
        &self,
        serial: &str,
        hardware_id: Option<&str>,
        info: Option<&DeviceInfo>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO device (serial, hardware_id, firmware, api_version, mac, updated_unix)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(serial) DO UPDATE SET
               hardware_id=COALESCE(excluded.hardware_id, device.hardware_id),
               firmware=COALESCE(excluded.firmware, device.firmware),
               api_version=COALESCE(excluded.api_version, device.api_version),
               mac=COALESCE(excluded.mac, device.mac),
               updated_unix=excluded.updated_unix",
            params![
                serial,
                hardware_id,
                info.map(|i| i.firmware_version.clone()),
                info.map(|i| i.api_version.clone()),
                info.map(|i| i.mac.clone()),
                now_unix(),
            ],
        )?;
        Ok(())
    }

    /// Device identity + last-sync for display: the most-recently-updated device
    /// row joined with its sync state.
    /// Returns `(serial, hardware_id, firmware, api_version, mac, updated_unix, last_sync_unix, next_cursor)`.
    #[allow(clippy::type_complexity)]
    pub fn device_info(
        &self,
    ) -> Result<Option<(String, String, String, String, String, i64, i64, i64)>> {
        let row = self
            .conn
            .query_row(
                "SELECT d.serial, COALESCE(d.hardware_id,''), COALESCE(d.firmware,''),
                        COALESCE(d.api_version,''), COALESCE(d.mac,''), COALESCE(d.updated_unix,0),
                        COALESCE(s.last_sync_unix,0), COALESCE(s.next_cursor,0)
                 FROM device d LEFT JOIN sync_state s ON s.serial = d.serial
                 ORDER BY d.updated_unix DESC LIMIT 1",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?,
                        r.get::<_, i64>(7)?,
                    ))
                },
            )
            .optional()?;
        Ok(row)
    }

    /// The persisted incremental-sync cursor (deciseconds), or 0 if none.
    pub fn cursor(&self, serial: &str) -> Result<u32> {
        let v: Option<i64> = self
            .conn
            .query_row(
                "SELECT next_cursor FROM sync_state WHERE serial = ?1",
                params![serial],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.unwrap_or(0) as u32)
    }

    /// Persist the next sync cursor.
    pub fn set_cursor(&self, serial: &str, cursor: u32) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_state (serial, next_cursor, last_sync_unix)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(serial) DO UPDATE SET
               next_cursor=excluded.next_cursor,
               last_sync_unix=excluded.last_sync_unix",
            params![serial, cursor as i64, now_unix()],
        )?;
        Ok(())
    }

    /// Insert an event, ignoring exact duplicates. Returns true if a row was added.
    pub fn insert_event(&self, serial: &str, ev: &RingEvent) -> Result<bool> {
        let decoded = ev
            .decoded
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default());
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO events
               (serial, tag, name, ring_timestamp, body, decoded_json, captured_unix)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                serial,
                ev.tag as i64,
                ev.name,
                ev.timestamp as i64,
                ev.body,
                decoded,
                now_unix(),
            ],
        )?;
        Ok(changed > 0)
    }

    /// Record a scalar reading (e.g. live HR bpm, SpO2 %, battery %).
    pub fn insert_reading(&self, serial: &str, kind: &str, value: f64, unit: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO readings (serial, kind, value, unit, captured_unix)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![serial, kind, value, unit, now_unix()],
        )?;
        Ok(())
    }

    /// Convenience: store a battery reading.
    pub fn insert_battery(&self, serial: &str, battery: &Battery) -> Result<()> {
        self.insert_reading(serial, "battery_percent", battery.percent as f64, "%")
    }

    fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let has_meta: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='store_meta'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !has_meta {
            return Ok(None);
        }
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM store_meta WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
        conn.execute(
            "INSERT INTO store_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn delete_meta(conn: &Connection, key: &str) -> Result<()> {
        conn.execute("DELETE FROM store_meta WHERE key = ?1", params![key])?;
        Ok(())
    }

    /// Return the persisted decoder schema version (0 if not yet migrated).
    pub fn decoder_version(&self) -> Result<i64> {
        Ok(self
            .get_meta("decoder_version")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0))
    }

    fn needs_decoder_migration(&self) -> Result<bool> {
        if self.get_meta("redecode_cursor_id")?.is_some() {
            return Ok(true);
        }
        Ok(self.decoder_version()? < DECODER_VERSION)
    }

    fn ensure_decoder_version(&self) -> Result<()> {
        if !self.needs_decoder_migration()? {
            return Ok(());
        }
        self.redecode_batched(REDECODE_BATCH_SIZE)?;
        Ok(())
    }

    /// Re-decode every stored event body with the current decoders in bounded
    /// transactional batches, updating `decoded_json` and `name`.
    /// Returns `(rows_with_decode, total_rows)`.
    pub fn redecode(&self) -> Result<(usize, usize)> {
        self.redecode_batched(REDECODE_BATCH_SIZE)
    }

    /// Paginated, bounded-memory, resumable re-decode with `batch_size` rows per
    /// SQLite transaction. Preserves synthetic phone-anchor provenance and existing
    /// valid JSON when decoding is unsupported or fails.
    pub fn redecode_batched(&self, batch_size: usize) -> Result<(usize, usize)> {
        let batch_size = batch_size.max(1);
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;

        let mut last_id: i64 = self
            .get_meta("redecode_cursor_id")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let mut decoded_count: usize = if last_id > 0 {
            self.get_meta("redecode_decoded_count")?
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0)
        } else {
            0
        };
        let mut total: usize = if last_id > 0 {
            self.get_meta("redecode_total_count")?
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0)
        } else {
            0
        };

        loop {
            let batch: Vec<(i64, i64, String, Vec<u8>, Option<String>)> = {
                let mut stmt = self.conn.prepare(
                    "SELECT id, tag, name, body, decoded_json
                     FROM events
                     WHERE id > ?1
                     ORDER BY id ASC
                     LIMIT ?2",
                )?;
                let rows = stmt
                    .query_map(params![last_id, batch_size as i64], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            };

            if batch.is_empty() {
                break;
            }

            let tx = self.conn.unchecked_transaction()?;
            {
                let mut update_stmt = tx.prepare_cached(
                    "UPDATE events SET decoded_json = ?1, name = ?2 WHERE id = ?3",
                )?;
                for (id, tag, existing_name, body, existing_decoded) in batch {
                    let tag_u8 = u8::try_from(tag).unwrap_or(0);
                    let decoded =
                        decode_stored_event(tag_u8, &body, existing_decoded.as_deref());
                    if decoded.is_some() {
                        decoded_count += 1;
                    }
                    total += 1;
                    let name = oura_protocol::events::event_name(tag_u8);
                    if decoded.as_deref() != existing_decoded.as_deref() || name != existing_name {
                        update_stmt.execute(params![decoded, name, id])?;
                    }
                    last_id = id;
                }
            }
            Self::set_meta(&tx, "redecode_cursor_id", &last_id.to_string())?;
            Self::set_meta(&tx, "redecode_decoded_count", &decoded_count.to_string())?;
            Self::set_meta(&tx, "redecode_total_count", &total.to_string())?;
            tx.commit()?;
        }

        let tx = self.conn.unchecked_transaction()?;
        Self::set_meta(&tx, "decoder_version", &DECODER_VERSION.to_string())?;
        Self::delete_meta(&tx, "redecode_cursor_id")?;
        Self::delete_meta(&tx, "redecode_decoded_count")?;
        Self::delete_meta(&tx, "redecode_total_count")?;
        tx.commit()?;

        Ok((decoded_count, total))
    }

    /// All decoded events as `(ring_timestamp_deciseconds, tag, decoded_json,
    /// captured_unix)`, ordered by capture order. Also surfaces valid raw
    /// `ring_start` (`0x41`) reboot records even when `decoded_json` is NULL on an
    /// unmigrated read-only database so reboot boundaries are never hidden.
    pub fn decoded_events(&self) -> Result<Vec<(i64, u8, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT ring_timestamp, tag, decoded_json, captured_unix, \
                    CASE WHEN decoded_json IS NULL THEN body ELSE NULL END \
             FROM events \
             WHERE decoded_json IS NOT NULL OR (tag = 65 AND LENGTH(body) >= 14) \
             ORDER BY captured_unix, id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                let ts = r.get::<_, i64>(0)?;
                let tag = r.get::<_, i64>(1)? as u8;
                let decoded_opt = r.get::<_, Option<String>>(2)?;
                let captured = r.get::<_, i64>(3)?;
                let json = match decoded_opt {
                    Some(s) => s,
                    None => {
                        let body = r.get::<_, Option<Vec<u8>>>(4)?.unwrap_or_default();
                        decode_stored_event(tag, &body, None).unwrap_or_else(|| "{}".to_string())
                    }
                };
                Ok((ts, tag, json, captured))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Count `(raw_events, decoded_events)` across the store.
    pub fn event_totals(&self) -> Result<(usize, usize)> {
        let (raw, decoded): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COUNT(decoded_json) FROM events",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((raw as usize, decoded as usize))
    }

    /// Count `(raw_events, decoded_events)` for a specific device serial.
    pub fn serial_event_totals(&self, serial: &str) -> Result<(usize, usize)> {
        let (raw, decoded): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COUNT(decoded_json) FROM events WHERE serial = ?1",
            params![serial],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((raw as usize, decoded as usize))
    }

    /// Distinct device serials that have stored events.
    pub fn device_serials(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT serial FROM events ORDER BY serial")?;
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Count stored events grouped by event name (descending).
    pub fn event_counts(&self, serial: &str) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, COUNT(*) FROM events WHERE serial = ?1 GROUP BY name ORDER BY 2 DESC",
        )?;
        let rows = stmt
            .query_map(params![serial], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event() -> RingEvent {
        RingEvent {
            tag: 0x43,
            name: "debug_event",
            timestamp: 42,
            body: vec![1, 2, 3],
            decoded: None,
        }
    }

    #[test]
    fn export_runs_beside_an_open_write_transaction() {
        let dir = std::env::temp_dir().join(format!("oura-store-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (src, dst) = (dir.join("oura.db"), dir.join("copy.db"));
        let _ = std::fs::remove_file(&src);
        let writer = Store::open(&src).unwrap();
        writer.insert_event("S1", &sample_event()).unwrap();
        // A sync mid-batch: uncommitted rows behind an open write transaction.
        writer.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let mut pending = sample_event();
        pending.timestamp = 43;
        writer.insert_event("S1", &pending).unwrap();

        Store::open_read_only(&src).unwrap().export_to(&dst).unwrap();
        writer.conn.execute_batch("COMMIT").unwrap();

        let copy = Store::open_read_only(&dst).unwrap();
        assert_eq!(copy.integrity_check().unwrap(), "ok");
        let rows: i64 = copy
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "the copy holds what was committed, not the open batch");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_database_keeps_the_previous_checkpoint() {
        let store = Store::open_in_memory().unwrap();
        store.set_cursor("S1", 7).unwrap();
        let pages: u32 = store
            .conn
            .query_row("PRAGMA page_count", [], |r| r.get(0))
            .unwrap();
        store
            .conn
            .execute_batch(&format!("PRAGMA max_page_count={pages};"))
            .unwrap();
        let mut event = sample_event();
        event.body = vec![42; 1024 * 1024];
        let error = store.commit_batch("S1", &[event], 43).unwrap_err();
        assert!(matches!(
            error,
            crate::error::Error::Sqlite { code: 13, .. }
        ));
        assert_eq!(store.cursor("S1").unwrap(), 7);
        assert!(store.event_counts("S1").unwrap().is_empty());
    }

    #[test]
    fn failed_cursor_commit_rolls_back_entire_batch() {
        let store = Store::open_in_memory().unwrap();
        store.set_cursor("S1", 7).unwrap();
        store.conn.execute_batch("CREATE TRIGGER fail_cursor BEFORE UPDATE ON sync_state BEGIN SELECT RAISE(ABORT, 'injected cursor failure'); END;").unwrap();
        let error = store.commit_batch("S1", &[sample_event()], 43).unwrap_err();
        assert!(matches!(
            error,
            crate::error::Error::Sqlite { code: 19, .. }
        ));
        assert_eq!(store.cursor("S1").unwrap(), 7);
        assert!(store.event_counts("S1").unwrap().is_empty());
        store
            .conn
            .execute_batch("DROP TRIGGER fail_cursor;")
            .unwrap();
        assert_eq!(store.commit_batch("S1", &[sample_event()], 43).unwrap(), 1);
        assert_eq!(store.commit_batch("S1", &[sample_event()], 43).unwrap(), 0);
        assert_eq!(store.cursor("S1").unwrap(), 43);
    }

    #[test]
    fn failed_insert_rolls_back_earlier_rows_and_cursor() {
        let store = Store::open_in_memory().unwrap();
        store.conn.execute_batch("CREATE TRIGGER fail_row BEFORE INSERT ON events WHEN NEW.ring_timestamp=99 BEGIN SELECT RAISE(ABORT, 'injected insert failure'); END;").unwrap();
        let mut bad = sample_event();
        bad.timestamp = 99;
        assert!(store
            .commit_batch("S1", &[sample_event(), bad], 100)
            .is_err());
        assert!(store.event_counts("S1").unwrap().is_empty());
        assert_eq!(store.cursor("S1").unwrap(), 0);
    }

    #[test]
    fn read_only_open_does_not_initialize_schema_or_create_file() {
        let path = std::env::temp_dir().join(format!("oura-missing-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(Store::open_read_only(&path).is_err());
        assert!(!path.exists());
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE sentinel(value);").unwrap();
        }
        let reader = Store::open_read_only(&path).unwrap();
        assert!(reader.event_counts("S1").is_err());
        assert_eq!(reader.integrity_check().unwrap(), "ok");
        drop(reader);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn open_enables_wal_on_writable_file() {
        let dir = std::env::temp_dir().join(format!("oura-store-wal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wal.db");
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        let mode: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reader_survives_open_writer_transaction() {
        let dir = std::env::temp_dir().join(format!("oura-store-rw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shared.db");
        let _ = std::fs::remove_file(&path);
        let writer = Store::open(&path).unwrap();
        writer.insert_event("S1", &sample_event()).unwrap();
        // Hold an uncommitted write open — under WAL a reader still gets a
        // consistent snapshot instead of SQLITE_BUSY / a partial read.
        writer.conn.execute_batch("BEGIN IMMEDIATE;").unwrap();
        writer
            .conn
            .execute(
                "INSERT INTO readings (serial, kind, value, unit, captured_unix)
                 VALUES ('S1', 'battery_percent', 50.0, '%', 0)",
                [],
            )
            .unwrap();
        let reader = Store::open_read_only(&path).unwrap();
        let counts = reader.event_counts("S1").unwrap();
        assert_eq!(counts, vec![("debug_event".to_string(), 1)]);
        writer.conn.execute_batch("COMMIT;").unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn events_dedup_and_cursor_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        let ev = RingEvent {
            tag: 0x43,
            name: "debug_event",
            timestamp: 42,
            body: vec![1, 2, 3],
            decoded: None,
        };
        assert!(store.insert_event("S1", &ev).unwrap());
        assert!(!store.insert_event("S1", &ev).unwrap()); // duplicate ignored

        store.set_cursor("S1", 1234).unwrap();
        assert_eq!(store.cursor("S1").unwrap(), 1234);

        let counts = store.event_counts("S1").unwrap();
        assert_eq!(counts, vec![("debug_event".to_string(), 1)]);
    }

    #[test]
    fn decoded_events_preserve_capture_order_across_clock_reset() {
        let store = Store::open_in_memory().unwrap();
        for timestamp in [5_000_000, 10] {
            let event = RingEvent {
                tag: 0x42,
                name: "time_sync",
                timestamp,
                body: vec![0, 0, 0, 0],
                decoded: Some(serde_json::json!({"unix_time": 1_700_000_000})),
            };
            assert!(store.insert_event("S1", &event).unwrap());
        }
        let timestamps: Vec<i64> = store
            .decoded_events()
            .unwrap()
            .into_iter()
            .map(|row| row.0)
            .collect();
        assert_eq!(timestamps, [5_000_000, 10]);
    }

    #[test]
    fn historical_raw_ring_start_visible_and_migrated_idempotently() {
        let dir = std::env::temp_dir().join(format!("oura-store-mig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("legacy.db");
        let _ = std::fs::remove_file(&path);

        // Simulate a legacy DB before `decode_ring_start` and `store_meta` existed.
        let raw_samples: &[(i64, &[u8], i64)] = &[
            (20_715, &hex_bytes("040000003A020103010001020100"), 1_780_000_001),
            (7_307_868, &hex_bytes("0400000038020103010001020100"), 1_780_000_002),
            (21_989_329, &hex_bytes("0400000038020103010001020100"), 1_780_000_003),
            (30_662_842, &hex_bytes("0400000038020103010001020100"), 1_780_000_004),
            (34_445_201, &hex_bytes("0400000038020114010001020100"), 1_780_000_005),
            (40_127_511, &hex_bytes("0130000038020114010001020100"), 1_780_000_006),
            (49_912_254, &hex_bytes("0400000038020114010001020100"), 1_780_000_007),
        ];
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute("DELETE FROM store_meta", []).unwrap();
            conn.execute(
                "INSERT INTO sync_state (serial, next_cursor, last_sync_unix) VALUES ('S1', 64519345, 1789200000)",
                [],
            )
            .unwrap();
            for (ts, body, cap) in raw_samples {
                conn.execute(
                    "INSERT INTO events (serial, tag, name, ring_timestamp, body, decoded_json, captured_unix)
                     VALUES ('S1', 65, 'ring_start', ?1, ?2, NULL, ?3)",
                    params![ts, body, cap],
                )
                .unwrap();
            }
        }

        // Even before migration runs, a read-only open surfaces all 7 raw `ring_start` rows.
        let ro = Store::open_read_only(&path).unwrap();
        assert_eq!(ro.event_totals().unwrap(), (7, 0));
        let pre_events = ro.decoded_events().unwrap();
        assert_eq!(pre_events.len(), 7);
        assert!(pre_events.iter().all(|(_, tag, json, _)| *tag == 0x41 && json.contains("firmware_version")));
        drop(ro);

        // Opening writable runs the versioned migration once and populates `decoded_json`.
        assert!(Store::migrate_if_writable(&path).unwrap());
        assert!(!Store::migrate_if_writable(&path).unwrap()); // idempotent

        let store = Store::open(&path).unwrap();
        assert_eq!(store.decoder_version().unwrap(), DECODER_VERSION);
        assert_eq!(store.event_totals().unwrap(), (7, 7));
        assert_eq!(store.cursor("S1").unwrap(), 64_519_345);

        // Verify capture timestamps and raw bodies were preserved verbatim.
        for (i, (want_ts, want_body, want_cap)) in raw_samples.iter().enumerate() {
            let (ts, body, cap): (i64, Vec<u8>, i64) = store
                .conn
                .query_row(
                    "SELECT ring_timestamp, body, captured_unix FROM events WHERE id = ?1",
                    params![(i + 1) as i64],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(ts, *want_ts);
            assert_eq!(&body[..], *want_body);
            assert_eq!(cap, *want_cap);
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn redecode_preserves_phone_anchor_provenance_and_resumes_after_interruption() {
        let store = Store::open_in_memory().unwrap();

        // Row 1: synthetic phone anchor with `<u32 LE>phone` body and `source="phone"`.
        let mut phone_body = 1_787_733_221u32.to_le_bytes().to_vec();
        phone_body.extend_from_slice(b"phone");
        store
            .conn
            .execute(
                "INSERT INTO events (serial, tag, name, ring_timestamp, body, decoded_json, captured_unix)
                 VALUES ('S1', 66, 'time_sync', 47893458, ?1, '{\"unix_time\":1787733221,\"source\":\"phone\"}', 100)",
                params![phone_body],
            )
            .unwrap();

        // Row 2: historical undecoded `ring_start`.
        let ring_start_body = hex_bytes("0400000038020114010001020100");
        store
            .conn
            .execute(
                "INSERT INTO events (serial, tag, name, ring_timestamp, body, decoded_json, captured_unix)
                 VALUES ('S1', 65, 'unknown', 49912254, ?1, NULL, 101)",
                params![ring_start_body],
            )
            .unwrap();

        // Row 3: unsupported tag (0x5f raw_acm_event) that already has valid custom JSON.
        store
            .conn
            .execute(
                "INSERT INTO events (serial, tag, name, ring_timestamp, body, decoded_json, captured_unix)
                 VALUES ('S1', 95, 'raw_acm_event', 50000000, X'01020304', '{\"preserved\":true}', 102)",
                [],
            )
            .unwrap();

        // Row 4: unsupported tag with NULL JSON.
        store
            .conn
            .execute(
                "INSERT INTO events (serial, tag, name, ring_timestamp, body, decoded_json, captured_unix)
                 VALUES ('S1', 95, 'raw_acm_event', 50000010, X'05060708', NULL, 103)",
                [],
            )
            .unwrap();

        // Row 5: another historical undecoded `ring_start` that will trigger a simulated interruption.
        store
            .conn
            .execute(
                "INSERT INTO events (serial, tag, name, ring_timestamp, body, decoded_json, captured_unix)
                 VALUES ('S1', 65, 'ring_start', 57660709, ?1, NULL, 104)",
                params![ring_start_body],
            )
            .unwrap();

        // Inject failure on row 5 (batch 3 when batch_size = 2).
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER fail_on_row_5 BEFORE UPDATE ON events WHEN OLD.id = 5
                 BEGIN SELECT RAISE(ABORT, 'simulated mid-migration interruption'); END;",
            )
            .unwrap();

        assert!(store.redecode_batched(2).is_err());

        // Batch 1 (rows 1..=2) and Batch 2 (rows 3..=4) committed; checkpoint is at id = 4.
        assert_eq!(store.get_meta("redecode_cursor_id").unwrap().as_deref(), Some("4"));
        // Row 2 was decoded in batch 1; row 5 is still NULL because batch 3 rolled back.
        let row2_json: Option<String> = store
            .conn
            .query_row("SELECT decoded_json FROM events WHERE id = 2", [], |r| r.get(0))
            .unwrap();
        let row5_json: Option<String> = store
            .conn
            .query_row("SELECT decoded_json FROM events WHERE id = 5", [], |r| r.get(0))
            .unwrap();
        assert!(row2_json.unwrap().contains("2.1.20"));
        assert!(row5_json.is_none());

        // Forbid any updates to already-checkpointed rows (id <= 4) to prove resumption skips them.
        store
            .conn
            .execute_batch(
                "DROP TRIGGER fail_on_row_5;
                 CREATE TRIGGER forbid_early_rows BEFORE UPDATE ON events WHEN OLD.id <= 4
                 BEGIN SELECT RAISE(ABORT, 're-updated already checkpointed batch'); END;",
            )
            .unwrap();

        let (decoded_count, total) = store.redecode_batched(2).unwrap();
        assert_eq!((decoded_count, total), (4, 5));
        assert!(store.get_meta("redecode_cursor_id").unwrap().is_none());
        assert_eq!(store.decoder_version().unwrap(), DECODER_VERSION);

        // Verify phone anchor kept `source="phone"` and unsupported row 3 kept its valid JSON.
        let row1_json: String = store
            .conn
            .query_row("SELECT decoded_json FROM events WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        let row1_val: serde_json::Value = serde_json::from_str(&row1_json).unwrap();
        assert_eq!(row1_val["unix_time"].as_u64(), Some(1_787_733_221));
        assert_eq!(row1_val["source"].as_str(), Some("phone"));

        let row3_json: String = store
            .conn
            .query_row("SELECT decoded_json FROM events WHERE id = 3", [], |r| r.get(0))
            .unwrap();
        assert_eq!(row3_json, "{\"preserved\":true}");
        assert_eq!(store.event_totals().unwrap(), (5, 4));
    }

    fn hex_bytes(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
}
