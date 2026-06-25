import Foundation
import OuraFFI

/// Swift wrappers over the Rust C core (auth + event decoders). All heavy
/// byte-level logic lives in Rust; this file just marshals bytes/strings.
enum OuraCore {
    /// AES-128/ECB/PKCS7 encrypt a ring auth nonce under `key` (16 bytes).
    /// Returns the 16-byte response block, or nil on bad input.
    static func encryptNonce(key: Data, nonce: Data) -> Data? {
        guard key.count == 16 else { return nil }
        var out = [UInt8](repeating: 0, count: 16)
        let rc = key.withUnsafeBytes { kp in
            nonce.withUnsafeBytes { np in
                oura_encrypt_nonce(
                    kp.bindMemory(to: UInt8.self).baseAddress, key.count,
                    np.bindMemory(to: UInt8.self).baseAddress, nonce.count,
                    &out)
            }
        }
        return rc == 0 ? Data(out) : nil
    }

    /// Decode an event body for `tag` into a parsed JSON object, or nil.
    static func decodeEvent(tag: UInt8, body: Data) -> [String: Any]? {
        let cstr: UnsafeMutablePointer<CChar>? = body.withUnsafeBytes { bp in
            oura_decode_event(tag, bp.bindMemory(to: UInt8.self).baseAddress, body.count)
        }
        guard let cstr else { return nil }
        defer { oura_string_free(cstr) }
        let json = String(cString: cstr)
        guard let data = json.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }
        return obj
    }

    /// Human-readable name for an event tag.
    static func eventName(tag: UInt8) -> String {
        guard let cstr = oura_event_name(tag) else { return "tag_\(tag)" }
        defer { oura_string_free(cstr) }
        return String(cString: cstr)
    }
}
