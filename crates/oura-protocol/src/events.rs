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

/// Decode an event body for a given tag. Public entry point for re-decoding
/// events already stored raw (e.g. after adding new decoders).
pub fn decode_event_body(tag: u8, body: &[u8]) -> Option<serde_json::Value> {
    decode_body(tag, body)
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
        // hrv_event: N pairs of (u8 avg HR bpm, u8 avg RMSSD ms), one per 5 min.
        0x5d => decode_hrv(body),
        // green_ibi_quality_event (Ring 5 tag 0x80): green-LED IBI + quality stream.
        0x80 => decode_green_ibi_quality(body),
        // ambient_event / eda: u16 LE samples, one per 5 min.
        0x59 => decode_u16_samples(body, "ambient"),
        // ehr_acm_intensity_event: up to 7 u16 LE intensity values.
        0x74 => decode_u16_samples(body, "intensity"),
        // activity_information: state byte + per-bin MET levels.
        0x50 => decode_activity_info(body),
        // spo2_event: one SpO2 % per sample (1 Hz).
        0x6f => decode_spo2(body),
        // sleep_phase_information / details / data: 2-bit hypnogram codes.
        0x4b | 0x4e | 0x5a => decode_sleep_phases(body),
        // motion_event: orientation + per-axis average motion + intensity.
        0x47 => decode_motion(body),
        // bedtime_period: detected sleep window (start/end ring deciseconds).
        0x76 => decode_bedtime_period(body),
        // sleep_acm_period: 6 accelerometer MAD statistics (fixed-point floats).
        0x72 => decode_sleep_acm_period(body),
        // spo2_r_pi_event (Ring 5 tag 0x8b): SpO2 R-ratio + perfusion index.
        0x8b => decode_spo2_r_pi(body),
        // ibi_and_amplitude_event: 14-byte packed 6× (IBI delta ms, amplitude).
        0x60 => decode_ibi_amplitude(body),
        // alert_event: single alert-type byte.
        0x56 => decode_first_byte(body, "alert_type"),
        // motion_period: header byte + packed 2-bit motion levels (4/byte, MSB-first).
        // From the native parser `parse_api_motion_period @ 0x3c7cd8`.
        0x6b => decode_motion_period(body),
        // feature_session: [feature_id][session_status][optional u16 value].
        // From `parse_api_feature_session @ 0x3c1de0`.
        0x6c => decode_feature_session(body),
        // BLE/radio telemetry + self-test diagnostics. Identified from the native
        // parser registration table; bodies preserved (layouts are device-internal).
        0x5b => decode_telemetry(body, "ble_connection_ind"), // parse_api_ble_connection_ind @ 0x3c6118
        0x79 => decode_telemetry(body, "self_test_data"),     // parse_api_selftest_data_event @ 0x3ca74c
        0x82 => decode_telemetry(body, "scan_start"),         // parse_api_scan_start @ 0x3cbe20
        0x83 => decode_telemetry(body, "scan_end"),           // parse_api_scan_end @ 0x3cc43c
        _ => None,
    }
}

/// Decode an `hrv_event` body: pairs of `(avg_hr_bpm, avg_rmssd_ms)`, one sample
/// per 5-minute window. Layout confirmed from the ring's native parser
/// (`parse_api_hrv_event`): each sample is two bytes, even body length.
fn decode_hrv(body: &[u8]) -> Option<serde_json::Value> {
    if body.is_empty() || !body.len().is_multiple_of(2) {
        return None;
    }
    let hr: Vec<u8> = body.iter().step_by(2).copied().collect();
    let rmssd: Vec<u8> = body.iter().skip(1).step_by(2).copied().collect();
    Some(serde_json::json!({
        "hr_bpm": hr,
        "rmssd_ms": rmssd,
        "interval_min": 5,
    }))
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
    if body.is_empty() || !body.len().is_multiple_of(2) {
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

/// `green_ibi_quality_event` (Ring 5 tag `0x80`): green-LED inter-beat intervals
/// with a quality flag. Per the native `parse_api_green_ibi_quality_event`, each
/// sample is two bytes: `ibi_ms = (b1 & 7) | (b0 << 3)`, `quality = (b1>>3)&3`,
/// `flag = b1>>5`. We also surface heart rate from good-quality, plausible beats.
fn decode_green_ibi_quality(body: &[u8]) -> Option<serde_json::Value> {
    if body.len() < 2 {
        return None;
    }
    let mut ibi_ms = Vec::new();
    let mut quality = Vec::new();
    let mut hr_bpm = Vec::new();
    for p in body.chunks_exact(2) {
        let ibi = ((p[1] & 0x07) as u16) | ((p[0] as u16) << 3);
        let q = (p[1] >> 3) & 0x03;
        if q == 1 && (300..=2000).contains(&ibi) {
            hr_bpm.push(60_000u32 / ibi as u32);
        }
        ibi_ms.push(ibi);
        quality.push(q);
    }
    Some(serde_json::json!({ "ibi_ms": ibi_ms, "quality": quality, "hr_bpm": hr_bpm }))
}

/// A body of little-endian `u16` samples under a single key (ambient, intensity).
fn decode_u16_samples(body: &[u8], key: &str) -> Option<serde_json::Value> {
    if body.is_empty() || !body.len().is_multiple_of(2) {
        return None;
    }
    let v: Vec<u16> = body
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Some(serde_json::json!({ key: v }))
}

/// `activity_information`: a state byte followed by per-bin MET levels. The native
/// `parse_api_activity_info_event` scales each byte `b<128 -> b*0.1`, else
/// `12.8 + (b-128)*0.2` MET.
fn decode_activity_info(body: &[u8]) -> Option<serde_json::Value> {
    let (&state, mets) = body.split_first()?;
    let met: Vec<f64> = mets
        .iter()
        .map(|&b| {
            let m = if b < 0x80 {
                b as f64 * 0.1
            } else {
                12.8 + (b as f64 - 128.0) * 0.2
            };
            (m * 100.0).round() / 100.0
        })
        .collect();
    Some(serde_json::json!({ "state": state, "met": met }))
}

/// `spo2_event`: a header byte then one SpO2 % per sample (1 Hz). A trailing
/// `0xff` is a "continued" sentinel, not a sample.
fn decode_spo2(body: &[u8]) -> Option<serde_json::Value> {
    if body.len() < 2 {
        return None;
    }
    let mut end = body.len();
    if body[end - 1] == 0xff {
        end -= 1;
    }
    let spo2: Vec<u8> = body[1..end].to_vec();
    if spo2.is_empty() {
        return None;
    }
    Some(serde_json::json!({ "spo2_percent": spo2 }))
}

/// Sleep-stage hypnogram: a header byte then 2-bit phase codes (4 per byte,
/// MSB-first). Enum from the native `SleepPhase_OSSAv1`.
fn decode_sleep_phases(body: &[u8]) -> Option<serde_json::Value> {
    const PHASE: [&str; 4] = ["deep", "light", "rem", "awake"];
    if body.len() < 2 {
        return None;
    }
    let mut phases = Vec::new();
    for &b in &body[1..] {
        for shift in [6u8, 4, 2, 0] {
            phases.push(PHASE[((b >> shift) & 0x03) as usize]);
        }
    }
    Some(serde_json::json!({ "header": body[0], "phases": phases }))
}

/// `ibi_and_amplitude_event` (tag `0x60`): a fixed 14-byte packet holding 6
/// inter-beat intervals (ms) and PPG amplitudes, bit-packed per the native
/// `parse_api_ibi_and_amplitude_event`. Layout ported from the decompiled bit
/// extraction; pending validation against real `0x60` captures.
fn decode_ibi_amplitude(body: &[u8]) -> Option<serde_json::Value> {
    if body.len() != 14 {
        return None;
    }
    let b = body;
    let ibi_ms = [
        ((b[6] & 1) as u16) | ((b[0] as u16) << 3) | ((b[12] >> 5) & 6) as u16,
        ((b[7] & 1) as u16) | ((b[1] as u16) << 3) | ((b[12] >> 3) & 6) as u16,
        ((b[8] & 1) as u16) | ((b[2] as u16) << 3) | ((b[12] >> 1) & 6) as u16,
        ((b[9] & 1) as u16) | ((b[3] as u16) << 3) | (((b[12] & 3) << 1) as u16),
        ((b[10] & 1) as u16) | ((b[4] as u16) << 3) | ((b[13] >> 5) & 6) as u16,
        ((b[11] & 1) as u16) | ((b[5] as u16) << 3) | ((b[13] >> 3) & 6) as u16,
    ];
    let shift = if (b[13] & 0x0f) == 7 { 0 } else { (b[13] & 0x0f) + 1 };
    let amplitude: Vec<u32> = (0..6).map(|k| ((b[6 + k] >> 1) as u32) << shift).collect();
    // heart rate from plausible beats (validated on overnight data ~ median 41 bpm)
    let hr_bpm: Vec<u16> = ibi_ms
        .iter()
        .filter(|&&i| (300..=2000).contains(&i))
        .map(|&i| 60_000 / i)
        .collect();
    Some(serde_json::json!({ "ibi_ms": ibi_ms, "amplitude": amplitude, "hr_bpm": hr_bpm }))
}

/// `spo2_r_pi_event` (Ring 5 tag `0x8b`): a header byte then 3-byte samples of
/// `(R-ratio: u16 big-endian / 16384, perfusion index: u8/255 × 0.05)`. The R
/// ratio feeds Oura's (proprietary) SpO2 curve; we surface R and PI directly.
fn decode_spo2_r_pi(body: &[u8]) -> Option<serde_json::Value> {
    if body.len() < 4 || !(body.len() - 1).is_multiple_of(3) {
        return None;
    }
    let mut r = Vec::new();
    let mut pi = Vec::new();
    let mut o = 1;
    while o + 3 <= body.len() {
        let rv = u16::from_be_bytes([body[o], body[o + 1]]) as f64 / 16384.0;
        r.push((rv * 1000.0).round() / 1000.0);
        pi.push(((body[o + 2] as f64 / 255.0 * 0.05) * 10000.0).round() / 10000.0);
        o += 3;
    }
    Some(serde_json::json!({ "r": r, "perfusion_index": pi }))
}

/// `sleep_acm_period` (tag `0x72`): six accelerometer MAD statistics packed as
/// fixed-point — three `int + frac/255` values, then three `12-bit/4095 + nibble`
/// values, per the native `parse_api_sleep_acm_period`.
fn decode_sleep_acm_period(body: &[u8]) -> Option<serde_json::Value> {
    if body.len() < 12 {
        return None;
    }
    let r4 = |v: f64| (v * 10000.0).round() / 10000.0;
    let fp = |frac: u8, intg: u8| intg as f64 + frac as f64 / 255.0;
    let q12 = |lo: u8, hi: u8| ((lo as u16 | ((hi as u16 & 0x0f) << 8)) as f64 / 4095.0) + (hi >> 4) as f64;
    let vals = [
        r4(fp(body[0], body[1])),
        r4(fp(body[2], body[3])),
        r4(fp(body[4], body[5])),
        r4(q12(body[6], body[7])),
        r4(q12(body[8], body[9])),
        r4(q12(body[10], body[11])),
    ];
    Some(serde_json::json!({ "acm_mad": vals }))
}

/// `motion_event` (tag `0x47`): a compact per-window motion summary. From the
/// native `parse_api_motion_events`: `b0>>5` = orientation, `b0&0x1f` =
/// motion-seconds, then three signed `i8` average-axis values scaled ×8, and
/// optional low/high intensity nibbles. Field names are best-effort (axis order is
/// inferred from the struct layout).
fn decode_motion(body: &[u8]) -> Option<serde_json::Value> {
    if body.len() < 4 {
        return None;
    }
    let mut v = serde_json::json!({
        "orientation": body[0] >> 5,
        "motion_seconds": body[0] & 0x1f,
        "avg_x": (body[1] as i8 as i32) * 8,
        "avg_y": (body[2] as i8 as i32) * 8,
        "avg_z": (body[3] as i8 as i32) * 8,
    });
    if body.len() >= 5 {
        if body[4] & 0x40 != 0 {
            return None;
        }
        v["low_intensity"] = (body[4] & 0x3f).into();
    }
    if body.len() >= 6 {
        if body[5] & 0x40 != 0 {
            return None;
        }
        v["high_intensity"] = (body[5] & 0x3f).into();
    }
    Some(v)
}

/// `bedtime_period` (tag `0x76`): the ring's detected sleep window as two `u32`
/// little-endian ring timestamps (deciseconds). Produced by sleep analysis.
fn decode_bedtime_period(body: &[u8]) -> Option<serde_json::Value> {
    if body.len() < 8 {
        return None;
    }
    let start = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
    let end = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
    let hours = end.saturating_sub(start) as f64 / 10.0 / 3600.0;
    Some(serde_json::json!({
        "bedtime_start_ds": start,
        "bedtime_end_ds": end,
        "duration_hours": (hours * 100.0).round() / 100.0,
    }))
}

/// A single leading byte under a named key.
fn decode_first_byte(body: &[u8], key: &str) -> Option<serde_json::Value> {
    body.first().map(|&b| serde_json::json!({ key: b }))
}

/// Map an event tag to its name. Mirrors the Android app's event taxonomy.
/// `motion_period` (tag `0x6b`): a compact activity timeline. The header byte
/// packs `type` (bits 6-7), the sample count of the final byte (bits 4-5), and a
/// low nibble; each subsequent byte holds four 2-bit motion levels (0-3), MSB
/// first. Layout from the native parser `parse_api_motion_period @ 0x3c7cd8`.
fn decode_motion_period(body: &[u8]) -> Option<serde_json::Value> {
    if body.len() < 2 {
        return None;
    }
    let header = body[0];
    let period_type = header >> 6;
    let low_nibble = header & 0x0f;
    let last_count = ((header >> 4) & 0x03) as usize;
    let mut levels: Vec<u8> = Vec::new();
    for (i, &b) in body[1..].iter().enumerate() {
        let n = if i == body.len() - 2 { last_count } else { 4 };
        for k in 0..n {
            levels.push((b >> (6 - 2 * k)) & 0x03);
        }
    }
    Some(serde_json::json!({
        "period_type": period_type,
        "low_nibble": low_nibble,
        "motion_levels": levels,
    }))
}

/// `feature_session` (tag `0x6c`): start/stop markers for an on-ring measurement
/// feature. `feature_id` (byte 0, < 8), `session_status` (byte 1, 0-12), and an
/// optional `value` (u16 LE) for feature-specific payloads. From the native parser
/// `parse_api_feature_session @ 0x3c1de0`.
fn decode_feature_session(body: &[u8]) -> Option<serde_json::Value> {
    if body.len() < 2 {
        return None;
    }
    let mut v = serde_json::json!({
        "feature_id": body[0],
        "session_status": body[1],
    });
    if body.len() >= 4 {
        v["value"] = serde_json::json!(u16::from_le_bytes([body[2], body[3]]));
    }
    Some(v)
}

/// Identify a BLE/radio-telemetry or diagnostic event whose payload layout is
/// device-internal: tag the event by name and preserve the raw body (plus the
/// leading sub-type byte, which these events use as a discriminator).
fn decode_telemetry(body: &[u8], kind: &str) -> Option<serde_json::Value> {
    if body.is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "kind": kind,
        "subtype": body[0],
        "raw": hex::encode(body),
    }))
}

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
        0x80 => "green_ibi_quality_event",
        0x81 => "cva_raw_ppg_data",
        0x8b => "spo2_r_pi_event",
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
    fn decodes_feature_session() {
        // captured 0x6c body: feature 2, status 1, value 5
        let v = decode_feature_session(&[0x02, 0x01, 0x05, 0x00]).unwrap();
        assert_eq!(v["feature_id"], 2);
        assert_eq!(v["session_status"], 1);
        assert_eq!(v["value"], 5);
    }

    #[test]
    fn decodes_motion_period() {
        // captured 0x6b body (14 B): header 0x30 -> type 0, last byte holds 3 levels
        let body = hex::decode("30abefaa596ea89669197afffffb").unwrap();
        let v = decode_motion_period(&body).unwrap();
        assert_eq!(v["period_type"], 0);
        // 12 full bytes x4 + final byte x3 = 51 two-bit levels
        assert_eq!(v["motion_levels"].as_array().unwrap().len(), 51);
    }

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
    fn decodes_hrv_event() {
        // 3 samples: (hr=60,rmssd=40), (hr=62,rmssd=45), (hr=58,rmssd=50)
        let body = [60u8, 40, 62, 45, 58, 50];
        let v = decode_hrv(&body).unwrap();
        assert_eq!(v["hr_bpm"].as_array().unwrap().len(), 3);
        assert_eq!(v["hr_bpm"][1].as_u64().unwrap(), 62);
        assert_eq!(v["rmssd_ms"][2].as_u64().unwrap(), 50);
        assert_eq!(v["interval_min"].as_u64().unwrap(), 5);
    }

    #[test]
    fn decodes_green_ibi_quality_real_bytes() {
        // Captured Ring 5 0x80 body: resting beats ~47-50 bpm at quality 1.
        let body = hex::decode("9d09940b9d0d9a099a09a62e946e").unwrap();
        let v = decode_green_ibi_quality(&body).unwrap();
        let ibi = v["ibi_ms"].as_array().unwrap();
        assert_eq!(ibi.len(), 7);
        // first beat: (0x09 & 7) | (0x9d << 3) = 1 | 1256 = 1257 ms
        assert_eq!(ibi[0].as_u64().unwrap(), 1257);
        // good-quality beats yield plausible resting HR
        for hr in v["hr_bpm"].as_array().unwrap() {
            let h = hr.as_u64().unwrap();
            assert!((40..=60).contains(&h), "hr {h} out of resting range");
        }
    }

    #[test]
    fn decodes_activity_met() {
        // state=3, then bytes below/above 128
        let v = decode_activity_info(&[3, 10, 0x80, 0x90]).unwrap();
        assert_eq!(v["state"].as_u64().unwrap(), 3);
        let met = v["met"].as_array().unwrap();
        assert_eq!(met[0].as_f64().unwrap(), 1.0); // 10 * 0.1
        assert_eq!(met[1].as_f64().unwrap(), 12.8); // boundary
        assert_eq!(met[2].as_f64().unwrap(), 12.8 + 16.0 * 0.2); // 0x90-128=16
    }

    #[test]
    fn decodes_sleep_phases_codes() {
        // header byte, then one byte 0b00_01_10_11 = deep,light,rem,awake
        let v = decode_sleep_phases(&[0x00, 0b00_01_10_11]).unwrap();
        let p = v["phases"].as_array().unwrap();
        assert_eq!(p[0], "deep");
        assert_eq!(p[1], "light");
        assert_eq!(p[2], "rem");
        assert_eq!(p[3], "awake");
    }

    #[test]
    fn decodes_spo2_r_pi_real_bytes() {
        // Captured Ring 5 0x8b body: stable R-ratio ~0.78, plausible PI.
        let body = hex::decode("00321f8c323795328b9532bb95").unwrap();
        let v = decode_spo2_r_pi(&body).unwrap();
        let r = v["r"].as_array().unwrap();
        assert_eq!(r.len(), 4);
        assert_eq!(r[0].as_f64().unwrap(), 0.783);
        for x in r {
            let rv = x.as_f64().unwrap();
            assert!((0.5..1.0).contains(&rv), "R {rv} out of physiological range");
        }
    }

    #[test]
    fn decodes_sleep_acm_period_real_bytes() {
        let body = hex::decode("b1004601f0001e003e000200").unwrap();
        let v = decode_sleep_acm_period(&body).unwrap();
        let m = v["acm_mad"].as_array().unwrap();
        assert_eq!(m.len(), 6);
        assert_eq!(m[0].as_f64().unwrap(), 0.6941); // 177/255
        assert_eq!(m[1].as_f64().unwrap(), 1.2745); // 1 + 70/255
    }

    #[test]
    fn decodes_bedtime_period_real_bytes() {
        // Captured after triggering sleep analysis: ~7.28 h window.
        let v = decode_bedtime_period(&hex::decode("74376100e6366500").unwrap()).unwrap();
        assert_eq!(v["bedtime_start_ds"].as_u64().unwrap(), 6_371_188);
        assert_eq!(v["bedtime_end_ds"].as_u64().unwrap(), 6_633_190);
        assert_eq!(v["duration_hours"].as_f64().unwrap(), 7.28);
    }

    #[test]
    fn decodes_motion_event() {
        // 0x6f,0x0c,0x1d,0x07,0x0c,0x07 -> orientation 3, axes signed ×8
        let v = decode_motion(&[0x6f, 0x0c, 0x1d, 0x07, 0x0c, 0x07]).unwrap();
        assert_eq!(v["orientation"].as_u64().unwrap(), 3);
        assert_eq!(v["avg_x"].as_i64().unwrap(), 12 * 8);
        assert_eq!(v["avg_y"].as_i64().unwrap(), 29 * 8);
        assert_eq!(v["avg_z"].as_i64().unwrap(), 7 * 8);
        assert_eq!(v["high_intensity"].as_u64().unwrap(), 7);
    }

    #[test]
    fn decodes_state_change_text() {
        // Captured state_change body: state byte 0x01 then ASCII "chg. stopped".
        let v = decode_state_text(&hex::decode("016368672e2073746f70706564").unwrap()).unwrap();
        assert_eq!(v["state"].as_u64().unwrap(), 1);
        assert_eq!(v["text"].as_str().unwrap(), "chg. stopped");
    }
}
