import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var ring: OuraRing
    @State private var keyHex: String = KeyStore.loadHex() ?? ""
    @State private var saveMsg: String = ""

    var body: some View {
        NavigationStack {
            Form {
                Section("Connection") {
                    HStack {
                        Text("Status"); Spacer()
                        Text(ring.status).foregroundColor(.secondary)
                    }
                    let inProgress = [ConnState.scanning, .connecting, .authenticating].contains(ring.state)
                    Button {
                        ring.showConnectGuide = true
                    } label: {
                        Label("Connection guide", systemImage: "wave.3.right.circle")
                    }
                    if ring.state == .ready {
                        Button("Disconnect", role: .destructive) { ring.disconnect() }
                    } else {
                        let paired = KeyStore.keyBytes() != nil
                        if paired {
                            Button(inProgress ? "Connecting…" : "Connect") {
                                Task { await ring.authenticateAndLoad() }
                            }
                            .disabled(inProgress)
                        } else {
                            Button(inProgress ? "Pairing…" : "Pair ring") {
                                Task { await ring.pairNewRing() }
                            }
                            .disabled(inProgress)
                            Text("Generates a key and installs it on the ring — like the Oura app's setup. The ring must be factory-reset.")
                                .font(.footnote).foregroundColor(.secondary)
                        }
                    }
                }

                Section("Advanced — import existing key") {
                    Text("Already paired this ring elsewhere (e.g. the CLI's `oura pair`)? Paste that 16-byte key (32 hex chars) instead of re-pairing.")
                        .font(.footnote).foregroundColor(.secondary)
                    TextField("e.g. 4431967d8bacc2659743142b68391d9a", text: $keyHex)
                        .font(.system(.footnote, design: .monospaced))
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Button("Import key") {
                        if KeyStore.dataFromHex(keyHex) != nil, KeyStore.saveHex(keyHex) {
                            saveMsg = "Saved ✓ — tap Connect"
                        } else {
                            saveMsg = "Invalid — need exactly 32 hex chars"
                        }
                    }
                    if !saveMsg.isEmpty { Text(saveMsg).font(.footnote).foregroundColor(.secondary) }
                }

                Section("Automatic") {
                    Toggle("Auto-reconnect", isOn: $ring.autoConnect)
                    Toggle("Auto-sync on connect", isOn: $ring.autoSync)
                    Text("Reconnects to your ring whenever it wakes (on the charger or worn) and pulls new history — no manual connecting.")
                        .font(.footnote).foregroundColor(.secondary)
                }

                Section("Device") {
                    LabeledContent("Serial", value: ring.serial ?? "—")
                    LabeledContent("Hardware", value: ring.hardware ?? "—")
                    LabeledContent("Firmware", value: ring.firmware ?? "—")
                    LabeledContent("Battery", value: ring.batteryPercent.map { "\($0)%" } ?? "—")
                }

                Section("About") {
                    Text("Open Oura — a cloud-free client that reads your ring directly over Bluetooth. HR, HRV, motion, temperature and on-device sleep stages, no Oura account.")
                        .font(.footnote).foregroundColor(.secondary)
                }
            }
            .navigationTitle("Settings")
        }
    }
}
