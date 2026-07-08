import CoreBluetooth
import Foundation

// Native CoreBluetooth implementation of the ring link — the iOS counterpart to
// `oura-link::ble` (btleplug), conforming to the same shape as the Rust
// `Transport` trait: write a request frame, and receive the merged stream of
// inbound notification frames. The auth handshake + sync drain stay in Rust
// (oura-link `OuraClient`); this just moves bytes.
//
// Wiring: oura-core exposes `Transport` as a UniFFI callback interface and a
// `sync(transport, db_path)` entry; `RingTransport` below is what we hand across
// the FFI. (BLE needs a real ring + the simulator has no Bluetooth, so this runs
// on device only.) Requires `NSBluetoothAlwaysUsageDescription` in Info.plist.

enum RingUUID {
    static let service = CBUUID(string: "98ED0001-A541-11E4-B6A0-0002A5D5C51B")
    static let write = CBUUID(string: "98ED0002-A541-11E4-B6A0-0002A5D5C51B")
    // notify/indicate chars: gen-4 uses …0003; Ring 5 adds …0004/0005/0006.
    static let notify: Set<String> = [
        "98ED0003-A541-11E4-B6A0-0002A5D5C51B",
        "98ED0004-A541-11E4-B6A0-0002A5D5C51B",
        "98ED0005-A541-11E4-B6A0-0002A5D5C51B",
        "98ED0006-A541-11E4-B6A0-0002A5D5C51B",
    ]
}

/// The contract Rust drives over FFI: write a frame; observe inbound frames.
protocol RingTransport: AnyObject {
    func write(_ data: Data) async throws
    /// Every notify/indicate characteristic merged into one stream of raw frames.
    var notifications: AsyncStream<Data> { get }
}

enum BLEError: Error, CustomStringConvertible {
    case poweredOff, notFound, noWriteCharacteristic, disconnected, busy
    /// carries the stage the attempt was in, so "timed out" says *what* never happened
    /// (no advertisement seen vs GATT connect stalled vs subscriptions pending).
    case timedOut(stage: String)

    var description: String {
        switch self {
        case .poweredOff: return "Bluetooth is off or not authorized"
        case .notFound: return "ring service/characteristics not found"
        case .noWriteCharacteristic: return "no write characteristic (98ED0002)"
        case .disconnected: return "ring disconnected"
        case .busy: return "another BLE operation is in flight"
        case .timedOut(let stage): return "timed out while \(stage)"
        }
    }
}

/// Scans for an Oura ring advertising the service (filtered by case-insensitive
/// name), connects, discovers the write + notify characteristics, and bridges them
/// to `RingTransport`. Mirrors `oura-link::ble::Connection`.
// @unchecked Sendable: continuations are taken/resumed under `lock`, and the rest of
// the mutable CB state is only touched on the central manager's callback queue.
final class BLETransport: NSObject, RingTransport, CBCentralManagerDelegate, CBPeripheralDelegate, @unchecked Sendable {
    private var central: CBCentralManager!
    private var peripheral: CBPeripheral?
    private var writeChar: CBCharacteristic?
    private let nameContains: String

    private var notifyContinuation: AsyncStream<Data>.Continuation?
    // recreated per connect() so a reconnect gets a fresh, live stream — the previous
    // one is finished on disconnect, and a single lazy stream would stay terminated,
    // silently dropping all frames after the first link loss.
    private(set) var notifications: AsyncStream<Data> = AsyncStream { _ in }

    private var connectCont: CheckedContinuation<Void, Error>?
    private var writeCont: CheckedContinuation<Void, Error>?
    private var connectTimeout: DispatchWorkItem?
    private var pendingNotify = 0 // notify subscriptions still awaiting confirmation
    private var poweredOn = false
    // where the in-flight connect currently is, for the timeout error message.
    private var stage = "waiting for Bluetooth to power on"
    // advertisement reports already logged (id|name) — allow-duplicates re-reports the
    // same ring many times a second; log each device once, and again when its name
    // first arrives via scan response. Only touched on the (serial) CB queue.
    private var loggedAds = Set<String>()
    // distinct non-ring devices seen this scan: proves the radio works when the ring
    // itself never shows up (written on the CB queue, read under `lock` at timeout).
    private var otherDevices = Set<UUID>()
    // delegate callbacks land on a concurrent queue; this serialises take-and-resume
    // of the continuations so a success and a timeout can't both resume one (a crash).
    private let lock = NSLock()

    init(nameContains: String = "Oura") {
        self.nameContains = nameContains
        super.init()
        // CoreBluetooth requires a SERIAL queue for delegate callbacks; a concurrent
        // global queue can deliver them out of order (e.g. a notify confirmation
        // racing service discovery).
        central = CBCentralManager(
            delegate: self,
            queue: DispatchQueue(label: "md.thomas.openoura.ble", qos: .userInitiated))
    }

    /// Scan → connect → discover. Resolves once the write characteristic is ready
    /// and notifications are subscribed.
    ///
    /// The default budget mirrors the desktop client, which allows 25 s of scanning
    /// plus 30 s for connect + discovery: a worn ring advertises in low-power mode
    /// only intermittently, so 20 s of scan alone was routinely not enough.
    func connect(timeout: TimeInterval = 50) async throws {
        try await withCheckedThrowingContinuation { (c: CheckedContinuation<Void, Error>) in
            lock.lock()
            // reject (rather than strand) a second connect while one is in flight OR
            // already connected — a re-entrant connect would swap the notifications
            // stream and silently strand whoever is still draining the current one.
            if connectCont != nil || writeChar != nil {
                lock.unlock()
                c.resume(throwing: BLEError.busy)
                return
            }
            connectCont = c
            stage = "waiting for Bluetooth to power on"
            // fail rather than hang forever if the ring never advertises (off the
            // charger / not worn) or Bluetooth stays off. The work item is stored so
            // finishConnect() can cancel it — a stale timer from a prior/finished
            // attempt must not fire and abort a newer connection.
            let work = DispatchWorkItem { [weak self] in
                guard let self else { return }
                self.central.stopScan()
                self.lock.lock()
                var at = self.stage
                let others = self.otherDevices.count
                self.lock.unlock()
                // Distinguish "the ring isn't advertising" from "Bluetooth is dead":
                // the unfiltered scan tells us whether ANY advertisements arrived.
                if at.hasPrefix("scanning") {
                    at = others > 0
                        ? "scanning — saw \(others) other BLE device(s) but no Oura ring: "
                            + "the ring is connected to another device (phone with the "
                            + "official app? Mac?), off its charger and asleep, or out of battery"
                        : "scanning — saw NO BLE advertisements at all: Bluetooth may be "
                            + "off, restricted, or the permission was revoked"
                }
                dlog("ble", "TIMEOUT while \(at)")
                self.finishConnect(.failure(BLEError.timedOut(stage: at)))
            }
            connectTimeout = work
            lock.unlock()
            // fresh notification stream for this connection (a reconnect must not hand
            // back the previous, already-finished stream).
            notifications = AsyncStream { self.notifyContinuation = $0 }
            DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: work)
            dlog("ble", "connect(timeout: \(Int(timeout))s) — central.state=\(Self.name(of: central.state))")
            switch central.state {
            case .poweredOn:
                startScan()
            case .poweredOff, .unauthorized, .unsupported:
                // fail NOW instead of burning the whole timeout: the one-shot state
                // callback fired before this connect registered, so it won't recur.
                finishConnect(.failure(BLEError.poweredOff))
            default:
                break // .unknown/.resetting — wait for centralManagerDidUpdateState
            }
        }
    }

    private static func name(of state: CBManagerState) -> String {
        switch state {
        case .poweredOn: return "poweredOn"
        case .poweredOff: return "poweredOff"
        case .unauthorized: return "unauthorized (check Settings > Privacy > Bluetooth)"
        case .unsupported: return "unsupported"
        case .resetting: return "resetting"
        case .unknown: return "unknown (still initializing)"
        @unknown default: return "state \(state.rawValue)"
        }
    }

    private func startScan() {
        lock.lock(); stage = "scanning — no ring advertisement seen yet"; lock.unlock()
        dlog("ble", "scanning (unfiltered, allow duplicates) — matching service \(RingUUID.service)")
        // UNFILTERED scan, matching done in didDiscover: an OS-side service filter
        // reports nothing when the ring isn't advertising, which is indistinguishable
        // from broken Bluetooth. Seeing (and counting) other devices' advertisements
        // proves the radio works and pins the failure on the ring itself.
        // Allow duplicate discovery reports: the ring's ADV packet is full (flags +
        // 128-bit service UUID + manufacturer data), so its name only arrives in the
        // scan response, which a worn ring in low-power mode answers lazily. Without
        // duplicates iOS may coalesce the ring into a single early, name-less report.
        central.scanForPeripherals(
            withServices: nil,
            options: [CBCentralManagerScanOptionAllowDuplicatesKey: true])
    }

    /// Finish the inbound frame stream so a Rust drain blocked on `recv` returns at once
    /// (instead of waiting out the quiet-window) — used when a write fails so the sync
    /// surfaces the error promptly rather than proceeding as if the frame was sent.
    func abort() { notifyContinuation?.finish() }

    /// Release the ring: cancel the GATT connection (and any scan) and finish the
    /// stream. The ring has a SINGLE BLE link and only advertises when nothing holds
    /// it — an app that keeps the connection after a sync blocks the official app,
    /// the Mac, and its own next scan.
    func disconnect() {
        lock.lock()
        let p = peripheral
        peripheral = nil
        writeChar = nil
        lock.unlock()
        central.stopScan()
        notifyContinuation?.finish()
        if let p {
            central.cancelPeripheralConnection(p)
            dlog("ble", "disconnected — ring link released")
        }
    }

    /// Write a request frame and await the ring's GATT acknowledgement, so the caller
    /// (Rust `OuraClient`, which drives requests sequentially) knows the frame landed
    /// before sending the next. Resolved in `didWriteValueFor`.
    func write(_ data: Data) async throws {
        guard let p = peripheral, let wc = writeChar else { throw BLEError.noWriteCharacteristic }
        try await withCheckedThrowingContinuation { (c: CheckedContinuation<Void, Error>) in
            lock.lock()
            // reject (don't strand) an overlapping write — the caller drives writes
            // sequentially, so a second in-flight write is a misuse, not a queue.
            if writeCont != nil {
                lock.unlock()
                c.resume(throwing: BLEError.busy)
                return
            }
            writeCont = c
            lock.unlock()
            dlog("send", "\(data.count)B \(data.hexString)")
            p.writeValue(data, for: wc, type: .withResponse)
        }
    }

    // ── CBCentralManagerDelegate ──
    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        dlog("ble", "central state → \(Self.name(of: central.state))")
        switch central.state {
        case .poweredOn:
            poweredOn = true
            if connectCont != nil { startScan() }
        case .poweredOff, .unauthorized, .unsupported:
            finishConnect(.failure(BLEError.poweredOff))
        default: break
        }
    }

    func centralManager(_ central: CBCentralManager, didDiscover peripheral: CBPeripheral,
                        advertisementData: [String: Any], rssi RSSI: NSNumber) {
        // ignore a discovery that arrives after the attempt already resolved (e.g. a
        // callback queued just past the timeout) — don't start a stray connection.
        lock.lock(); let active = connectCont != nil; lock.unlock()
        guard active else { central.stopScan(); return }
        let advName = (advertisementData[CBAdvertisementDataLocalNameKey] as? String)
            ?? peripheral.name ?? ""
        let advServices = advertisementData[CBAdvertisementDataServiceUUIDsKey] as? [CBUUID] ?? []
        // A ring advertises the proprietary Oura service UUID; accept on that even
        // when the name is missing (the name lives in the scan response, which a worn
        // ring may not have answered yet). Name match covers factory-reset shapes.
        let isRing = advServices.contains(RingUUID.service)
            || (!advName.isEmpty && advName.lowercased().contains(nameContains.lowercased()))
        if !isRing {
            // count distinct non-ring devices as radio liveness proof; log the first
            // few so the transcript shows what the scan IS seeing.
            lock.lock()
            let inserted = otherDevices.insert(peripheral.identifier).inserted
            let count = otherDevices.count
            lock.unlock()
            if inserted && count <= 5 {
                dlog("scan", "other device '\(advName.isEmpty ? "<no name>" : advName)' rssi=\(RSSI) — not a ring (\(count) distinct so far)")
            }
            return
        }
        // full advertisement dump, once per (device, name) so allow-duplicates doesn't
        // flood the transcript but a late-arriving scan-response name still shows up.
        let adKey = "\(peripheral.identifier.uuidString)|\(advName)"
        if loggedAds.insert(adKey).inserted {
            let svc = advServices.map(\.uuidString).joined(separator: ",")
            let mfr = (advertisementData[CBAdvertisementDataManufacturerDataKey] as? Data)?.hexString ?? "—"
            let conn = advertisementData[CBAdvertisementDataIsConnectable] as? Bool
            dlog("scan", "saw '\(advName.isEmpty ? "<no name>" : advName)' id=\(peripheral.identifier.uuidString.suffix(12)) rssi=\(RSSI) services=[\(svc)] mfr=\(mfr) connectable=\(conn.map(String.init) ?? "?")")
        }
        central.stopScan()
        dlog("ble", "matched '\(advName.isEmpty ? "<no name yet>" : advName)' rssi=\(RSSI) — GATT connect…")
        lock.lock(); stage = "GATT-connecting to the discovered ring"; lock.unlock()
        self.peripheral = peripheral
        peripheral.delegate = self
        central.connect(peripheral, options: nil)
    }

    func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        let mtu = peripheral.maximumWriteValueLength(for: .withResponse)
        dlog("ble", "GATT connected (maxWrite=\(mtu)B) — discovering the Oura service")
        lock.lock(); stage = "discovering services/characteristics"; lock.unlock()
        peripheral.discoverServices([RingUUID.service])
    }

    func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral,
                        error: Error?) {
        dlog("ble", "GATT connect FAILED: \(error.map { String(describing: $0) } ?? "no error info")")
        finishConnect(.failure(error ?? BLEError.notFound))
    }

    func centralManager(_ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral,
                        error: Error?) {
        dlog("ble", "peripheral disconnected: \(error.map { String(describing: $0) } ?? "clean")")
        notifyContinuation?.finish()
        // don't strand a caller awaiting a connect or write when the link drops.
        finishWrite(.failure(BLEError.disconnected))
        finishConnect(.failure(BLEError.disconnected))
    }

    // ── CBPeripheralDelegate ──
    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        if let error {
            dlog("ble", "service discovery FAILED: \(error)")
            return finishConnect(.failure(error))
        }
        let found = (peripheral.services ?? []).map(\.uuid.uuidString).joined(separator: ",")
        dlog("ble", "services: [\(found)]")
        guard let svc = peripheral.services?.first(where: { $0.uuid == RingUUID.service }) else {
            dlog("ble", "Oura service 98ED0001 NOT among them — wrong device?")
            return finishConnect(.failure(BLEError.notFound))
        }
        peripheral.discoverCharacteristics(nil, for: svc)
    }

    private static func props(_ c: CBCharacteristic) -> String {
        var p: [String] = []
        if c.properties.contains(.read) { p.append("read") }
        if c.properties.contains(.write) { p.append("write") }
        if c.properties.contains(.writeWithoutResponse) { p.append("writeNR") }
        if c.properties.contains(.notify) { p.append("notify") }
        if c.properties.contains(.indicate) { p.append("indicate") }
        return p.joined(separator: "+")
    }

    func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService,
                    error: Error?) {
        if let error {
            dlog("ble", "characteristic discovery FAILED: \(error)")
            return finishConnect(.failure(error))
        }
        var notifyChars: [CBCharacteristic] = []
        for c in service.characteristics ?? [] {
            dlog("ble", "char …\(c.uuid.uuidString.suffix(4).lowercased()) [\(Self.props(c))]")
            if c.uuid == RingUUID.write { writeChar = c }
            if RingUUID.notify.contains(c.uuid.uuidString.uppercased()) { notifyChars.append(c) }
        }
        dlog("ble", "characteristics discovered — write=\(writeChar != nil), notify=\(notifyChars.count)")
        guard writeChar != nil else {
            dlog("ble", "no write characteristic (98ED0002) — wrong device?")
            return finishConnect(.failure(BLEError.noWriteCharacteristic))
        }
        guard !notifyChars.isEmpty else {
            dlog("ble", "no notify characteristics (98ED0003..0006)")
            return finishConnect(.failure(BLEError.notFound))
        }
        // don't report "connected" until every notify subscription is confirmed —
        // otherwise Rust can start syncing before inbound frames flow and miss the
        // ring's early responses. didUpdateNotificationStateFor finishes the connect.
        lock.lock()
        pendingNotify = notifyChars.count
        stage = "subscribing to notify characteristics"
        lock.unlock()
        for c in notifyChars { peripheral.setNotifyValue(true, for: c) }
    }

    func peripheral(_ peripheral: CBPeripheral, didUpdateNotificationStateFor characteristic: CBCharacteristic,
                    error: Error?) {
        if let error {
            // a pairing/encryption demand surfaces here (e.g. "Authentication is
            // insufficient") — the single most diagnostic error on a keyed ring.
            dlog("ble", "subscribe FAILED on …\(characteristic.uuid.uuidString.suffix(4).lowercased()): \(error)")
            return finishConnect(.failure(error))
        }
        dlog("ble", "subscribed …\(characteristic.uuid.uuidString.suffix(4).lowercased())")
        lock.lock(); pendingNotify -= 1; let ready = pendingNotify <= 0; lock.unlock()
        if ready {
            dlog("ble", "all notify subscriptions confirmed — BLE link ready, handing to Rust auth")
            finishConnect(.success(()))
        }
    }

    func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic,
                    error: Error?) {
        // drop the callback on a read/notify error — a stale payload must not be fed
        // into the frame stream Rust drains as protocol responses.
        guard error == nil, let v = characteristic.value else {
            if let error { dlog("ble", "notify ERROR on \(characteristic.uuid): \(error)") }
            return
        }
        dlog("recv", "\(v.count)B [\(characteristic.uuid.uuidString.suffix(4).lowercased())] \(v.hexString)")
        notifyContinuation?.yield(v)
    }

    /// GATT write-with-response acknowledgement (or error) for the in-flight `write`.
    func peripheral(_ peripheral: CBPeripheral, didWriteValueFor characteristic: CBCharacteristic,
                    error: Error?) {
        if let error { dlog("ble", "write NAK: \(error)") }
        finishWrite(error.map { .failure($0) } ?? .success(()))
    }

    private func finishConnect(_ result: Result<Void, Error>) {
        lock.lock()
        let c = connectCont; connectCont = nil
        let timer = connectTimeout; connectTimeout = nil
        lock.unlock()
        timer?.cancel() // stop a still-pending timeout from firing on a finished attempt
        if case .failure = result {
            // tear down an abandoned/failed attempt: cancel the peripheral so iOS stops
            // delivering its callbacks, and reset the per-attempt state so a stray late
            // didUpdateNotificationStateFor can't bleed into a later connect's counter.
            if let p = peripheral { central.cancelPeripheralConnection(p) }
            peripheral = nil
            writeChar = nil
            lock.lock(); pendingNotify = 0; lock.unlock()
        }
        c?.resume(with: result)
    }

    private func finishWrite(_ result: Result<Void, Error>) {
        lock.lock(); let c = writeCont; writeCont = nil; lock.unlock()
        c?.resume(with: result)
    }
}
