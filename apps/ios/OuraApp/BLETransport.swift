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

enum BLEError: Error { case poweredOff, notFound, noWriteCharacteristic, disconnected, timedOut, busy }

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
    lazy var notifications: AsyncStream<Data> = AsyncStream { self.notifyContinuation = $0 }

    private var connectCont: CheckedContinuation<Void, Error>?
    private var writeCont: CheckedContinuation<Void, Error>?
    private var connectTimeout: DispatchWorkItem?
    private var pendingNotify = 0 // notify subscriptions still awaiting confirmation
    private var poweredOn = false
    // delegate callbacks land on a concurrent queue; this serialises take-and-resume
    // of the continuations so a success and a timeout can't both resume one (a crash).
    private let lock = NSLock()

    init(nameContains: String = "Oura") {
        self.nameContains = nameContains
        super.init()
        central = CBCentralManager(delegate: self, queue: .global(qos: .userInitiated))
    }

    /// Scan → connect → discover. Resolves once the write characteristic is ready
    /// and notifications are subscribed.
    func connect(timeout: TimeInterval = 20) async throws {
        try await withCheckedThrowingContinuation { (c: CheckedContinuation<Void, Error>) in
            lock.lock()
            // reject (rather than strand) a second connect while one is in flight — the
            // earlier caller keeps its continuation and stays the active attempt.
            if connectCont != nil {
                lock.unlock()
                c.resume(throwing: BLEError.busy)
                return
            }
            connectCont = c
            // fail rather than hang forever if the ring never advertises (off the
            // charger / not worn) or Bluetooth stays off. The work item is stored so
            // finishConnect() can cancel it — a stale timer from a prior/finished
            // attempt must not fire and abort a newer connection.
            let work = DispatchWorkItem { [weak self] in
                guard let self else { return }
                self.central.stopScan()
                self.finishConnect(.failure(BLEError.timedOut))
            }
            connectTimeout = work
            lock.unlock()
            DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: work)
            if poweredOn { startScan() }
        }
    }

    private func startScan() {
        central.scanForPeripherals(withServices: [RingUUID.service], options: nil)
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
            p.writeValue(data, for: wc, type: .withResponse)
        }
    }

    // ── CBCentralManagerDelegate ──
    func centralManagerDidUpdateState(_ central: CBCentralManager) {
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
        let advName = (advertisementData[CBAdvertisementDataLocalNameKey] as? String)
            ?? peripheral.name ?? ""
        guard advName.lowercased().contains(nameContains.lowercased()) else { return }
        central.stopScan()
        self.peripheral = peripheral
        peripheral.delegate = self
        central.connect(peripheral, options: nil)
    }

    func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        peripheral.discoverServices([RingUUID.service])
    }

    func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral,
                        error: Error?) {
        finishConnect(.failure(error ?? BLEError.notFound))
    }

    func centralManager(_ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral,
                        error: Error?) {
        notifyContinuation?.finish()
        // don't strand a caller awaiting a connect or write when the link drops.
        finishWrite(.failure(BLEError.disconnected))
        finishConnect(.failure(BLEError.disconnected))
    }

    // ── CBPeripheralDelegate ──
    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        if let error { return finishConnect(.failure(error)) }
        guard let svc = peripheral.services?.first(where: { $0.uuid == RingUUID.service }) else {
            return finishConnect(.failure(BLEError.notFound))
        }
        peripheral.discoverCharacteristics(nil, for: svc)
    }

    func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService,
                    error: Error?) {
        if let error { return finishConnect(.failure(error)) }
        var notifyChars: [CBCharacteristic] = []
        for c in service.characteristics ?? [] {
            if c.uuid == RingUUID.write { writeChar = c }
            if RingUUID.notify.contains(c.uuid.uuidString.uppercased()) { notifyChars.append(c) }
        }
        guard writeChar != nil else { return finishConnect(.failure(BLEError.noWriteCharacteristic)) }
        guard !notifyChars.isEmpty else { return finishConnect(.failure(BLEError.notFound)) }
        // don't report "connected" until every notify subscription is confirmed —
        // otherwise Rust can start syncing before inbound frames flow and miss the
        // ring's early responses. didUpdateNotificationStateFor finishes the connect.
        lock.lock(); pendingNotify = notifyChars.count; lock.unlock()
        for c in notifyChars { peripheral.setNotifyValue(true, for: c) }
    }

    func peripheral(_ peripheral: CBPeripheral, didUpdateNotificationStateFor characteristic: CBCharacteristic,
                    error: Error?) {
        if let error { return finishConnect(.failure(error)) }
        lock.lock(); pendingNotify -= 1; let ready = pendingNotify <= 0; lock.unlock()
        if ready { finishConnect(.success(())) }
    }

    func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic,
                    error: Error?) {
        if let v = characteristic.value { notifyContinuation?.yield(v) }
    }

    /// GATT write-with-response acknowledgement (or error) for the in-flight `write`.
    func peripheral(_ peripheral: CBPeripheral, didWriteValueFor characteristic: CBCharacteristic,
                    error: Error?) {
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
