import Foundation
import Security

// On-device BLE sync: connect to the ring over CoreBluetooth (BLETransport), then
// drive the SAME Rust client over FFI (RingSession) to authenticate + drain history
// events into a writable SQLite DB. Mirrors `oura sync` on desktop. The actual BLE
// round-trip only works on a physical device (no Bluetooth in the simulator).

/// Where the app reads/writes its SQLite DB. The synced DB lives in Application
/// Support (writable); until a sync has happened we fall back to the bundled seed.
enum DB {
    static var url: URL {
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("oura.db")
    }
    /// Absolute path of the DB to READ from (synced if present, else bundled seed).
    static func readPath() -> String {
        let p = url.path
        if FileManager.default.fileExists(atPath: p) { return p }
        return Bundle.main.path(forResource: "oura", ofType: "db") ?? p
    }
}

/// The ring auth key (exported from the desktop client) kept in the Keychain.
enum Keychain {
    private static let account = "ring-auth-key"
    static func saveKey(_ hex: String) {
        let data = Data(hex.utf8)
        let base: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
                                    kSecAttrAccount as String: account]
        SecItemDelete(base as CFDictionary)
        var add = base
        add[kSecValueData as String] = data
        SecItemAdd(add as CFDictionary, nil)
    }
    static func loadKey() -> String? {
        let q: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
                                kSecAttrAccount as String: account,
                                kSecReturnData as String: true,
                                kSecMatchLimit as String: kSecMatchLimitOne]
        var out: AnyObject?
        guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess,
              let data = out as? Data, let s = String(data: data, encoding: .utf8) else { return nil }
        return s
    }
}

/// Bridges the Rust BleWriter callback onto BLETransport's async write. The callback is
/// synchronous (Rust's transact then waits for the response via push_frame), but a GATT
/// write-with-response must complete before the next one or CoreBluetooth rejects it as
/// busy. So writes are chained into a FIFO: each awaits the previous one's completion,
/// guaranteeing strictly sequential, non-overlapping writes.
final class RingWriter: BleWriter, @unchecked Sendable {
    private let transport: BLETransport
    private let lock = NSLock()
    private var tail: Task<Void, Never> = Task {}
    init(_ t: BLETransport) { transport = t }
    func write(data: Data) {
        let t = transport
        lock.lock()
        let prev = tail
        tail = Task {
            _ = await prev.value          // wait for the prior write to finish…
            do {
                try await t.write(data)   // …then perform (and await) this one
            } catch {
                // a failed write means the ring never got the frame — close the inbound
                // stream so the Rust drain stops waiting and the sync fails loudly
                // instead of proceeding as if the request was sent.
                t.abort()
            }
        }
        lock.unlock()
    }
}

/// Orchestrates a sync and exposes progress to the UI.
@MainActor
final class RingSync: ObservableObject {
    @Published var status: String = ""
    @Published var busy = false
    @Published var lastReport: SyncReport?

    private var transport: BLETransport?
    private var session: RingSession?
    private var pump: Task<Void, Never>?

    /// Connect, wire the inbound-frame pump, and run a full sync into the writable DB.
    func run(keyHex: String) async {
        lastReport = nil   // clear any prior success so a failed retry isn't read as one
        let key = keyHex.trimmingCharacters(in: .whitespacesAndNewlines)
        guard key.count == 32, key.allSatisfy(\.isHexDigit) else {
            status = "key must be 32 hex characters"
            return
        }
        busy = true
        defer { busy = false }

        status = "connecting to ring…"
        let t = BLETransport(nameContains: "Oura")
        transport = t
        do {
            try await t.connect()
        } catch {
            status = "couldn't connect — is the ring nearby and off the charger? (\(error))"
            return
        }

        let s = RingSession(writer: RingWriter(t))
        session = s
        pump = Task { for await frame in t.notifications { s.pushFrame(data: frame) } }

        status = "syncing…"
        do {
            let report = try await s.sync(dbPath: DB.url.path, keyHex: key)
            Keychain.saveKey(key)
            lastReport = report
            status = "synced — \(report.inserted) new events from \(report.serial)"
        } catch {
            status = "sync failed: \(error)"
        }
        pump?.cancel()
        pump = nil
    }
}
