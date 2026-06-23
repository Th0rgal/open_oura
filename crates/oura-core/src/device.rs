//! Parsers for device-info responses: firmware, battery, product/serial, and
//! capabilities. These wire formats are stable across ring generations.

use serde::{Deserialize, Serialize};

use crate::protocol::Packet;

/// Firmware / version metadata (response tag `0x09`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub api_version: String,
    pub firmware_version: String,
    pub bootloader_version: String,
    pub bt_stack_version: String,
    /// BLE MAC, colon-separated.
    pub mac: String,
}

impl DeviceInfo {
    pub fn parse(packet: &Packet) -> Option<DeviceInfo> {
        if packet.tag != 0x09 || packet.payload.len() < 18 {
            return None;
        }
        let p = &packet.payload;
        let v3 = |s: &[u8]| s.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(".");
        Some(DeviceInfo {
            api_version: v3(&p[0..3]),
            firmware_version: v3(&p[3..6]),
            bootloader_version: v3(&p[6..9]),
            bt_stack_version: v3(&p[9..12]),
            mac: p[12..18]
                .iter()
                .rev()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(":"),
        })
    }
}

/// Battery state (response tag `0x0d`). Requires app-auth on rings with a key set.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Battery {
    pub percent: u8,
    pub charging_progress: u8,
    pub charging_recommended: u8,
}

impl Battery {
    pub fn parse(packet: &Packet) -> Option<Battery> {
        if packet.tag != 0x0d || packet.payload.len() < 3 {
            return None;
        }
        Some(Battery {
            percent: packet.payload[0],
            charging_progress: packet.payload[1],
            charging_recommended: packet.payload[2],
        })
    }
}

/// A product-info response (tag `0x19`): a status byte then ASCII/bytes.
pub fn parse_product_ascii(packet: &Packet) -> Option<String> {
    if packet.tag != 0x19 || packet.payload.is_empty() {
        return None;
    }
    if packet.payload[0] != 0 {
        return None;
    }
    let text: String = String::from_utf8_lossy(&packet.payload[1..])
        .trim_end_matches('\0')
        .trim()
        .to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// A single `(feature, value)` pair from a capabilities page.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Capability {
    pub feature: u8,
    pub value: u8,
}

/// Parse a capabilities page response (`0x2f` ext `0x02`).
pub fn parse_capabilities(packet: &Packet) -> Vec<Capability> {
    if packet.ext_tag() != Some(0x02) || packet.payload.len() < 2 {
        return Vec::new();
    }
    // payload: [0]=ext 0x02, [1]=page count, then (feature, value) pairs.
    packet.payload[2..]
        .chunks_exact(2)
        .map(|c| Capability {
            feature: c[0],
            value: c[1],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ring3_firmware() {
        let frame = hex::decode("091202000003040301000105000ca56c2af838a0").unwrap();
        let p = Packet::parse(&frame).unwrap();
        let info = DeviceInfo::parse(&p).unwrap();
        assert_eq!(info.api_version, "2.0.0");
        assert_eq!(info.firmware_version, "3.4.3");
        assert_eq!(info.mac, "a0:38:f8:2a:6c:a5");
    }

    #[test]
    fn parses_battery() {
        let p = Packet::parse(&hex::decode("0d0659000001f00f").unwrap()).unwrap();
        let b = Battery::parse(&p).unwrap();
        assert_eq!(b.percent, 0x59); // 89%
    }

    #[test]
    fn parses_serial() {
        let p = Packet::parse(&hex::decode("191100324833413233343730303433").unwrap()).unwrap();
        // status 0x00 then ASCII "2H3A23470043"
        assert_eq!(parse_product_ascii(&p).as_deref(), Some("2H3A23470043"));
    }
}
