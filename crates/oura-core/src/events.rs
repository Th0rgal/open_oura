//! Ring history events.
//!
//! Each event frame is `tag | length | payload`, where the payload begins with a
//! 4-byte little-endian timestamp (deciseconds) followed by an event-specific
//! body. The *body* layout is produced by the ring's native parser
//! (`libringeventparser.so`) and is NOT part of the decompiled Java, so this
//! crate stores every event body **raw and lossless** and decodes the envelope
//! plus the bodies whose format has been recovered by correlating captured bytes
//! against the protobuf field shapes (temperatures, time-sync, state/wear text,
//! debug ASCII). New decoders can be added in [`decode_body`] without re-syncing,
//! because the raw bytes are always retained.

use serde::{Deserialize, Serialize};

use crate::protocol::Packet;

/// A single history event with its envelope decoded and body retained raw.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RingEvent {
    pub tag: u8,
    pub name: &'static str,
    /// Envelope timestamp (deciseconds), as reported by the ring.
    pub timestamp: u32,
    /// Event-specific body (payload after the 4-byte timestamp).
    pub body: Vec<u8>,
    /// Best-effort structured decode, when the body format is known.
    pub decoded: Option<serde_json::Value>,
}

impl RingEvent {
    /// Build an event from a parsed history-event packet (tag >= 0x41).
    pub fn from_packet(packet: &Packet) -> RingEvent {
        let p = &packet.payload;
        let timestamp = if p.len() >= 4 {
            u32::from_le_bytes([p[0], p[1], p[2], p[3]])
        } else {
            0
        };
        let body = if p.len() > 4 { p[4..].to_vec() } else { Vec::new() };
        let name = event_name(packet.tag);
        let decoded = decode_body(packet.tag, &body);
        RingEvent {
            tag: packet.tag,
            name,
            timestamp,
            body,
            decoded,
        }
    }
}

/// Best-effort decode of an event body. Unknown bodies are intentionally left raw
/// (see module docs). Returns `None` when we don't (yet) understand the layout.
///
/// The layouts below were recovered by correlating real captured bodies against
/// the protobuf field shapes; each is covered by a test using captured bytes.
fn decode_body(tag: u8, body: &[u8]) -> Option<serde_json::Value> {
    match tag {
        // time_sync: u32 LE unix timestamp (plus trailing timezone bytes).
        0x42 => decode_time_sync(body),
        // debug_event / debug_data: ASCII strings (e.g. "git;ca22327", "SNH;4369").
        0x43 | 0x61 => decode_ascii(body),
        // state_change / wear_event: one state byte then an ASCII description.
        0x45 | 0x53 => decode_state_text(body),
        // temp_event (7 probes), temp_period, sleep_temp_event: int16 LE centi-°C.
        0x46 | 0x69 | 0x75 => decode_temperatures(body),
        _ => None,
    }
}

fn decode_ascii(body: &[u8]) -> Option<serde_json::Value> {
    let text = String::from_utf8_lossy(body)
        .trim_end_matches('\0')
        .trim()
        .to_string();
    if text.is_empty() {
        None
    } else {
        Some(serde_json::json!({ "ascii": text }))
    }
}

fn decode_time_sync(body: &[u8]) -> Option<serde_json::Value> {
    if body.len() < 4 {
        return None;
    }
    let unix = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
    Some(serde_json::json!({ "unix_time": unix }))
}

fn decode_state_text(body: &[u8]) -> Option<serde_json::Value> {
    if body.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&body[1..])
        .trim_end_matches('\0')
        .trim()
        .to_string();
    Some(serde_json::json!({ "state": body[0], "text": text }))
}

/// Decode a body of one or more little-endian `i16` temperatures in centi-degrees
/// Celsius. Returns `None` if the length is odd or any value falls outside a
/// plausible sensor range, leaving the body stored raw rather than mis-decoded.
fn decode_temperatures(body: &[u8]) -> Option<serde_json::Value> {
    if body.is_empty() || body.len() % 2 != 0 {
        return None;
    }
    let mut temps = Vec::with_capacity(body.len() / 2);
    for c in body.chunks_exact(2) {
        let centi = i16::from_le_bytes([c[0], c[1]]);
        let celsius = centi as f64 / 100.0;
        if !(-40.0..=85.0).contains(&celsius) {
            return None;
        }
        temps.push((celsius * 100.0).round() / 100.0);
    }
    Some(serde_json::json!({ "temps_c": temps }))
}

/// Map an event tag to its name. Mirrors the Android app's event taxonomy.
pub fn event_name(tag: u8) -> &'static str {
    match tag {
        0x41 => "ring_start",
        0x42 => "time_sync",
        0x43 => "debug_event",
        0x44 => "ibi_event",
        0x45 => "state_change",
        0x46 => "temp_event",
        0x47 => "motion_event",
        0x48 => "sleep_period_information",
        0x49 => "sleep_summary_1",
        0x4a => "ppg_amplitude",
        0x4b => "sleep_phase_information",
        0x4c => "sleep_summary_2",
        0x4d => "ring_sleep_feature_information",
        0x4e => "sleep_phase_details",
        0x4f => "sleep_summary_3",
        0x50 => "activity_information",
        0x51 => "activity_summary_1",
        0x52 => "activity_summary_2",
        0x53 => "wear_event",
        0x54 => "recovery_summary",
        0x55 => "sleep_heart_rate",
        0x56 => "alert_event",
        0x57 => "ring_sleep_feature_information_2",
        0x58 => "sleep_summary_4",
        0x59 => "eda_event",
        0x5a => "sleep_phase_data",
        0x5b => "ble_connection",
        0x5c => "user_information",
        0x5d => "hrv_event",
        0x5e => "self_test_event",
        0x5f => "raw_acm_event",
        0x60 => "ibi_and_amplitude_event",
        0x61 => "debug_data",
        0x62 => "on_demand_meas",
        0x63 => "ppg_peak_event",
        0x64 => "raw_ppg_event",
        0x65 => "on_demand_session",
        0x66 => "on_demand_motion",
        0x67 => "raw_ppg_summary",
        0x68 => "raw_ppg_data",
        0x69 => "temp_period",
        0x6a => "sleep_period_information_2",
        0x6b => "motion_period",
        0x6c => "feature_session",
        0x6d => "meas_quality_event",
        0x6e => "spo2_ibi_and_amplitude_event",
        0x6f => "spo2_event",
        0x70 => "spo2_smoothed_event",
        0x71 => "green_ibi_and_amplitude_event",
        0x72 => "sleep_acm_period",
        0x73 => "ehr_trace_event",
        0x74 => "ehr_acm_intensity_event",
        0x75 => "sleep_temp_event",
        0x76 => "bedtime_period",
        0x77 => "spo2_dc_event",
        0x79 => "self_test_data_event",
        0x7a => "tag_event",
        0x7e => "real_step_event_feature_1",
        0x7f => "real_step_event_feature_2",
        0x81 => "cva_raw_ppg_data",
        0x82 => "scan_start",
        0x83 => "scan_end",
        _ => "unknown",
    }
}

/// Summary frame returned at the end of a `GetEvent` batch (tag `0x11`).
#[derive(Clone, Copy, Debug)]
pub struct EventBatchSummary {
    pub events_received: u8,
    pub sleep_analysis_progress: u8,
    pub bytes_left: u32,
}

impl EventBatchSummary {
    pub fn parse(packet: &Packet) -> Option<EventBatchSummary> {
        if packet.tag != 0x11 || packet.payload.len() < 6 {
            return None;
        }
        let p = &packet.payload;
        Some(EventBatchSummary {
            events_received: p[0],
            sleep_analysis_progress: p[1],
            bytes_left: u32::from_le_bytes([p[2], p[3], p[4], p[5]]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_batch_summary() {
        // 11 08 08 00 9e0e0000 0300 -> 8 events, 3742 bytes left
        let p = Packet::parse(&hex::decode("110808009e0e00000300").unwrap()).unwrap();
        let s = EventBatchSummary::parse(&p).unwrap();
        assert_eq!(s.events_received, 8);
        assert_eq!(s.bytes_left, 3742);
    }

    #[test]
    fn decodes_debug_ascii() {
        // tag 0x43, 4-byte ts then ASCII "git;abc"
        let mut frame = vec![0x43, 0x0b, 0x01, 0x00, 0x00, 0x00];
        frame.extend_from_slice(b"git;abc");
        let p = Packet::parse(&frame).unwrap();
        let ev = RingEvent::from_packet(&p);
        assert_eq!(ev.name, "debug_event");
        assert_eq!(ev.decoded.unwrap()["ascii"], "git;abc");
    }

    #[test]
    fn decodes_temp_event_seven_probes() {
        // Captured temp_event body: 7x int16 LE centi-degrees.
        let body = hex::decode("1c0dec0b8d0aa90e1f0dae0c9c0c").unwrap();
        let v = decode_temperatures(&body).unwrap();
        let temps = v["temps_c"].as_array().unwrap();
        assert_eq!(temps.len(), 7);
        assert_eq!(temps[0].as_f64().unwrap(), 33.56);
        assert_eq!(temps[3].as_f64().unwrap(), 37.53);
    }

    #[test]
    fn decodes_temp_period_single() {
        // Captured temp_period body: one int16 LE centi-degree value.
        let v = decode_temperatures(&hex::decode("6c0d").unwrap()).unwrap();
        assert_eq!(v["temps_c"][0].as_f64().unwrap(), 34.36);
    }

    #[test]
    fn rejects_implausible_temperatures() {
        // Garbage out of sensor range stays raw (None) rather than mis-decoding.
        assert!(decode_temperatures(&[0xff, 0x7f]).is_none());
    }

    #[test]
    fn decodes_time_sync_timestamp() {
        // Captured time_sync body: u32 LE unix time then timezone bytes.
        let v = decode_time_sync(&hex::decode("4fd2376a0000000000").unwrap()).unwrap();
        assert_eq!(v["unix_time"].as_u64().unwrap(), 1_782_043_215);
    }

    #[test]
    fn decodes_state_change_text() {
        // Captured state_change body: state byte 0x01 then ASCII "chg. stopped".
        let v = decode_state_text(&hex::decode("016368672e2073746f70706564").unwrap()).unwrap();
        assert_eq!(v["state"].as_u64().unwrap(), 1);
        assert_eq!(v["text"].as_str().unwrap(), "chg. stopped");
    }
}
