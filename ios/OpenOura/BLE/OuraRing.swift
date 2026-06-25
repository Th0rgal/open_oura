import Combine
import CoreBluetooth
import Foundation
import os

/// One decoded history event (timestamp in ring deciseconds + decoded JSON).
struct DecodedEvent: Identifiable {
    let id = UUID()
    let tag: UInt8
    let timestamp: UInt32
    let name: String
    let json: [String: Any]
}

enum ConnState: Equatable {
    case idle, scanning, connecting, authenticating, ready, failed(String)
}

/// CoreBluetooth client + protocol orchestration for an Oura ring. Mirrors the
/// Rust `OuraClient`: request/response with a quiet-window collector, a bounded
/// single-response wait, and a stream-safe incremental event drain.
final class OuraRing: NSObject, ObservableObject {
    // Published UI state (always mutated on the main queue).
    @Published var state: ConnState = .idle
    @Published var status: String = "Not connected"
    @Published var firmware: String?
    @Published var serial: String?
    @Published var hardware: String?
    @Published var batteryPercent: Int?
    @Published var charging = false

    @Published var liveActive = false
    @Published var liveHR: Int?
    @Published var liveHRV: Int?
    @Published var motionG: Double?
    @Published var restlessness: Int?
    @Published var hrSeries: [Double] = []
    @Published var hrvSeries: [Double] = []
    @Published var motionSeries: [Double] = []

    @Published var syncing = false
    @Published var events: [DecodedEvent] = []
    @Published var health = HealthData()
    @Published var alert: String?   // surfaced as a user-facing alert on failure
    @Published var showConnectGuide = false   // drives the onboarding/connect sheet

    /// Auto-reconnect to the known ring (and auto-sync) without manual taps.
    @Published var autoConnect: Bool = (UserDefaults.standard.object(forKey: "autoConnect") as? Bool) ?? true {
        didSet { UserDefaults.standard.set(autoConnect, forKey: "autoConnect") }
    }
    @Published var autoSync: Bool = (UserDefaults.standard.object(forKey: "autoSync") as? Bool) ?? true {
        didSet { UserDefaults.standard.set(autoSync, forKey: "autoSync") }
    }

    /// Whether we can silently reconnect on launch (we have a key) — only a ring
    /// with no key stored needs the onboarding guide.
    var canAutoReconnect: Bool { autoConnect && KeyStore.keyBytes() != nil }
    private var wantsAutoReconnect = false
    private var autoScanning = false
    private var userDisconnecting = false

    private let bleQueue = DispatchQueue(label: "com.openoura.ble")
    private let log = Logger(subsystem: "com.openoura.app", category: "ble")
    private var central: CBCentralManager!
    private var peripheral: CBPeripheral?
    private var writeChar: CBCharacteristic?
    private var pendingConnect = false
    private var connecting = false      // a connect attempt is in flight (bleQueue)
    private var scanTimer: DispatchSourceTimer?
    private var connectTimer: DispatchSourceTimer?
    private var readyTimer: DispatchSourceTimer?

    // Frame fan-out: every inbound notification is delivered to all listeners
    // (touched only on bleQueue). Transactions register temporary listeners.
    private var listeners: [UUID: (Data) -> Void] = [:]
    private var readyContinuation: CheckedContinuation<Void, Never>?

    // Live-mode rolling buffers (bleQueue).
    private var ibiBuffer: [Double] = []
    private var liveListenerID: UUID?
    private var liveTask: Task<Void, Never>?
    private var restMoves = 0, restWin = 0

    override init() {
        super.init()
        central = CBCentralManager(delegate: self, queue: bleQueue)
    }

    // MARK: - Publishing helper
    private func publish(_ block: @escaping () -> Void) { DispatchQueue.main.async(execute: block) }

    /// Diagnostic log: to the unified log (Console.app) and stdout (captured by
    /// `devicectl ... process launch --console`).
    private func dbg(_ msg: String) {
        log.info("\(msg, privacy: .public)")
        print("[ble] \(msg)")
    }

    private func writeRaw(_ data: Data) {
        guard let p = peripheral, let c = writeChar else { return }
        p.writeValue(data, for: c, type: .withResponse)
    }

    // MARK: - Connect / scan
    func connect() {
        bleQueue.async {
            // Re-entrancy guard: ignore taps while a connect attempt is in flight or
            // we're already linked (prevents the double-scan + leaked continuation).
            guard !self.connecting, self.writeChar == nil else {
                self.dbg("connect ignored (already connecting/linked)")
                return
            }
            self.connecting = true
            self.pendingConnect = true
            self.publish { self.state = .scanning; self.status = "Scanning for ring…" }
            self.startScanIfReady()
        }
    }

    /// Begin scanning once Bluetooth is powered on (called on bleQueue, also from
    /// `centralManagerDidUpdateState` so a not-yet-ready radio doesn't drop the request).
    private func startScanIfReady() {
        guard pendingConnect else { return }
        switch central.state {
        case .poweredOn:
            pendingConnect = false
            dbg("scanning for ring (service \(OuraGATT.service.uuidString))")
            central.scanForPeripherals(withServices: [OuraGATT.service])
            scanTimer?.cancel()
            let t = DispatchSource.makeTimerSource(queue: bleQueue)
            t.schedule(deadline: .now() + 15)
            t.setEventHandler { [weak self] in self?.scanTimedOut() }
            t.resume(); scanTimer = t
        case .poweredOff:
            failConnect("Bluetooth off", "Turn on Bluetooth")
        case .unauthorized:
            failConnect("No BT permission", "Allow Bluetooth in iOS Settings → Open Oura")
        case .unsupported:
            failConnect("BLE unsupported", "BLE unsupported on this device")
        case .unknown, .resetting:
            dbg("bluetooth state \(self.central.state.rawValue) — waiting")
        @unknown default: break
        }
    }

    private func scanTimedOut() {
        guard writeChar == nil else { return }
        central.stopScan()
        dbg("scan timed out — ring not found")
        failConnect("Ring not found", "Ring not found — wear it, keep it close, and quit other BLE apps")
    }

    /// Abort an in-flight connect attempt: tear down timers, cancel the (possibly
    /// hung) CoreBluetooth connection, surface a user alert, and resume any waiter.
    /// Safe to call from anywhere — always runs its mutations on bleQueue.
    private func failConnect(_ short: String, _ status: String) {
        bleQueue.async {
            self.dbg("connect failed: \(short)")
            self.connecting = false; self.pendingConnect = false; self.autoScanning = false
            self.scanTimer?.cancel(); self.connectTimer?.cancel()
            self.central.stopScan()
            if let p = self.peripheral { self.central.cancelPeripheralConnection(p) }
            self.publish { self.state = .failed(short); self.status = status; self.alert = status }
            if let c = self.readyContinuation { self.readyContinuation = nil; self.readyTimer?.cancel(); c.resume() }
        }
    }

    func disconnect() {
        stopLive()
        bleQueue.async {
            self.userDisconnecting = true
            self.wantsAutoReconnect = false
            self.connecting = false
            if let p = self.peripheral { self.central.cancelPeripheralConnection(p) }
        }
        publish { self.state = .idle; self.status = "Disconnected" }
    }

    /// Suspend until the link is ready (write characteristic discovered), or until
    /// `timeout` elapses (so a failed scan/connect never hangs the caller).
    private func waitUntilReady(timeout: TimeInterval = 20) async {
        await withCheckedContinuation { (c: CheckedContinuation<Void, Never>) in
            bleQueue.async {
                if self.writeChar != nil { c.resume(); return }
                self.readyContinuation = c
                let t = DispatchSource.makeTimerSource(queue: self.bleQueue)
                t.schedule(deadline: .now() + timeout)
                t.setEventHandler { [weak self] in
                    guard let self, let cont = self.readyContinuation else { return }
                    self.readyContinuation = nil; cont.resume()
                }
                t.resume(); self.readyTimer = t
            }
        }
    }

    // MARK: - Request/response primitives (all dispatch onto bleQueue)

    /// Write `req` and collect frames until the link is quiet for `quiet` seconds.
    private func transact(_ req: Data, quiet: TimeInterval = 1.2) async -> [Packet] {
        await withCheckedContinuation { cont in
            bleQueue.async {
                var frames: [Packet] = []
                let id = UUID()
                var timer: DispatchSourceTimer?
                var finished = false
                func finish() {
                    if finished { return }; finished = true
                    self.listeners.removeValue(forKey: id); timer?.cancel()
                    cont.resume(returning: frames)
                }
                func arm() {
                    timer?.cancel()
                    let t = DispatchSource.makeTimerSource(queue: self.bleQueue)
                    t.schedule(deadline: .now() + quiet)
                    t.setEventHandler(handler: finish)
                    t.resume(); timer = t
                }
                self.listeners[id] = { data in
                    if let p = Packet.parse(data) { frames.append(p) }
                    arm()
                }
                self.writeRaw(req); arm()
            }
        }
    }

    /// Write `req`; return the first frame matching `tag` (+ `ext`), else nil after `timeout`.
    private func requestUntil(_ req: Data, tag: UInt8, ext: UInt8?, timeout: TimeInterval) async -> Packet? {
        await withCheckedContinuation { cont in
            bleQueue.async {
                let id = UUID()
                var done = false
                let timer = DispatchSource.makeTimerSource(queue: self.bleQueue)
                func finish(_ p: Packet?) {
                    if done { return }; done = true
                    self.listeners.removeValue(forKey: id); timer.cancel()
                    cont.resume(returning: p)
                }
                timer.schedule(deadline: .now() + timeout)
                timer.setEventHandler { finish(nil) }
                timer.resume()
                self.listeners[id] = { data in
                    guard let p = Packet.parse(data) else { return }
                    if p.tag == tag && (ext == nil || p.extTag == ext) { finish(p) }
                }
                self.writeRaw(req)
            }
        }
    }

    /// One `GetEvent` batch: collect event frames until the `0x11` summary or timeout.
    private func getEventBatch(start: UInt32, timeout: TimeInterval) async -> (events: [Packet], bytesLeft: UInt32, maxTs: UInt32) {
        await withCheckedContinuation { cont in
            bleQueue.async {
                var evs: [Packet] = []
                var maxTs = start
                var bytesLeft: UInt32 = 0
                let id = UUID()
                var done = false
                let timer = DispatchSource.makeTimerSource(queue: self.bleQueue)
                func finish() {
                    if done { return }; done = true
                    self.listeners.removeValue(forKey: id); timer.cancel()
                    cont.resume(returning: (evs, bytesLeft, maxTs))
                }
                timer.schedule(deadline: .now() + timeout)
                timer.setEventHandler(handler: finish)
                timer.resume()
                self.listeners[id] = { data in
                    guard let p = Packet.parse(data) else { return }
                    if p.tag == 0x11 {
                        if p.payload.count >= 6 {
                            bytesLeft = UInt32(p.payload[2]) | UInt32(p.payload[3]) << 8
                                      | UInt32(p.payload[4]) << 16 | UInt32(p.payload[5]) << 24
                        }
                        finish()
                    } else if p.tag >= HISTORY_EVENT_PREFIX {
                        evs.append(p)
                        if p.payload.count >= 4 {
                            let ts = UInt32(p.payload[0]) | UInt32(p.payload[1]) << 8
                                   | UInt32(p.payload[2]) << 16 | UInt32(p.payload[3]) << 24
                            maxTs = max(maxTs, ts)
                        }
                    }
                }
                self.writeRaw(Req.getEvent(start: start, maxEvents: 255, flags: -1))
            }
        }
    }

    /// Stream-safe incremental drain from `cursor`; calls `onEvent` per event.
    /// Returns the next cursor. Safe to run while the accelerometer streams.
    @discardableResult
    private func drainEventsLive(cursor: UInt32, onEvent: (DecodedEvent) -> Void) async -> UInt32 {
        var start = cursor
        for _ in 0..<10_000 {
            let batch = await getEventBatch(start: start, timeout: 1.5)
            for p in batch.events {
                guard p.payload.count >= 4 else { continue }
                let ts = UInt32(p.payload[0]) | UInt32(p.payload[1]) << 8
                       | UInt32(p.payload[2]) << 16 | UInt32(p.payload[3]) << 24
                let body = Data(p.payload[4...])
                let json = OuraCore.decodeEvent(tag: p.tag, body: body) ?? [:]
                onEvent(DecodedEvent(tag: p.tag, timestamp: ts, name: OuraCore.eventName(tag: p.tag), json: json))
            }
            let next = batch.maxTs &+ 1
            let progressed = !batch.events.isEmpty && next > start
            if progressed { start = next }
            if batch.bytesLeft == 0 || !progressed { break }
        }
        return start
    }

    // MARK: - High-level flows

    /// Pair a factory-reset ring exactly like the real app: connect, generate a
    /// random 16-byte key, install it with `SetAuthKey`, store it in the Keychain,
    /// then authenticate. No key typing — pairing *creates* the key.
    func pairNewRing() async {
        if writeChar == nil { connect(); await waitUntilReady() }
        publish { self.state = .authenticating; self.status = "Pairing…" }

        var keyBytes = [UInt8](repeating: 0, count: 16)
        guard SecRandomCopyBytes(kSecRandomDefault, 16, &keyBytes) == errSecSuccess else {
            publish { self.state = .failed("RNG error") }
            return
        }
        let key = Data(keyBytes)
        // Persist before installing, so a crash mid-pair never loses the only copy
        // of a key that may already be live on the ring.
        _ = KeyStore.saveHex(key.map { String(format: "%02x", $0) }.joined())

        guard let resp = await requestUntil(Req.setAuthKey(key), tag: 0x25, ext: nil, timeout: 3.0),
              resp.payload.first == 0x00 else {
            publish { self.state = .failed("SetAuthKey rejected"); self.status = "Pair failed — is the ring factory-reset?" }
            return
        }
        publish { self.status = "Key installed — authenticating…" }
        await authenticateAndLoad()
    }

    /// Connect (if needed) via the interactive scan flow, authenticate, then
    /// auto-sync if enabled. Used by the guide's Connect button.
    func authenticateAndLoad() async {
        if writeChar == nil {
            connect()
            await waitUntilReady()
        }
        guard writeChar != nil else {
            dbg("authenticateAndLoad: link never became ready")
            // Make sure the UI doesn't stay stuck in "Connecting…": surface failure.
            if case .failed = state {} else {
                failConnect("Couldn't connect", "Couldn't connect to the ring. Keep it close and make sure it isn't connected elsewhere, then try again.")
            }
            return
        }
        if await runAuth(), autoSync { await syncHistory() }
    }

    /// Authenticate over an already-established link, read metadata. Returns success.
    @discardableResult
    private func runAuth() async -> Bool {
        guard let key = KeyStore.keyBytes() else {
            publish { self.state = .failed("No auth key set"); self.status = "Set the ring key in Settings" }
            return false
        }
        publish { self.state = .authenticating; self.status = "Authenticating…" }
        dbg("requesting auth nonce")

        guard let noncePkt = await requestUntil(Req.authNonce, tag: 0x2f, ext: 0x2c, timeout: 2.0),
              noncePkt.payload.count > 1 else {
            dbg("no nonce response")
            publish { self.state = .failed("No nonce"); self.status = "Auth failed (no nonce)"; self.alert = "The ring didn't respond to the auth handshake. Try Connect again." }
            return false
        }
        dbg("nonce received (\(noncePkt.payload.count - 1) bytes)")
        let nonce = Data(noncePkt.payload[1...])
        guard let enc = OuraCore.encryptNonce(key: key, nonce: nonce) else {
            publish { self.state = .failed("Crypto error") }
            return false
        }
        let authPkt = await requestUntil(Req.authenticate(enc), tag: 0x2f, ext: 0x2e, timeout: 2.0)
        guard let authPkt, authPkt.payload.count > 1, authPkt.payload[1] == 0x00 else {
            let code = authPkt?.payload.count ?? 0 > 1 ? authPkt!.payload[1] : 0xff
            dbg("auth rejected (state byte \(code))")
            publish { self.state = .failed("Auth rejected"); self.status = "Auth rejected — wrong key? (\(code))"; self.alert = "The ring rejected the key (code \(code)). Make sure the imported key matches the one the ring was paired with." }
            return false
        }
        dbg("authenticated OK")
        publish { self.state = .ready; self.status = "Connected"; self.alert = nil }
        await readDeviceInfo()
        await readBattery()
        return true
    }

    // MARK: - Auto-reconnect (pending connect: connects whenever the ring wakes)

    /// Silently (re)connect to the known ring without scanning or a timeout — iOS
    /// completes the connection whenever the ring next becomes available (on its
    /// charger or worn). On link-up we authenticate and, if enabled, sync.
    func autoReconnect() {
        bleQueue.async {
            self.wantsAutoReconnect = true
            self.startAutoReconnectIfReady()
        }
    }

    private func startAutoReconnectIfReady() {
        guard wantsAutoReconnect, autoConnect, !connecting, writeChar == nil,
              central.state == .poweredOn, KeyStore.keyBytes() != nil
        else { return }
        connecting = true
        publish { if self.state != .ready { self.state = .connecting }; self.status = "Waiting for ring…" }
        if let idStr = UserDefaults.standard.string(forKey: "ringPeripheralID"),
           let uuid = UUID(uuidString: idStr),
           let p = central.retrievePeripherals(withIdentifiers: [uuid]).first {
            // Known ring: pending connect, no timeout — completes when it wakes.
            peripheral = p
            p.delegate = self
            dbg("auto-reconnect: pending connect to known ring")
            central.connect(p)
        } else {
            // First reconnect on this install: scan silently (no timeout, no modal,
            // no failure alert). When the ring advertises we connect + save its id.
            autoScanning = true
            dbg("auto-reconnect: scanning silently (no known ring id yet)")
            central.scanForPeripherals(withServices: [OuraGATT.service])
        }
    }

    func readDeviceInfo() async {
        let pkts = await transact(Req.firmware, quiet: 1.0)
        if let p = pkts.first(where: { $0.tag == 0x09 }), p.payload.count >= 18 {
            let fw = "\(p.payload[3]).\(p.payload[4]).\(p.payload[5])"
            publish { self.firmware = fw }
        }
        if let s = await productString(Req.serial) { publish { self.serial = s } }
        if let h = await productString(Req.hardware) { publish { self.hardware = h } }
    }

    private func productString(_ req: Data) async -> String? {
        let pkts = await transact(req, quiet: 0.8)
        guard let p = pkts.first(where: { $0.tag == 0x19 }), p.payload.first == 0 else { return nil }
        let bytes = Array(p.payload[1...]).prefix { $0 != 0 }
        return String(bytes: bytes, encoding: .utf8)
    }

    func readBattery() async {
        if let p = await requestUntil(Req.battery, tag: 0x0d, ext: nil, timeout: 1.0), p.payload.count >= 3 {
            let pct = Int(p.payload[0]); let chg = p.payload[1] > 0
            publish { self.batteryPercent = pct; self.charging = chg }
        }
    }

    // MARK: - Live mode (HR / HRV / motion / battery)

    func startLive() {
        dbg("startLive tapped (state ready=\(state == .ready), already live=\(liveActive))")
        guard state == .ready, !liveActive else { return }
        publish { self.liveActive = true; self.status = "Live — measuring…" }
        liveTask = Task { await self.runLive() }
    }

    func stopLive() {
        liveTask?.cancel(); liveTask = nil
        bleQueue.async {
            if let id = self.liveListenerID { self.listeners.removeValue(forKey: id); self.liveListenerID = nil }
            self.ibiBuffer.removeAll()
            self.writeRaw(Req.realtimeOff)
            self.writeRaw(Req.setFeatureMode(Feature.daytimeHR, FeatureMode.automatic))
        }
        publish { self.liveActive = false; self.status = "Connected"; self.motionG = nil }
    }

    private func runLive() async {
        dbg("live: enabling notifications + daytime-HR CONNECTED_LIVE")
        await sendAndWait(Req.setNotification(0x3f))
        await sendAndWait(Req.setFeatureMode(Feature.daytimeHR, FeatureMode.connectedLive))
        // Confirm the ring actually entered the measuring state (green LED on).
        if let st = await feature_status(Feature.daytimeHR) {
            dbg("live: daytime-HR mode=\(st.mode) state=\(st.state) status=\(st.status) (state 2 = measuring)")
        } else {
            dbg("live: no feature-status response")
        }
        var cursor = await drainEventsLive(cursor: 0) { _ in }   // baseline
        dbg("live: baseline cursor=\(cursor)")
        installLiveACMListener()
        await sendAndWait(Req.setRealtime(bitmask: Realtime.acm, minutes: 5, delay: 0))
        dbg("live: ACM armed; entering poll loop")

        var tick = 0
        while !Task.isCancelled {
            var n = 0, hr80 = 0
            cursor = await drainEventsLive(cursor: cursor) { [weak self] ev in
                n += 1; if ev.tag == 0x80 { hr80 += 1 }
                self?.handleLiveEvent(ev)
            }
            dbg("live: tick \(tick) drained \(n) events (\(hr80)×0x80) cursor=\(cursor)")
            if tick % 6 == 0 { await readBattery() }
            tick += 1
            try? await Task.sleep(nanoseconds: 2_000_000_000)
        }
    }

    /// Read a feature's status (mode/state/status) — used to confirm measuring.
    private func feature_status(_ f: UInt8) async -> (mode: UInt8, status: UInt8, state: UInt8)? {
        guard let p = await requestUntil(Req.featureStatus(f), tag: 0x2f, ext: 0x21, timeout: 1.0),
              p.payload.count >= 6 else { return nil }
        return (p.payload[2], p.payload[3], p.payload[4])
    }

    private func sendAndWait(_ req: Data) async {
        bleQueue.async { self.writeRaw(req) }
        try? await Task.sleep(nanoseconds: 120_000_000)
    }

    private func installLiveACMListener() {
        bleQueue.async {
            let id = UUID()
            self.liveListenerID = id
            self.listeners[id] = { [weak self] data in self?.handleACMFrame(data) }
        }
    }

    private func handleACMFrame(_ data: Data) {
        let b = [UInt8](data)
        guard b.count >= 10, b[0] == Realtime.acmResponseTag else { return }
        func s(_ o: Int) -> Int16 { Int16(bitPattern: UInt16(b[o]) | UInt16(b[o + 1]) << 8) }
        let x = Double(s(4)), y = Double(s(6)), z = Double(s(8))
        let g = (x * x + y * y + z * z).squareRoot() / 1024.0
        restWin += 1
        if abs(g - 1.0) > 0.06 { restMoves += 1 }
        let rest = restWin > 0 ? Int(Double(restMoves) / Double(restWin) * 100) : 0
        publish {
            self.motionG = g
            self.motionSeries.append(g); if self.motionSeries.count > 300 { self.motionSeries.removeFirst() }
            self.restlessness = rest
        }
        if restWin > 50 { restWin = 0; restMoves = 0 }
    }

    private func handleLiveEvent(_ ev: DecodedEvent) {
        guard ev.tag == 0x80 else { return }
        // bpm: last plausible value in hr_bpm
        if let arr = ev.json["hr_bpm"] as? [Any] {
            let bpms = arr.compactMap { ($0 as? NSNumber)?.intValue }.filter { $0 > 30 && $0 < 240 }
            if let bpm = bpms.last {
                publish {
                    self.liveHR = bpm
                    self.hrSeries.append(Double(bpm)); if self.hrSeries.count > 240 { self.hrSeries.removeFirst() }
                }
            }
        }
        // HRV (RMSSD) from physiologically plausible IBIs
        if let arr = ev.json["ibi_ms"] as? [Any] {
            for v in arr.compactMap({ ($0 as? NSNumber)?.doubleValue }) where v >= 400 && v <= 1300 {
                ibiBuffer.append(v)
            }
            while ibiBuffer.count > 40 { ibiBuffer.removeFirst() }
            if let hrv = Self.rmssd(ibiBuffer) {
                publish {
                    self.liveHRV = hrv
                    self.hrvSeries.append(Double(hrv)); if self.hrvSeries.count > 120 { self.hrvSeries.removeFirst() }
                }
            }
        }
    }

    static func rmssd(_ ibi: [Double]) -> Int? {
        guard ibi.count >= 3 else { return nil }
        var s = 0.0, n = 0
        for i in 1..<ibi.count {
            let d = ibi[i] - ibi[i - 1]
            if abs(d) > 250 { continue }
            s += d * d; n += 1
        }
        return n >= 2 ? Int((s / Double(n)).squareRoot().rounded()) : nil
    }

    // MARK: - Full history sync (Today/Sleep/Activity tabs)

    func syncHistory() async {
        guard state == .ready, !syncing else { return }
        publish { self.syncing = true; self.status = "Syncing history…" }
        var collected: [DecodedEvent] = []
        _ = await drainEventsLive(cursor: 0) { ev in collected.append(ev) }
        let model = HealthData(events: collected)
        publish {
            self.events = collected
            self.health = model
            self.syncing = false
            self.status = "Synced \(collected.count) events"
        }
    }
}

// MARK: - CoreBluetooth delegates
extension OuraRing: CBCentralManagerDelegate, CBPeripheralDelegate {
    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        dbg("central state -> \(central.state.rawValue)")
        startScanIfReady()           // resume a pending manual scan
        startAutoReconnectIfReady()  // resume a pending auto-reconnect
    }

    func centralManager(_ central: CBCentralManager, didDiscover peripheral: CBPeripheral,
                        advertisementData: [String: Any], rssi RSSI: NSNumber) {
        dbg("discovered \(peripheral.name ?? "ring") rssi \(RSSI)")
        central.stopScan()
        scanTimer?.cancel()
        self.peripheral = peripheral
        peripheral.delegate = self
        publish { if self.state != .ready { self.state = .connecting }; self.status = "Connecting…" }
        central.connect(peripheral)
        if autoScanning {
            // Silent background reconnect — no timeout, no failure alert.
            autoScanning = false
            dbg("auto-reconnect: connecting (no timeout)")
            return
        }
        // Manual scan path: CoreBluetooth connect() has no timeout of its own — bound
        // it so a ring held by another central (e.g. a paired Mac) can't hang us.
        connectTimer?.cancel()
        let t = DispatchSource.makeTimerSource(queue: bleQueue)
        t.schedule(deadline: .now() + 30)
        t.setEventHandler { [weak self] in
            guard let self, self.writeChar == nil else { return }
            let st = self.peripheral?.state.rawValue ?? -1
            self.dbg("connect timeout (peripheral.state=\(st); 0=disc 1=connecting 2=connected)")
            self.failConnect("Couldn't connect", "Couldn't connect to the ring. Wear it (or put it on the charger) and keep the phone right next to it, then try again.")
        }
        t.resume(); connectTimer = t
    }

    func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        dbg("connected — discovering services")
        // Remember this ring so future launches can silently auto-reconnect.
        UserDefaults.standard.set(peripheral.identifier.uuidString, forKey: "ringPeripheralID")
        peripheral.discoverServices([OuraGATT.service])
    }

    func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral, error: Error?) {
        failConnect("Connect failed", "Connect failed: \(error?.localizedDescription ?? "unknown")")
    }

    func centralManager(_ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral, error: Error?) {
        dbg("disconnected\(error.map { ": \($0.localizedDescription)" } ?? "")")
        self.writeChar = nil
        self.connecting = false
        self.liveActive = false
        // Unless the user asked to disconnect, immediately re-arm a pending connect
        // so we silently reconnect when the ring next wakes (charger/worn).
        if !userDisconnecting, autoConnect {
            wantsAutoReconnect = true
            publish { self.state = .connecting; self.status = "Waiting for ring…" }
            startAutoReconnectIfReady()
        } else {
            publish { if self.state != .idle { self.state = .idle }; self.status = "Disconnected" }
        }
        userDisconnecting = false
    }

    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        if let error { dbg("discoverServices error: \(error.localizedDescription)") }
        guard let svc = peripheral.services?.first(where: { $0.uuid == OuraGATT.service }) else {
            dbg("Oura service not found among \(peripheral.services?.count ?? 0) services")
            return
        }
        dbg("service found — discovering characteristics")
        peripheral.discoverCharacteristics(nil, for: svc)
    }

    func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        for c in service.characteristics ?? [] {
            if c.uuid == OuraGATT.write { writeChar = c }
            if c.properties.contains(.notify) || c.properties.contains(.indicate) {
                peripheral.setNotifyValue(true, for: c)
            }
        }
        // Link is usable once the write characteristic is known.
        if writeChar != nil {
            dbg("link ready (write characteristic discovered)")
            connecting = false
            scanTimer?.cancel(); connectTimer?.cancel(); readyTimer?.cancel()
            if let c = readyContinuation {
                // A manual authenticateAndLoad() is awaiting — let it drive auth.
                readyContinuation = nil; c.resume()
            } else {
                // Auto-reconnect path: authenticate (and sync) ourselves.
                Task { if await self.runAuth(), self.autoSync { await self.syncHistory() } }
            }
        } else {
            dbg("write characteristic not found in service")
        }
    }

    func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        guard let data = characteristic.value else { return }
        for listener in listeners.values { listener(data) }
    }
}
