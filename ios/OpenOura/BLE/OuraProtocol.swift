import CoreBluetooth
import Foundation

/// GATT UUIDs and the `tag | len | payload` packet framing + request builders,
/// mirroring `crates/oura-protocol`. Little-endian throughout; extended ops ride
/// outer tag 0x2f with the first payload byte as the extended op.
enum OuraGATT {
    static let service = CBUUID(string: "98ED0001-A541-11E4-B6A0-0002A5D5C51B")
    static let notify  = CBUUID(string: "98ED0003-A541-11E4-B6A0-0002A5D5C51B")
    static let write   = CBUUID(string: "98ED0002-A541-11E4-B6A0-0002A5D5C51B")
}

enum Feature {
    static let daytimeHR: UInt8 = 0x02
    static let exerciseHR: UInt8 = 0x03
    static let spo2: UInt8 = 0x04
    static let restingHR: UInt8 = 0x08
}

enum FeatureMode {
    static let off: UInt8 = 0x00
    static let automatic: UInt8 = 0x01
    static let requested: UInt8 = 0x02
    static let connectedLive: UInt8 = 0x03
}

enum Realtime {
    static let acm: UInt32 = 0x20
    static let onDemand: UInt32 = 0x200
    static let acmResponseTag: UInt8 = 0x33
}

let HISTORY_EVENT_PREFIX: UInt8 = 0x41

/// A decoded protocol frame.
struct Packet {
    let tag: UInt8
    let payload: [UInt8]

    /// Parse a notification frame leniently (matches the Rust `Packet::parse`).
    static func parse(_ frame: Data) -> Packet? {
        let b = [UInt8](frame)
        guard b.count >= 2 else { return nil }
        let tag = b[0]
        let len = Int(b[1])
        let end = min(2 + len, b.count)
        return Packet(tag: tag, payload: Array(b[2..<end]))
    }

    /// Extended op tag (first payload byte) for 0x2f frames.
    var extTag: UInt8? { tag == 0x2f ? payload.first : nil }
}

/// Build the wire bytes `[tag, len, payload...]`.
private func packet(_ tag: UInt8, _ payload: [UInt8]) -> Data {
    Data([tag, UInt8(payload.count)] + payload)
}

private func le32(_ v: UInt32) -> [UInt8] { [UInt8(v & 0xff), UInt8((v >> 8) & 0xff), UInt8((v >> 16) & 0xff), UInt8((v >> 24) & 0xff)] }
private func le16(_ v: UInt16) -> [UInt8] { [UInt8(v & 0xff), UInt8((v >> 8) & 0xff)] }

/// Request builders (each returns the bytes to write).
enum Req {
    static let firmware = Data([0x08, 0x03, 0x00, 0x00, 0x00])
    static let battery = packet(0x0c, [])
    static let authNonce = Data([0x2f, 0x01, 0x2b])
    static let serial = Data([0x18, 0x03, 0x08, 0x00, 0x10])
    static let hardware = Data([0x18, 0x03, 0x18, 0x00, 0x10])
    static let realtimeOff = packet(0x06, [0, 0, 0, 0])

    static func authenticate(_ enc: Data) -> Data { packet(0x2f, [0x2d] + [UInt8](enc)) }
    /// Install a 16-byte auth key (only valid on a factory-reset ring).
    static func setAuthKey(_ key: Data) -> Data { packet(0x24, [UInt8](key)) }
    /// Factory-reset the ring (wipes its auth key + user data). Returns tag 0x1b.
    static let factoryReset = Data([0x1a, 0x00])
    static func capabilities(_ page: UInt8) -> Data { Data([0x2f, 0x02, 0x01, page]) }
    static func setNotification(_ flags: UInt8) -> Data { packet(0x1c, [flags]) }
    static func featureStatus(_ f: UInt8) -> Data { Data([0x2f, 0x02, 0x20, f]) }
    static func featureLatest(_ f: UInt8) -> Data { Data([0x2f, 0x02, 0x24, f]) }
    static func setFeatureMode(_ f: UInt8, _ mode: UInt8) -> Data { Data([0x2f, 0x03, 0x22, f, mode]) }

    static func syncTime(_ unix: UInt64, tzHalfHours: UInt8) -> Data {
        var p = [UInt8]()
        for i in 0..<8 { p.append(UInt8((unix >> (8 * UInt64(i))) & 0xff)) }
        p.append(tzHalfHours)
        return packet(0x12, p)
    }

    static func getEvent(start: UInt32, maxEvents: UInt8, flags: Int32) -> Data {
        packet(0x10, le32(start) + [maxEvents] + le32(UInt32(bitPattern: flags)))
    }

    static func setRealtime(bitmask: UInt32, minutes: UInt16, delay: UInt8) -> Data {
        packet(0x06, le32(bitmask) + le16(minutes) + [delay])
    }
}
