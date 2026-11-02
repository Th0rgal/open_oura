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
        let v3 = |s: &[u8]| {
            s.iter()
                .map(|b| b.to_string())
                .collect::<Vec<_>>()
                .join(".")
        };
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

/// A product code response (tag `0x19`): a status byte then the code bytes.
pub fn parse_product_code(packet: &Packet) -> Option<Vec<u8>> {
    match packet.payload.split_first() {
        Some((0, code)) if packet.tag == 0x19 && !code.is_empty() => Some(code.to_vec()),
        _ => None,
    }
}

/// Ring model, color and size decoded from the hardware id and product code.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingIdentity {
    pub model: &'static str,
    pub color: Option<&'static str>,
    pub size: Option<u8>,
}

#[derive(Clone, Copy)]
enum Hardware {
    Gen3,
    Gen4,
    Ring5,
    Proto,
    Older,
}

/// Decode a 9-byte product code: design at 0, size at 4, color at 8.
pub fn ring_identity(hardware_id: &str, product_code: &[u8]) -> Option<RingIdentity> {
    const PREFIXES: &[(&str, Hardware)] = &[
        ("GPS_", Hardware::Older),
        ("BLB_", Hardware::Gen3),
        ("KTH_", Hardware::Gen4),
        ("ORE_", Hardware::Gen4),
        ("JAD_", Hardware::Gen4),
        ("COR_", Hardware::Ring5),
        ("BEM_", Hardware::Proto),
        ("BEN_", Hardware::Proto),
        ("AST_", Hardware::Proto),
        ("FLR_", Hardware::Proto),
        ("AMR_", Hardware::Proto),
        ("PRC_", Hardware::Older),
        ("NMC_", Hardware::Older),
        ("SMB_", Hardware::Older),
    ];
    let hardware = PREFIXES
        .iter()
        .find(|(prefix, _)| hardware_id.starts_with(prefix))?
        .1;
    if product_code.len() < 9 {
        return None;
    }
    let (design_code, color_code) = (product_code[0], product_code[8]);
    let generic_model = "Oura Ring";
    let model = match hardware {
        Hardware::Gen3 => match design_code {
            1 => "Heritage",
            2 => "Balance",
            3 => "Balance Diamond",
            5 => "Gucci x Oura Ring",
            6 => "Horizon",
            8 => "Oura Ring 4",
            9 => "Oura Ring 4 Ceramic",
            10 => "Oura Ring 5",
            _ => generic_model,
        },
        Hardware::Gen4 => match design_code {
            1 => "Oura Ring 4",
            2 => "Oura Ring 4 Ceramic",
            _ => generic_model,
        },
        Hardware::Ring5 => match design_code {
            1 => "Oura Ring 5",
            _ => generic_model,
        },
        Hardware::Proto => generic_model,
        Hardware::Older if color_code == 5 => "Balance Diamond",
        Hardware::Older => match design_code {
            1 => "Balance",
            2 => "Heritage",
            _ => generic_model,
        },
    };
    let color = match hardware {
        Hardware::Gen3 => match color_code {
            1 => Some("Arctic White"),
            2 | 9 => Some("Black"),
            3 => Some("Stealth"),
            4 => Some("Rose Gold"),
            5 => Some("Silver"),
            6 => Some("Brushed Titanium"),
            7 => Some("Gold"),
            12 => Some("Brushed Silver"),
            13 => Some("Midnight"),
            14 => Some("Petal"),
            15 => Some("Tide"),
            16 => Some("Cloud"),
            17 => Some("Light Gold"),
            18 => Some("Deep Rose"),
            _ => None,
        },
        Hardware::Gen4 => match color_code {
            1 => Some("Silver"),
            2 => Some("Gold"),
            3 => Some("Rose Gold"),
            4 => Some("Black"),
            5 => Some("Stealth"),
            6 => Some("Brushed Silver"),
            7 => Some("Midnight"),
            8 => Some("Petal"),
            9 => Some("Tide"),
            10 => Some("Cloud"),
            _ => None,
        },
        Hardware::Ring5 => match color_code {
            1 => Some("Silver"),
            2 => Some("Gold"),
            3 => Some("Deep Rose"),
            4 => Some("Black"),
            5 => Some("Stealth"),
            6 => Some("Brushed Silver"),
            _ => None,
        },
        Hardware::Proto => None,
        Hardware::Older => match color_code {
            1 => Some("Black"),
            2 => Some("Stealth"),
            3 | 5 => Some("Silver"),
            4 => Some("Rose Gold"),
            6 => Some("Gold"),
            _ => None,
        },
    };
    Some(RingIdentity {
        model,
        color,
        size: (product_code[4] > 0).then_some(product_code[4]),
    })
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
        let frame = hex::decode("091202000003040301000105000cffeeddccbbaa").unwrap();
        let p = Packet::parse(&frame).unwrap();
        let info = DeviceInfo::parse(&p).unwrap();
        assert_eq!(info.api_version, "2.0.0");
        assert_eq!(info.firmware_version, "3.4.3");
        assert_eq!(info.mac, "aa:bb:cc:dd:ee:ff");
    }

    #[test]
    fn parses_battery() {
        let p = Packet::parse(&hex::decode("0d0659000001f00f").unwrap()).unwrap();
        let b = Battery::parse(&p).unwrap();
        assert_eq!(b.percent, 0x59); // 89%
    }

    #[test]
    fn parses_serial() {
        let p = Packet::parse(&hex::decode("191100585858585858585858585858").unwrap()).unwrap();
        // status 0x00 then ASCII "XXXXXXXXXXXX" (real serial scrubbed; see local/device-identifiers.md)
        assert_eq!(parse_product_ascii(&p).as_deref(), Some("XXXXXXXXXXXX"));
    }

    #[test]
    fn decodes_ring_identity() {
        let product_code = |design, size, color| [design, 0, 0, 0, size, 0, 0, 0, color];
        assert_eq!(
            ring_identity("BLB_03", &product_code(6, 9, 3)),
            Some(RingIdentity {
                model: "Horizon",
                color: Some("Stealth"),
                size: Some(9)
            })
        );
        assert_eq!(
            ring_identity("JAD_01", &product_code(2, 8, 9)).map(|r| (r.model, r.color)),
            Some(("Oura Ring 4 Ceramic", Some("Tide")))
        );
        assert_eq!(
            ring_identity("GPS_02", &product_code(1, 7, 5)).map(|r| r.model),
            Some("Balance Diamond")
        );
        assert_eq!(ring_identity("XYZ_1", &product_code(1, 9, 1)), None);
        assert_eq!(ring_identity("BLB_03", &[6, 0]), None);
    }
}
