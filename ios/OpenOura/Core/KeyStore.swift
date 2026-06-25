import Foundation
import Security

/// Stores the 16-byte ring auth key (as hex) in the iOS Keychain. The same key
/// that `oura pair` installed on the ring is used here — paste its hex once.
enum KeyStore {
    private static let service = "com.openoura.app"
    private static let account = "ring-auth-key"

    static func saveHex(_ hex: String) -> Bool {
        let clean = hex.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let data = clean.data(using: .utf8) else { return false }
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        SecItemDelete(query as CFDictionary)
        var add = query
        add[kSecValueData as String] = data
        return SecItemAdd(add as CFDictionary, nil) == errSecSuccess
    }

    static func loadHex() -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var out: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &out) == errSecSuccess,
              let data = out as? Data, let s = String(data: data, encoding: .utf8)
        else { return nil }
        return s
    }

    /// The key as 16 raw bytes, or nil if unset/invalid.
    static func keyBytes() -> Data? {
        guard let hex = loadHex() else { return nil }
        return dataFromHex(hex)
    }

    static func dataFromHex(_ hex: String) -> Data? {
        let s = hex.trimmingCharacters(in: .whitespacesAndNewlines)
        guard s.count == 32 else { return nil }
        var bytes = [UInt8]()
        var i = s.startIndex
        while i < s.endIndex {
            let j = s.index(i, offsetBy: 2)
            guard let b = UInt8(s[i..<j], radix: 16) else { return nil }
            bytes.append(b)
            i = j
        }
        return Data(bytes)
    }
}
