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

enum BLEError: Error { case poweredOff, notFound, noWriteCharacteristic, disconnected }

/// Scans for an Oura ring advertising the service (filtered by case-insensitive
/// name), connects, discovers the write + notify characteristics, and bridges them
/// to `RingTransport`. Mirrors `oura-link::ble::Connection`.
final class BLETransport: NSObject, RingTransport, CBCentralManagerDelegate, CBPeripheralDelegate {
    private var central: CBCentralManager!
    private var peripheral: CBPeripheral?
    private var writeChar: CBCharacteristic?
    private let nameContains: String

    private var notifyContinuation: AsyncStream<Data>.Continuation?
    lazy var notifications: AsyncStream<Data> = AsyncStream { self.notifyContinuation = $0 }

    private var connectCont: CheckedContinuation<Void, Error>?
    private var poweredOn = false

    init(nameContains: String = "Oura") {
        self.nameContains = nameContains
        super.init()
        central = CBCentralManager(delegate: self, queue: .global(qos: .userInitiated))
    }

    /// Scan → connect → discover. Resolves once the write characteristic is ready
    /// and notifications are subscribed.
    func connect(timeout: TimeInterval = 20) async throws {
        try await withCheckedThrowingContinuation { (c: CheckedContinuation<Void, Error>) in
            connectCont = c
            if poweredOn { startScan() }
        }
    }

    private func startScan() {
        central.scanForPeripherals(withServices: [RingUUID.service], options: nil)
    }

    func write(_ data: Data) async throws {
        guard let p = peripheral, let wc = writeChar else { throw BLEError.noWriteCharacteristic }
        p.writeValue(data, for: wc, type: .withResponse)
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
    }

    // ── CBPeripheralDelegate ──
    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        guard let svc = peripheral.services?.first(where: { $0.uuid == RingUUID.service }) else {
            return finishConnect(.failure(BLEError.notFound))
        }
        peripheral.discoverCharacteristics(nil, for: svc)
    }

    func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService,
                    error: Error?) {
        for c in service.characteristics ?? [] {
            if c.uuid == RingUUID.write { writeChar = c }
            if RingUUID.notify.contains(c.uuid.uuidString.uppercased()) {
                peripheral.setNotifyValue(true, for: c)
            }
        }
        if writeChar != nil { finishConnect(.success(())) }
        else { finishConnect(.failure(BLEError.noWriteCharacteristic)) }
    }

    func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic,
                    error: Error?) {
        if let v = characteristic.value { notifyContinuation?.yield(v) }
    }

    private func finishConnect(_ result: Result<Void, Error>) {
        guard let c = connectCont else { return }
        connectCont = nil
        c.resume(with: result)
    }
}
