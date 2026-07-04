import Foundation
import Security
import os
import CryptoKit

// Shared debug logger for the connect + auth + sync path. View live in Console.app
// (filter subsystem `md.thomas.openoura`) or `xcrun simctl spawn booted log stream
// --predicate 'subsystem == "md.thomas.openoura"'`. Frames are logged as hex; the auth
// KEY is never logged (only its length), and the Rust layer logs only nonce/state bytes.
let ringLog = Logger(subsystem: "md.thomas.openoura", category: "ring")
extension Data {
    var hexString: String { map { String(format: "%02x", $0) }.joined() }
}

/// In-app diagnostics for the connect/auth/sync pipeline. Every `dlog` line is kept
/// in a bounded in-memory transcript that the sync screen shows live and can copy to
/// the pasteboard — so a failure in the field can be pasted into a bug report as-is,
/// with no tethered Mac, no Console.app, and no rebuild with more logging.
final class RingDiag: ObservableObject, @unchecked Sendable {
    static let shared = RingDiag()

    /// Coalesced UI mirror (the last `tailCount` lines) + total, updated ≤4×/s so a
    /// chatty sync drain doesn't hammer SwiftUI from the BLE queue.
    @Published private(set) var tail: [String] = []
    @Published private(set) var totalLines = 0

    private let lock = NSLock()
    private var lines: [String] = []
    private var total = 0
    private var dropped = 0
    private var uiUpdateQueued = false
    private static let cap = 4000      // ring buffer: newest lines win
    private static let tailCount = 30

    // DateFormatter is documented thread-safe on modern OSes.
    private static let clock: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss.SSS"
        return f
    }()

    func log(_ tag: String, _ msg: String) {
        let line = "\(Self.clock.string(from: Date())) [\(tag)] \(msg)"
        lock.lock()
        lines.append(line)
        total += 1
        if lines.count > Self.cap {
            dropped += lines.count - Self.cap
            lines.removeFirst(lines.count - Self.cap)
        }
        let alreadyQueued = uiUpdateQueued
        uiUpdateQueued = true
        lock.unlock()
        if !alreadyQueued {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { [weak self] in
                guard let self else { return }
                self.lock.lock()
                self.uiUpdateQueued = false
                let t = Array(self.lines.suffix(Self.tailCount))
                let n = self.total
                self.lock.unlock()
                self.tail = t
                self.totalLines = n
            }
        }
    }

    /// The full transcript, prefixed with enough environment to make it self-contained.
    func dump() -> String {
        lock.lock()
        let body = lines.joined(separator: "\n")
        let droppedNote = dropped > 0 ? "\n(oldest \(dropped) lines dropped — buffer cap \(Self.cap))" : ""
        lock.unlock()
        let os = ProcessInfo.processInfo.operatingSystemVersionString
        let v = Bundle.main.infoDictionary
        let app = "\(v?["CFBundleShortVersionString"] ?? "?") (\(v?["CFBundleVersion"] ?? "?"))"
        return "open_oura \(app) — iOS \(os) — \(Date())\(droppedNote)\n\(body)"
    }

    func clear() {
        lock.lock()
        lines = []; total = 0; dropped = 0
        lock.unlock()
        DispatchQueue.main.async { self.tail = []; self.totalLines = 0 }
    }
}

/// One call, two sinks: the in-app transcript (copy-pasteable) and os.log. The
/// `.public` privacy is deliberate — without it, os.log redacts interpolated strings
/// as `<private>` on an untethered device, which is exactly when we need them.
func dlog(_ tag: String, _ msg: String) {
    RingDiag.shared.log(tag, msg)
    ringLog.info("[\(tag, privacy: .public)] \(msg, privacy: .public)")
}

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
                dlog("write", "FAILED (\(error)) — aborting inbound stream so the sync errors out")
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
        // one attempt = one transcript, so a copied log is unambiguous about which run
        // it describes.
        RingDiag.shared.clear()
        let key = keyHex.trimmingCharacters(in: .whitespacesAndNewlines)
        // A SHA-256-derived fingerprint confirms the *right* key arrived intact without
        // exposing any key bytes (a raw slice would leak key material).
        let fp = SHA256.hash(data: Data(key.utf8)).prefix(4).map { String(format: "%02x", $0) }.joined()
        let hexOk = key.allSatisfy(\.isHexDigit)
        dlog("sync", "run — key len=\(key.count), hex=\(hexOk), fp(sha256)=\(fp)")
        guard key.count == 32, key.allSatisfy(\.isHexDigit) else {
            dlog("sync", "rejected key: len=\(key.count) (need 32 hex chars)")
            status = "key must be 32 hex characters"
            return
        }
        busy = true
        defer { busy = false }

        status = "connecting to ring…"
        dlog("sync", "connecting — scanning for the Oura service (name filter 'Oura')…")
        let t = BLETransport(nameContains: "Oura")
        transport = t
        do {
            try await t.connect()
        } catch {
            dlog("sync", "BLE connect FAILED: \(error)")
            // the ring advertises reliably only ON its charger (low-power adv when
            // worn), and it has a single BLE link — a phone running the official app
            // holds it, leaving nothing to discover.
            status = "couldn't connect (\(error)) — put the ring on its charger and " +
                "turn off Bluetooth on the phone with the official Oura app"
            return
        }
        dlog("sync", "BLE link ready — creating RingSession + inbound-frame pump")

        let s = RingSession(writer: RingWriter(t))
        session = s
        pump = Task { for await frame in t.notifications { s.pushFrame(data: frame) } }

        status = "syncing…"
        dlog("sync", "starting FFI sync() — authenticate, app stream, then event drain")
        do {
            let report = try await s.sync(dbPath: DB.url.path, keyHex: key)
            Keychain.saveKey(key)
            lastReport = report
            dlog("sync", "OK — serial=\(report.serial) inserted=\(report.inserted) events=\(report.eventsSynced) cursor=\(report.nextCursor)")
            status = "synced — \(report.inserted) new events from \(report.serial)"
        } catch {
            // the Rust auth layer packs the state byte + nonce + packet dump into this
            // message, so log it verbatim — it's the primary diagnostic to report back.
            dlog("sync", "FAILED: \(error)")
            status = "sync failed: \(error)"
        }
        pump?.cancel()
        pump = nil
    }
}
