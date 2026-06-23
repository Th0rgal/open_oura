//! Ring history events.
//!
//! Each event frame is `tag | length | payload`, where the payload begins with a
//! 4-byte little-endian timestamp (deciseconds) followed by an event-specific
//! body. The *body* layout is produced by the ring's native parser
//! (`libringeventparser.so`) and is NOT part of the decompiled Java, so this
//! crate stores every event body **raw and lossless** and decodes only the
//! envelope plus the handful of bodies whose format is known (debug ASCII,
//! ring-start metadata). New decoders can be added in [`decode_body`] without
//! re-syncing, because the raw bytes are always retained.

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

/// Best-effort decode of an event body. Most bodies are intentionally left raw
/// (see module docs). Returns `None` when we don't (yet) understand the layout.
fn decode_body(tag: u8, body: &[u8]) -> Option<serde_json::Value> {
    match tag {
        // debug_event / debug_data: ASCII strings (e.g. "git;ca22327", "SNH;4369").
        0x43 | 0x61 => {
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
        _ => None,
    }
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
}
