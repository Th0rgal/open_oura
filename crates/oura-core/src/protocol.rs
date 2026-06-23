//! Low-level protocol: GATT UUIDs, the `tag|length|payload` packet framing, and
//! request builders. All multi-byte integers are little-endian. Extended
//! operations are carried under outer tag `0x2f` with the first payload byte as
//! the extended op tag.

use uuid::Uuid;

/// Primary Oura ring GATT service (same across Ring 3/4/5).
pub const OURA_SERVICE: Uuid = Uuid::from_u128(0x98ed0001_a541_11e4_b6a0_0002a5d5c51b);
/// Read/notify characteristic — responses and async notifications arrive here.
pub const OURA_NOTIFY: Uuid = Uuid::from_u128(0x98ed0003_a541_11e4_b6a0_0002a5d5c51b);
/// Write characteristic — protocol requests are written here.
pub const OURA_WRITE: Uuid = Uuid::from_u128(0x98ed0002_a541_11e4_b6a0_0002a5d5c51b);

/// First tag value used by ring history events (`0x41`). Everything `>=` this is
/// a history-event frame rather than a command response.
pub const HISTORY_EVENT_PREFIX: u8 = 0x41;

/// Feature capability ids (extended `0x2f` feature ops).
pub mod feature {
    pub const BACKGROUND_DFU: u8 = 0x00;
    pub const RESEARCH_DATA: u8 = 0x01;
    pub const DAYTIME_HR: u8 = 0x02;
    pub const EXERCISE_HR: u8 = 0x03;
    pub const SPO2: u8 = 0x04;
    pub const RESTING_HR: u8 = 0x08;
    pub const CHARGING_CONTROL: u8 = 0x0e;
}

/// Feature modes used by `SetFeatureMode` (extended `0x22`).
pub mod feature_mode {
    pub const OFF: u8 = 0x00;
    pub const AUTOMATIC: u8 = 0x01;
    pub const REQUESTED: u8 = 0x02;
    /// Live streaming mode used for on-screen "current heart rate".
    pub const CONNECTED_LIVE: u8 = 0x03;
}

/// A decoded Oura protocol frame: a tag byte, a declared length, and the payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub tag: u8,
    pub payload: Vec<u8>,
}

impl Packet {
    /// Build a packet from a tag and payload.
    pub fn new(tag: u8, payload: Vec<u8>) -> Self {
        Self { tag, payload }
    }

    /// Encode to wire bytes: `[tag, len, payload..]`.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.payload.len() + 2);
        out.push(self.tag);
        out.push(self.payload.len() as u8);
        out.extend_from_slice(&self.payload);
        out
    }

    /// Parse a notification frame leniently. Returns `None` if too short to hold a
    /// header. If the declared length disagrees with the buffer, the remaining
    /// bytes after the header are used (rings occasionally pad frames).
    pub fn parse(frame: &[u8]) -> Option<Packet> {
        if frame.len() < 2 {
            return None;
        }
        let tag = frame[0];
        let len = frame[1] as usize;
        let payload = match frame.get(2..2 + len) {
            Some(slice) => slice.to_vec(),
            None => frame[2..].to_vec(),
        };
        Some(Packet { tag, payload })
    }

    /// The extended op tag (first payload byte) for `0x2f` frames.
    pub fn ext_tag(&self) -> Option<u8> {
        if self.tag == 0x2f {
            self.payload.first().copied()
        } else {
            None
        }
    }
}

// --- Request builders -------------------------------------------------------

/// Get firmware / API / bootloader / BT-stack / MAC (`0x08`).
pub fn req_firmware() -> Vec<u8> {
    vec![0x08, 0x03, 0x00, 0x00, 0x00]
}

/// Get battery level (`0x0c`).
pub fn req_battery() -> Vec<u8> {
    Packet::new(0x0c, vec![]).encode()
}

/// Request the app-auth nonce (`0x2f` ext `0x2b`).
pub fn req_auth_nonce() -> Vec<u8> {
    vec![0x2f, 0x01, 0x2b]
}

/// Authenticate with the AES-encrypted nonce (`0x2f` ext `0x2d`).
pub fn req_authenticate(encrypted: &[u8; 16]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(17);
    payload.push(0x2d);
    payload.extend_from_slice(encrypted);
    Packet::new(0x2f, payload).encode()
}

/// Install a 16-byte auth key on a factory-reset ring (`0x24`).
pub fn req_set_auth_key(key: &[u8; 16]) -> Vec<u8> {
    Packet::new(0x24, key.to_vec()).encode()
}

/// Set the ring clock to a UTC unix timestamp (`0x12`).
pub fn req_sync_time(unix_secs: u64, timezone_half_hours: u8) -> Vec<u8> {
    let mut payload = Vec::with_capacity(9);
    payload.extend_from_slice(&unix_secs.to_le_bytes());
    payload.push(timezone_half_hours);
    Packet::new(0x12, payload).encode()
}

/// Enable/disable the async notification flags (`0x1c`).
pub fn req_set_notification(flags: u8) -> Vec<u8> {
    Packet::new(0x1c, vec![flags]).encode()
}

/// Get a capabilities page (`0x2f` ext `0x01`).
pub fn req_capabilities(page: u8) -> Vec<u8> {
    vec![0x2f, 0x02, 0x01, page]
}

/// Product-info request slots (`0x18`). Each returns a `0x19` response.
pub mod product {
    /// Serial number slot (returns ASCII serial).
    pub const SERIAL: [u8; 5] = [0x18, 0x03, 0x08, 0x00, 0x10];
    /// Hardware id slot (e.g. `BLB_03`).
    pub const HARDWARE: [u8; 5] = [0x18, 0x03, 0x18, 0x00, 0x10];
    /// Product/design code slot.
    pub const CODE: [u8; 5] = [0x18, 0x03, 0x28, 0x00, 0x09];
}

/// Get up to `max_events` history events from `start` (deciseconds) (`0x10`).
///
/// `flags` is passed through verbatim; the app uses `-1` to request all types.
pub fn req_get_event(start_deciseconds: u32, max_events: u8, flags: i32) -> Vec<u8> {
    let mut payload = Vec::with_capacity(9);
    payload.extend_from_slice(&start_deciseconds.to_le_bytes());
    payload.push(max_events);
    payload.extend_from_slice(&flags.to_le_bytes());
    Packet::new(0x10, payload).encode()
}

/// Get a feature's status (`0x2f` ext `0x20`).
pub fn req_feature_status(feature: u8) -> Vec<u8> {
    vec![0x2f, 0x02, 0x20, feature]
}

/// Get a feature's latest values (`0x2f` ext `0x24`).
pub fn req_feature_latest(feature: u8) -> Vec<u8> {
    vec![0x2f, 0x02, 0x24, feature]
}

/// Set a feature's mode (`0x2f` ext `0x22`).
pub fn req_set_feature_mode(feature: u8, mode: u8) -> Vec<u8> {
    vec![0x2f, 0x03, 0x22, feature, mode]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_parses_roundtrip() {
        let p = Packet::new(0x2f, vec![0x2b]);
        let bytes = p.encode();
        assert_eq!(bytes, vec![0x2f, 0x01, 0x2b]);
        assert_eq!(Packet::parse(&bytes), Some(p));
    }

    #[test]
    fn firmware_request_matches_known_hex() {
        assert_eq!(hex::encode(req_firmware()), "0803000000");
    }

    #[test]
    fn get_event_matches_known_hex() {
        // start=0, max=8, flags=-1 -> 10 09 00000000 08 ffffffff
        assert_eq!(hex::encode(req_get_event(0, 8, -1)), "10090000000008ffffffff");
    }

    #[test]
    fn parse_handles_padding() {
        // declared length 1 but extra trailing byte
        let p = Packet::parse(&[0x25, 0x01, 0x00]).unwrap();
        assert_eq!(p.tag, 0x25);
        assert_eq!(p.payload, vec![0x00]);
    }
}
