import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var ring: OuraRing
    @State private var keyHex: String = KeyStore.loadHex() ?? ""
    @State private var saveMsg: String = ""
    @State private var showResetConfirm = false

    private var inProgress: Bool {
        [ConnState.scanning, .connecting, .authenticating].contains(ring.state)
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    HStack(spacing: 12) {
                        RingLogo().frame(width: 40, height: 40)
                        VStack(alignment: .leading, spacing: 1) {
                            Text("Open Oura").font(.headline)
                            Text(ring.status).font(.caption).foregroundStyle(Brand.dim)
                        }
                    }
                }
                .listRowBackground(Color.clear)

                Section("Connection") {
                    Button {
                        ring.showConnectGuide = true
                    } label: { Label("Connection guide", systemImage: "wave.3.right.circle") }

                    if ring.state == .ready {
                        Button("Disconnect", role: .destructive) { ring.disconnect() }
                    } else if KeyStore.keyBytes() != nil {
                        Button(inProgress ? "Connecting…" : "Connect") { Task { await ring.authenticateAndLoad() } }
                            .disabled(inProgress)
                    } else {
                        Button(inProgress ? "Pairing…" : "Pair ring") { Task { await ring.pairNewRing() } }
                            .disabled(inProgress)
                    }
                }

                Section("Automatic") {
                    Toggle("Auto-reconnect", isOn: $ring.autoConnect)
                    Toggle("Auto-sync on connect", isOn: $ring.autoSync)
                    Text("Reconnects whenever the ring wakes (charger or worn) and pulls new history — no manual connecting.")
                        .font(.footnote).foregroundStyle(Brand.dim)
                }

                Section("Device") {
                    LabeledContent("Serial", value: ring.serial ?? "—")
                    LabeledContent("Hardware", value: ring.hardware ?? "—")
                    LabeledContent("Firmware", value: ring.firmware ?? "—")
                    LabeledContent("Battery", value: ring.batteryPercent.map { "\($0)%" } ?? "—")
                    if let d = ring.lastSync { LabeledContent("Last sync", value: relativeTime(d)) }
                }

                Section("Advanced — import existing key") {
                    Text("Already paired this ring elsewhere (e.g. the CLI's `oura pair`)? Paste that 16-byte key (32 hex chars).")
                        .font(.footnote).foregroundStyle(Brand.dim)
                    TextField("e.g. 4431967d8bacc2659743142b68391d9a", text: $keyHex)
                        .font(.system(.footnote, design: .monospaced))
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                    Button("Import key") {
                        if KeyStore.dataFromHex(keyHex) != nil, KeyStore.saveHex(keyHex) {
                            saveMsg = "Saved ✓ — tap Connect"
                        } else { saveMsg = "Invalid — need exactly 32 hex chars" }
                    }
                    if !saveMsg.isEmpty { Text(saveMsg).font(.footnote).foregroundStyle(Brand.dim) }
                }

                Section("Danger zone") {
                    Button(role: .destructive) {
                        showResetConfirm = true
                    } label: { Label("Factory-reset ring", systemImage: "exclamationmark.triangle.fill") }
                        .disabled(ring.state != .ready)
                    Text("Wipes the ring's auth key and on-device data so you can pair fresh. You'll set a new key by pairing again.")
                        .font(.footnote).foregroundStyle(Brand.dim)
                }

                Section {
                    Text("A cloud-free client that reads your Oura ring directly over Bluetooth — heart rate, HRV, motion, temperature, and on-device sleep stages, with no Oura account.")
                        .font(.footnote).foregroundStyle(Brand.dim)
                } header: { Text("About") }
            }
            .scrollContentBackground(.hidden)
            .background(Brand.bg.ignoresSafeArea())
            .navigationTitle("Settings")
            .confirmationDialog("Factory-reset the ring?", isPresented: $showResetConfirm, titleVisibility: .visible) {
                Button("Factory reset", role: .destructive) { Task { await ring.factoryReset() } }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("This wipes the ring's auth key and data. The ring will need to be paired again.")
            }
            .onChange(of: ring.state) { _ in keyHex = KeyStore.loadHex() ?? "" }
        }
    }
}
